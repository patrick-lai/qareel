use crate::failure::{fail, fixable};
use anyhow::{Result, anyhow};
use qareel_protocol::{NativeBrowserHostMessage, NativeBrowserMessage, NativeBrowserOperation, NativeBrowserOutcome, NativeCapabilities, NativeCapability, NativeImplementation, NativePointerPhase, NativePopupIdentity, NativePresentation, NativeTabState};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot};

pub const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 256 * 1024;
const MAX_PENDING: usize = 64;
const RESTART_LIMIT: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(300);

pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, OsString)>,
    pub bootstrap: Value,
    pub log: PathBuf,
}

#[derive(Clone, Debug)]
pub enum PopupEvent {
    Opened { tab: String, identity: NativePopupIdentity, url: String },
    Closed { tab: String, identity: NativePopupIdentity },
}

#[derive(Clone, Default)]
pub struct Host {
    inner: Arc<Mutex<Inner>>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
}

#[derive(Default)]
struct Inner {
    connection: Option<Connection>,
    pending: HashMap<String, Pending>,
    tabs: HashMap<String, NativeTabState>,
    attempts: VecDeque<Instant>,
    exit: Option<String>,
    popups: VecDeque<PopupEvent>,
}

struct Connection {
    generation: String,
    sequence: u64,
    capabilities: NativeCapabilities,
    outgoing: mpsc::Sender<String>,
}

struct Pending {
    reply: oneshot::Sender<Result<Value>>,
    generation: String,
}

struct PendingGuard<'a> {
    host: &'a Host,
    id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.host.locked().pending.remove(&self.id);
    }
}

pub fn operation_capability(operation: &NativeBrowserOperation) -> NativeCapability {
    match operation {
        NativeBrowserOperation::Ensure { .. } | NativeBrowserOperation::Navigate { .. } | NativeBrowserOperation::Back | NativeBrowserOperation::Forward | NativeBrowserOperation::Reload | NativeBrowserOperation::Close | NativeBrowserOperation::Control { .. } | NativeBrowserOperation::Resize { .. } => NativeCapability::Core,
        NativeBrowserOperation::Snapshot => NativeCapability::Snapshot,
        NativeBrowserOperation::Evaluate { .. } | NativeBrowserOperation::Hold { .. } => NativeCapability::Evaluate,
        NativeBrowserOperation::Click { .. } | NativeBrowserOperation::Type { .. } | NativeBrowserOperation::Press { .. } => NativeCapability::ReferenceInput,
        NativeBrowserOperation::Pointer { phase: NativePointerPhase::Move, .. } => NativeCapability::PointerTap,
        NativeBrowserOperation::Pointer { .. } => NativeCapability::PointerInput,
        NativeBrowserOperation::Tap { .. } => NativeCapability::PointerTap,
        NativeBrowserOperation::Key { .. } => NativeCapability::KeyInput,
        NativeBrowserOperation::Wheel { .. } => NativeCapability::WheelInput,
        NativeBrowserOperation::Frames | NativeBrowserOperation::FrameEvaluate { .. } => NativeCapability::Frames,
        NativeBrowserOperation::Dialog { .. } => NativeCapability::Dialog,
        NativeBrowserOperation::CookiesExport { .. } | NativeBrowserOperation::CookiesApply { .. } => NativeCapability::Cookies,
        NativeBrowserOperation::Screenshot => NativeCapability::Screenshot,
        NativeBrowserOperation::AutomationObserve { .. } | NativeBrowserOperation::AutomationExecute { .. } => NativeCapability::Automation,
        NativeBrowserOperation::Console { .. } => NativeCapability::Console,
        NativeBrowserOperation::PopupDecision { .. } => NativeCapability::Popups,
        NativeBrowserOperation::RecordingStart { .. } | NativeBrowserOperation::RecordingCaption { .. } | NativeBrowserOperation::RecordingStop { .. } | NativeBrowserOperation::RecordingStatus { .. } | NativeBrowserOperation::RecordingRead { .. } | NativeBrowserOperation::RecordingRelease { .. } => NativeCapability::RecordingVideo,
    }
}

pub fn valid_capabilities(capabilities: &NativeCapabilities) -> bool {
    let operations = &capabilities.operations;
    let unique: HashSet<_> = operations.iter().collect();
    capabilities.version == 1
        && operations.len() <= 24
        && unique.len() == operations.len()
        && operations.contains(&NativeCapability::Core)
        && (!operations.contains(&NativeCapability::RecordingAudioApp) || operations.contains(&NativeCapability::RecordingVideo))
        && matches!(capabilities.implementation, NativeImplementation::AppkitWebkit | NativeImplementation::WpeWebkit)
        && capabilities.presentation == NativePresentation::None
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as u64).unwrap_or_default()
}

fn log_tail(log: &PathBuf) -> Option<String> {
    let text = std::fs::read(log).ok()?;
    let tail = String::from_utf8_lossy(&text[text.len().saturating_sub(4096)..]).to_string();
    tail.lines().rev().find(|line| line.contains("browser.")).map(|line| line.trim().chars().take(400).collect())
}

impl Host {
    fn locked(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn connected(&self) -> bool {
        self.locked().connection.is_some()
    }

    pub fn generation(&self) -> Option<String> {
        self.locked().connection.as_ref().map(|connection| connection.generation.clone())
    }

    pub fn advertises(&self, capability: NativeCapability) -> bool {
        self.locked().connection.as_ref().is_some_and(|connection| connection.capabilities.operations.contains(&capability))
    }

    pub fn implementation(&self) -> Option<NativeImplementation> {
        self.locked().connection.as_ref().map(|connection| connection.capabilities.implementation)
    }

    pub fn tab_state(&self, tab: &str) -> Option<NativeTabState> {
        self.locked().tabs.get(tab).cloned()
    }

    pub fn clear_tab_state(&self, tab: &str) {
        self.locked().tabs.remove(tab);
    }

    pub fn take_popups(&self) -> Vec<PopupEvent> {
        self.locked().popups.drain(..).collect()
    }

    pub fn require(&self, capability: NativeCapability) -> Result<()> {
        match self.locked().connection.as_ref() {
            Some(connection) if connection.capabilities.operations.contains(&capability) => Ok(()),
            Some(_) => Err(fail("browser.unsupported", format!("this browser engine does not support {capability:?}"))),
            None => Err(fail("browser.unavailable", "the browser engine is not running")),
        }
    }

    fn consume_attempt(&self) -> Result<()> {
        let mut inner = self.locked();
        let now = Instant::now();
        while inner.attempts.front().is_some_and(|started| now.duration_since(*started) >= RESTART_WINDOW) {
            inner.attempts.pop_front();
        }
        if inner.attempts.len() >= RESTART_LIMIT {
            let cause = inner.exit.clone().map(|exit| format!(" (last exit: {exit})")).unwrap_or_default();
            return Err(fixable("browser.restart_limit", format!("the browser engine stopped {RESTART_LIMIT} times within five minutes{cause}"), "read ~/.qareel/serve.log, then run `qareel stop` and try again"));
        }
        inner.attempts.push_back(now);
        Ok(())
    }

    pub async fn start(&self, launch: Launch) -> Result<()> {
        if self.connected() {
            return Ok(());
        }
        self.consume_attempt()?;
        self.stop().await;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&launch.log)?;
        let mut command = tokio::process::Command::new(&launch.program);
        command.args(&launch.args).env_clear().envs(launch.env.iter().map(|(key, value)| (key, value))).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(log)).kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| fixable("browser.host_start", format!("the browser engine could not start: {error}"), "reinstall with `npx @patrick-lai/qareel@latest`, or set QAREEL_HOST to a working engine"))?;
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(fail("browser.host_start", "the browser engine has no stdio pipes"));
        };
        *self.child.lock().await = Some(child);
        let generation = uuid::Uuid::new_v4().to_string();
        let mut bootstrap = serde_json::to_vec(&launch.bootstrap)?;
        bootstrap.push(b'\n');
        let hello = serde_json::to_string(&NativeBrowserMessage::Hello { generation: generation.clone(), features: vec!["ready-capabilities".to_owned()] })?;
        let delivered = tokio::time::timeout(READY_TIMEOUT, async {
            stdin.write_all(&bootstrap).await?;
            stdin.write_all(hello.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        }).await;
        if !matches!(delivered, Ok(Ok(()))) {
            self.stop().await;
            return Err(self.start_failure(&launch.log, "the browser engine did not accept its startup message"));
        }
        let (outgoing, receiver) = mpsc::channel::<String>(MAX_PENDING);
        let (ready, readied) = oneshot::channel();
        tokio::spawn(write_loop(stdin, receiver));
        tokio::spawn(self.clone().read_loop(stdout, generation.clone(), outgoing.downgrade(), ready));
        let capabilities = match tokio::time::timeout(READY_TIMEOUT, readied).await {
            Ok(Ok(Ok(capabilities))) => capabilities,
            Ok(Ok(Err(error))) => {
                drop(outgoing);
                self.stop().await;
                return Err(error);
            }
            _ => {
                drop(outgoing);
                self.stop().await;
                return Err(self.start_failure(&launch.log, "the browser engine did not become ready in time"));
            }
        };
        self.locked().connection = Some(Connection { generation, sequence: 0, capabilities, outgoing });
        Ok(())
    }

    fn start_failure(&self, log: &PathBuf, message: &str) -> anyhow::Error {
        match log_tail(log) {
            Some(cause) => fixable("browser.host_start", format!("{message}: {cause}"), "fix the cause above, then retry; the full log is ~/.qareel/serve.log"),
            None => fixable("browser.host_start", message.to_owned(), "read ~/.qareel/serve.log for the engine's output, then retry"),
        }
    }

    async fn read_loop(self, stdout: tokio::process::ChildStdout, generation: String, outgoing: mpsc::WeakSender<String>, ready: oneshot::Sender<Result<NativeCapabilities>>) {
        let mut ready = Some(ready);
        let mut reader = BufReader::new(stdout);
        let mut frame = Vec::new();
        let ended = loop {
            frame.clear();
            let read = (&mut reader).take(MAX_MESSAGE_BYTES as u64 + 1).read_until(b'\n', &mut frame).await;
            match read {
                Ok(0) => break "the browser engine exited".to_owned(),
                Ok(_) if frame.len() > MAX_MESSAGE_BYTES => break "the browser engine sent an oversized message".to_owned(),
                Ok(_) => {}
                Err(error) => break format!("the browser engine output failed: {error}"),
            }
            let Ok(message) = serde_json::from_slice::<NativeBrowserHostMessage>(&frame) else {
                break "the browser engine sent an invalid message".to_owned();
            };
            match message {
                NativeBrowserHostMessage::Ready { generation: received, capabilities, .. } if received == generation => {
                    let Some(ready) = ready.take() else { continue };
                    let outcome = match capabilities {
                        Some(capabilities) if valid_capabilities(&capabilities) => Ok(capabilities),
                        _ => Err(fixable("browser.incompatible", "the browser engine reported capabilities this qareel does not understand", "reinstall qareel so the CLI and engine versions match")),
                    };
                    let _ = ready.send(outcome);
                }
                NativeBrowserHostMessage::Reply { generation: received, id, outcome } if received == generation => {
                    let pending = self.locked().pending.remove(&id);
                    if let Some(pending) = pending {
                        let result = match outcome {
                            NativeBrowserOutcome::Ok { value } => Ok(value),
                            NativeBrowserOutcome::Error { message } => Err(anyhow!("{}", message.chars().take(1200).collect::<String>())),
                        };
                        let _ = pending.reply.send(result);
                    }
                }
                NativeBrowserHostMessage::Tab { generation: received, tab_id, state } if received == generation && tab_id.len() <= 256 => {
                    self.locked().tabs.insert(tab_id, state);
                }
                NativeBrowserHostMessage::Popup { generation: received, instance_id, sequence, opener_id, tab_id, popup_id, state } if received == generation && tab_id.len() <= 256 => {
                    let mut inner = self.locked();
                    if inner.popups.len() < 64 && !inner.popups.iter().any(|event| matches!(event, PopupEvent::Opened { tab, .. } if *tab == tab_id)) {
                        inner.popups.push_back(PopupEvent::Opened { tab: tab_id, identity: NativePopupIdentity { instance_id, sequence, opener_id, popup_id }, url: state.url });
                    }
                }
                NativeBrowserHostMessage::PopupClosed { generation: received, instance_id, sequence, opener_id, tab_id, popup_id } if received == generation && tab_id.len() <= 256 => {
                    let mut inner = self.locked();
                    if inner.popups.len() < 64 && !inner.popups.iter().any(|event| matches!(event, PopupEvent::Closed { tab, .. } if *tab == tab_id)) {
                        inner.popups.push_back(PopupEvent::Closed { tab: tab_id, identity: NativePopupIdentity { instance_id, sequence, opener_id, popup_id } });
                    }
                }
                NativeBrowserHostMessage::Flush { generation: received, id } if received == generation => {
                    let Ok(reply) = serde_json::to_string(&NativeBrowserMessage::Flushed { generation: generation.clone(), id }) else { break "the flush reply could not be encoded".to_owned() };
                    let Some(sender) = outgoing.upgrade() else { break "the browser engine input closed".to_owned() };
                    if sender.send(reply).await.is_err() {
                        break "the browser engine input closed".to_owned();
                    }
                }
                _ => {}
            }
        };
        if let Some(ready) = ready.take() {
            let _ = ready.send(Err(fail("browser.host_start", ended.clone())));
        }
        self.disconnect(&generation, &ended);
        let exit = {
            let mut child = self.child.lock().await;
            match child.as_mut() {
                Some(process) => match tokio::time::timeout(STOP_TIMEOUT, process.wait()).await {
                    Ok(Ok(status)) => { *child = None; Some(status.to_string()) }
                    _ => None,
                },
                None => None,
            }
        };
        if let Some(exit) = exit {
            self.locked().exit = Some(exit);
        }
    }

    fn disconnect(&self, generation: &str, reason: &str) {
        let mut inner = self.locked();
        if inner.connection.as_ref().is_some_and(|connection| connection.generation == generation) {
            inner.connection = None;
            inner.tabs.clear();
            inner.popups.clear();
        }
        let failed: Vec<String> = inner.pending.iter().filter(|(_, pending)| pending.generation == generation).map(|(id, _)| id.clone()).collect();
        for id in failed {
            if let Some(pending) = inner.pending.remove(&id) {
                let _ = pending.reply.send(Err(fixable("browser.host_exited", format!("{reason}; the outcome of the last command is unknown"), "run the command again; qareel restarts the engine")));
            }
        }
    }

    pub async fn stop(&self) {
        let connection = self.locked().connection.take();
        if let Some(connection) = &connection {
            self.disconnect(&connection.generation, "the browser engine was stopped");
        }
        drop(connection);
        let mut child = self.child.lock().await;
        if let Some(process) = child.as_mut()
            && !matches!(tokio::time::timeout(STOP_TIMEOUT, process.wait()).await, Ok(Ok(_)))
        {
            let _ = process.kill().await;
        }
        *child = None;
    }

    pub async fn call(&self, tab: &str, operation: NativeBrowserOperation, timeout: Duration) -> Result<Value> {
        if tab.is_empty() || tab.len() > 128 {
            return Err(fail("browser.invalid_tab", "tab identifiers are 1 to 128 characters"));
        }
        self.require(operation_capability(&operation))?;
        let agent = !matches!(operation, NativeBrowserOperation::Ensure { .. } | NativeBrowserOperation::PopupDecision { .. } | NativeBrowserOperation::RecordingStop { .. } | NativeBrowserOperation::RecordingStatus { .. } | NativeBrowserOperation::RecordingRead { .. } | NativeBrowserOperation::RecordingRelease { .. });
        let (reply, result) = oneshot::channel();
        let id = {
            let mut inner = self.locked();
            if inner.pending.len() >= MAX_PENDING {
                return Err(fail("browser.busy", "too many browser commands are waiting"));
            }
            let connection = inner.connection.as_mut().ok_or_else(|| fail("browser.unavailable", "the browser engine is not running"))?;
            let sequence = connection.sequence + 1;
            let id = format!("{}:{sequence}", connection.generation);
            let generation = connection.generation.clone();
            let message = serde_json::to_string(&NativeBrowserMessage::Command { generation: generation.clone(), id: id.clone(), tab_id: tab.to_owned(), agent, control_epoch: 0, deadline_ms: now_ms().saturating_add(timeout.as_millis() as u64), operation })?;
            if message.len() > MAX_COMMAND_BYTES {
                return Err(fail("browser.too_large", "the browser command exceeds 256 KiB"));
            }
            connection.outgoing.try_send(message).map_err(|_| fail("browser.busy", "the browser engine input is full or closed"))?;
            connection.sequence = sequence;
            inner.pending.insert(id.clone(), Pending { reply, generation });
            id
        };
        let _guard = PendingGuard { host: self, id };
        match tokio::time::timeout(timeout + Duration::from_secs(2), result).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(fail("browser.host_exited", "the browser engine stopped; the outcome of the last command is unknown")),
            Err(_) => Err(fail("browser.timeout", "the browser engine did not answer in time; the command was not repeated")),
        }
    }
}

async fn write_loop(mut stdin: ChildStdin, mut receiver: mpsc::Receiver<String>) {
    while let Some(message) = receiver.recv().await {
        let written = tokio::time::timeout(CALL_TIMEOUT, async {
            stdin.write_all(message.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        }).await;
        if !matches!(written, Ok(Ok(()))) {
            break;
        }
    }
    let _ = stdin.shutdown().await;
}
