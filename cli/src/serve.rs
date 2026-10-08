use crate::args::{Spec, parse};
use crate::browser::Session;
use crate::failure::{describe, fixable};
use crate::paths::Layout;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
const IDLE_CHECK: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub version: String,
    pub command: String,
    pub params: Vec<String>,
    pub input: Option<String>,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { text: String },
    Error { code: String, message: String, fix: Option<String> },
}

struct Shared {
    session: tokio::sync::Mutex<Session>,
    busy: AtomicUsize,
    last: std::sync::Mutex<Instant>,
    shutdown: tokio::sync::Notify,
}

const RECORD: Spec = Spec { command: "record", positional: &["action", "text"], strings: &["action", "text", "caption", "caption_id", "recording_id", "audio"], values: &["fps", "max_duration_ms", "max_bytes", "max_dimension", "overlays"], pointing: false };

fn demo_spec(action: Option<&String>) -> Spec {
    let positional: &'static [&'static str] = match action.map(String::as_str) {
        Some("start") => &["action", "url"],
        Some("shot") => &["action", "shot"],
        Some("check") => &["action", "criterion", "outcome", "evidence"],
        Some("finish") => &["action", "out"],
        _ => &["action"],
    };
    Spec { command: "demo", positional, strings: &["action", "url", "criterion", "outcome", "evidence", "out", "by", "engine", "voice"], values: &["shot", "afk", "auto"], pointing: false }
}

fn idle_limit() -> Duration {
    let minutes = std::env::var("QAREEL_IDLE_MINUTES").ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(15).clamp(1, 24 * 60);
    Duration::from_secs(minutes * 60)
}

async fn dispatch(shared: &Shared, request: &Request) -> Result<String> {
    let mut session = shared.session.lock().await;
    match request.command.as_str() {
        "ping" => Ok(env!("CARGO_PKG_VERSION").to_owned()),
        "record" => session.record(&parse(&RECORD, &request.params)?).await,
        "demo" => {
            let spec = demo_spec(request.params.first());
            session.demo(&parse(&spec, &request.params)?, request.input.as_deref(), &request.cwd).await
        }
        command => {
            let spec = crate::browser::spec(command).ok_or_else(|| fixable("args.unknown_command", format!("`{command}` is not a qareel command"), "run `qareel --help`"))?;
            session.run(command, &parse(&spec, &request.params)?, &request.cwd).await
        }
    }
}

async fn respond(stream: &mut UnixStream, response: &Response) -> Result<()> {
    let mut line = serde_json::to_vec(response)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle(shared: Arc<Shared>, mut stream: UnixStream) -> Result<()> {
    let mut line = Vec::new();
    {
        let mut reader = BufReader::new((&mut stream).take(MAX_REQUEST_BYTES + 1));
        reader.read_until(b'\n', &mut line).await?;
    }
    let request: Request = match serde_json::from_slice(&line) {
        Ok(request) if line.len() as u64 <= MAX_REQUEST_BYTES => request,
        _ => return respond(&mut stream, &Response::Error { code: "serve.request_invalid".to_owned(), message: "the request could not be read".to_owned(), fix: None }).await,
    };
    if request.version != env!("CARGO_PKG_VERSION") {
        let recording = shared.session.lock().await.recording_active();
        if !recording {
            shared.shutdown.notify_one();
        }
        return respond(&mut stream, &Response::Error { code: "serve.version".to_owned(), message: format!("the running qareel session is version {}", env!("CARGO_PKG_VERSION")), fix: recording.then(|| "stop the current recording with the older qareel, then retry".to_owned()) }).await;
    }
    if request.command == "shutdown" {
        shared.shutdown.notify_one();
        return respond(&mut stream, &Response::Ok { text: "Stopping the qareel session.".to_owned() }).await;
    }
    shared.busy.fetch_add(1, Ordering::SeqCst);
    let outcome = dispatch(&shared, &request).await;
    if let Ok(mut last) = shared.last.lock() {
        *last = Instant::now();
    }
    shared.busy.fetch_sub(1, Ordering::SeqCst);
    let response = match outcome {
        Ok(text) => Response::Ok { text },
        Err(error) => {
            let failure = describe(&error);
            eprintln!("qareel serve: {} failed: {}", request.command, failure);
            Response::Error { code: failure.code, message: failure.message, fix: failure.fix }
        }
    };
    respond(&mut stream, &response).await
}

fn take_lock(path: &Path) -> Result<Option<std::fs::File>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    Ok(Some(file))
}

async fn idle(shared: &Shared, limit: Duration) -> bool {
    if shared.busy.load(Ordering::SeqCst) > 0 {
        return false;
    }
    let quiet = shared.last.lock().map(|last| last.elapsed() >= limit).unwrap_or(false);
    if !quiet {
        return false;
    }
    match shared.session.try_lock() {
        Ok(session) => !session.recording_active(),
        Err(_) => false,
    }
}

pub async fn run() -> Result<()> {
    let layout = Layout::current()?;
    layout.prepare()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let _lock = loop {
        match take_lock(&layout.lock)? {
            Some(lock) => break lock,
            None if Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(200)).await,
            None => return Ok(()),
        }
    };
    let _ = std::fs::remove_file(&layout.socket);
    let listener = UnixListener::bind(&layout.socket).with_context(|| format!("serve.bind: cannot listen on {}", layout.socket.display()))?;
    std::fs::set_permissions(&layout.socket, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("qareel serve {} listening (pid {})", env!("CARGO_PKG_VERSION"), std::process::id());
    let shared = Arc::new(Shared { session: tokio::sync::Mutex::new(Session::new(layout.clone())), busy: AtomicUsize::new(0), last: std::sync::Mutex::new(Instant::now()), shutdown: tokio::sync::Notify::new() });
    let limit = idle_limit();
    let mut tick = tokio::time::interval(IDLE_CHECK);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle(shared, stream).await {
                            eprintln!("qareel serve: a client connection failed: {error:#}");
                        }
                    });
                }
                Err(error) => eprintln!("qareel serve: accept failed: {error}"),
            },
            _ = tick.tick() => if idle(&shared, limit).await {
                eprintln!("qareel serve: idle for {} minutes; exiting", limit.as_secs() / 60);
                break;
            },
            _ = shared.shutdown.notified() => break,
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
    }
    drop(listener);
    let _ = std::fs::remove_file(&layout.socket);
    shared.session.lock().await.host.stop().await;
    eprintln!("qareel serve: stopped");
    Ok(())
}
