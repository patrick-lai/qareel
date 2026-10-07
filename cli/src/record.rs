use crate::browser::{MarkLog, Session};
use crate::failure::{fail, fixable};
use crate::paths::atomic_write;
use anyhow::{Context, Result};
use base64::Engine;
use qareel_protocol::{NativeBrowserOperation, NativeRecordingArtifactKind, NativeRecordingAudio, NativeRecordingAudioStatus, NativeRecordingChunk, NativeRecordingControlPolicy, NativeRecordingOptions, NativeRecordingOverlays, NativeRecordingPhase, NativeRecordingScope, NativeRecordingStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_BYTES: u64 = 100 * 1024 * 1024;
const CHUNK: u32 = 256 * 1024;
const STOP_WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Caption {
    pub caption_id: String,
    pub text: String,
    pub time_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedMark {
    pub time_ms: u64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub tab: String,
    pub url: String,
    pub options: NativeRecordingOptions,
    pub status: Option<NativeRecordingStatus>,
    pub stopping: bool,
    pub copied: bool,
    pub released: bool,
    pub pending_caption: Option<(String, String)>,
    pub captions: Vec<Caption>,
    pub marks: Vec<SavedMark>,
    pub viewport: (f64, f64),
}

impl Job {
    pub fn terminal(&self) -> bool {
        self.status.as_ref().is_some_and(|status| terminal(status.phase))
    }

    pub fn state(&self) -> &'static str {
        match (self.copied, self.status.as_ref().map(|status| status.phase)) {
            (true, Some(NativeRecordingPhase::Complete)) => "complete",
            (true, Some(NativeRecordingPhase::Failed)) => "failed",
            (true, _) => "interrupted",
            (false, Some(phase)) if terminal(phase) => "transferring",
            (false, _) if self.stopping => "stopping",
            (false, Some(NativeRecordingPhase::Recording)) => "recording",
            (false, Some(NativeRecordingPhase::Stopping)) => "stopping",
            _ => "starting",
        }
    }
}

fn terminal(phase: NativeRecordingPhase) -> bool {
    matches!(phase, NativeRecordingPhase::Complete | NativeRecordingPhase::Interrupted | NativeRecordingPhase::Failed)
}

pub fn artifact_name(kind: NativeRecordingArtifactKind) -> &'static str {
    match kind {
        NativeRecordingArtifactKind::Video => "recording.mp4",
        NativeRecordingArtifactKind::CaptionsSrt => "captions.srt",
        NativeRecordingArtifactKind::CaptionsVtt => "captions.vtt",
        NativeRecordingArtifactKind::Events => "events.json",
    }
}

pub fn canonical_id(value: &str) -> Result<()> {
    match uuid::Uuid::parse_str(value) {
        Ok(id) if id.to_string() == value => Ok(()),
        _ => Err(fail("record.invalid_id", "recording IDs are canonical lowercase UUIDs")),
    }
}

pub fn scope_for(url: &str) -> Result<NativeRecordingScope> {
    let parsed = url::Url::parse(url).map_err(|_| fixable("record.scope", "the current page has no web address to record", "open the page under test first with `qareel open URL`"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(fixable("record.scope", "recordings need an http or https page", "open the page under test first with `qareel open URL`"));
    }
    let host = parsed.host_str().unwrap_or_default();
    if host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]" {
        return Ok(NativeRecordingScope::Local);
    }
    Ok(NativeRecordingScope::Origins { origins: vec![parsed.origin().ascii_serialization()] })
}

pub fn validate_status(job: &Job, status: &NativeRecordingStatus) -> Result<()> {
    let invalid = |detail: &str| fail("record.invalid_reply", format!("the engine reported recording metadata that does not match the accepted job: {detail}"));
    if status.recording_id != job.id || status.duration_ms > u64::from(job.options.max_duration_ms) + 10_000 || status.frames > 10_000 || status.audio != job.options.audio || status.artifacts.len() > 4 || status.reason.as_ref().is_some_and(|reason| reason.len() > 2048) {
        return Err(invalid("identity or bounds"));
    }
    let done = terminal(status.phase);
    let audio = match status.audio {
        NativeRecordingAudio::Off => status.audio_status == NativeRecordingAudioStatus::Disabled && status.audio_gap_ms == 0,
        NativeRecordingAudio::App => match status.audio_status {
            NativeRecordingAudioStatus::Disabled => false,
            NativeRecordingAudioStatus::Pending => !done,
            NativeRecordingAudioStatus::Captured => true,
            NativeRecordingAudioStatus::Failed => matches!(status.phase, NativeRecordingPhase::Interrupted | NativeRecordingPhase::Failed),
        },
        NativeRecordingAudio::Microphone | NativeRecordingAudio::AppAndMicrophone => false,
    };
    if !audio || status.audio_gap_ms > status.duration_ms {
        return Err(invalid("audio state"));
    }
    let mut kinds = HashSet::new();
    for artifact in &status.artifacts {
        let bound = if artifact.kind == NativeRecordingArtifactKind::Video { job.options.max_bytes + 4 * 1024 * 1024 } else { 4 * 1024 * 1024 };
        if !kinds.insert(artifact.kind) || artifact.bytes > bound || artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
            return Err(invalid("artifact descriptor"));
        }
    }
    if status.phase == NativeRecordingPhase::Complete && (status.frames == 0 || status.duration_ms == 0 || !status.artifacts.iter().any(|item| item.kind == NativeRecordingArtifactKind::Video && item.bytes > 0)) {
        return Err(fail("record.empty", "the engine reported a completed recording with no usable video frames"));
    }
    Ok(())
}

fn file_hash(path: &Path) -> Result<(u64, String)> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0u8; 65536];
    let mut hash = Sha256::new();
    let mut size = 0;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        hash.update(&buffer[..count]);
    }
    Ok((size, crate::paths::hex(&hash.finalize())))
}

pub fn summary(job: &Job, directory: &Path) -> String {
    let status = job.status.as_ref();
    let mut text = format!("Recording {}: {}", job.id, job.state());
    if let Some(status) = status {
        text.push_str(&format!(", {} frames, {:.1}s", status.frames, status.duration_ms as f64 / 1000.0));
        if let Some(reason) = &status.reason {
            text.push_str(&format!("\nReason: {reason}"));
        }
    }
    if job.pending_caption.is_some() {
        text.push_str("\nA caption is waiting to be confirmed; `qareel record status` reconciles it.");
    }
    if job.copied && status.is_some_and(|status| status.artifacts.iter().any(|artifact| artifact.kind == NativeRecordingArtifactKind::Video)) {
        text.push_str(&format!("\nVideo: {}", directory.join("recording.mp4").display()));
    }
    text
}

impl Session {
    pub fn recording_dir(&self, id: &str) -> PathBuf {
        self.layout.recordings.join(id)
    }

    fn active_pointer(&self) -> PathBuf {
        self.layout.recordings.join("active")
    }

    pub fn load_job(&self, id: &str) -> Result<Job> {
        canonical_id(id)?;
        let bytes = std::fs::read(self.recording_dir(id).join("job.json")).map_err(|_| fixable("record.not_found", format!("there is no recording {id}"), "run `qareel record status` to see the current recording"))?;
        serde_json::from_slice(&bytes).context("record.corrupt: the saved recording job is unreadable")
    }

    fn save_job(&self, job: &Job) -> Result<()> {
        atomic_write(&self.recording_dir(&job.id).join("job.json"), &serde_json::to_vec_pretty(job)?)
    }

    pub fn active_id(&self) -> Result<Option<String>> {
        match std::fs::read_to_string(self.active_pointer()) {
            Ok(text) => {
                let id = text.trim().to_owned();
                canonical_id(&id)?;
                Ok(Some(id))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn recording_active(&self) -> bool {
        self.active_id().ok().flatten().and_then(|id| self.load_job(&id).ok()).is_some_and(|job| !job.copied)
    }

    fn requested_id(&self, args: &Map<String, Value>) -> Result<String> {
        match args.get("recording_id").and_then(Value::as_str) {
            Some(id) => Ok(id.trim().to_owned()),
            None => self.active_id()?.ok_or_else(|| fixable("record.none", "no recording has been started", "qareel record start")),
        }
    }

    pub async fn record(&mut self, args: &Map<String, Value>) -> Result<String> {
        match args.get("action").and_then(Value::as_str).unwrap_or("status") {
            "start" => {
                let job = self.start_recording(args).await?;
                Ok(format!("{}\nShow each shot's caption before acting with `qareel record caption \"...\"`, and stop with `qareel record stop`.", summary(&job, &self.recording_dir(&job.id))))
            }
            "caption" => {
                let text = args.get("text").or_else(|| args.get("caption")).and_then(Value::as_str).unwrap_or_default().to_owned();
                let caption = self.caption(&text, args.get("caption_id").and_then(Value::as_str)).await?;
                Ok(format!("Caption {} shown at {:.1}s.", caption.caption_id, caption.time_ms as f64 / 1000.0))
            }
            "status" => {
                let id = self.requested_id(args)?;
                let job = self.reconcile(&id, false).await?;
                Ok(summary(&job, &self.recording_dir(&id)))
            }
            "stop" => {
                let id = self.requested_id(args)?;
                let job = self.stop_recording(&id).await?;
                Ok(summary(&job, &self.recording_dir(&id)))
            }
            other => Err(fixable("args.invalid", format!("`{other}` is not a recording action"), "use `qareel record start|caption|status|stop`")),
        }
    }

    pub async fn start_recording(&mut self, args: &Map<String, Value>) -> Result<Job> {
        if let Some(id) = self.active_id()?
            && let Ok(existing) = self.load_job(&id)
            && !existing.copied
        {
            return Err(fixable("record.already_active", format!("recording {id} is still {}", existing.state()), "stop it with `qareel record stop` before starting another"));
        }
        let tab = self.current_tab()?;
        self.ensure(&tab).await?;
        self.host.require(qareel_protocol::NativeCapability::RecordingVideo)?;
        let url = self.host.tab_state(&tab).map(|state| state.url).filter(|url| !url.is_empty()).unwrap_or_else(|| self.tabs.iter().find(|item| item.id == tab).map(|item| item.url.clone()).unwrap_or_default());
        let number = |key: &str, default: u64| args.get(key).and_then(Value::as_u64).unwrap_or(default);
        let overlay = |key: &str| args.get("overlays").and_then(|overlays| overlays.get(key)).and_then(Value::as_bool).unwrap_or(false);
        let options = NativeRecordingOptions {
            scope: scope_for(&url)?,
            fps: number("fps", 30).clamp(1, 30) as u16,
            max_duration_ms: number("max_duration_ms", 300_000).clamp(1000, 300_000) as u32,
            max_bytes: number("max_bytes", MAX_BYTES).clamp(1024 * 1024, MAX_BYTES),
            max_dimension: number("max_dimension", 1280).clamp(240, 1280) as u16,
            control_policy: NativeRecordingControlPolicy::Agent,
            audio: NativeRecordingAudio::Off,
            overlays: NativeRecordingOverlays { cursor: overlay("cursor"), clicks: overlay("clicks"), captions: overlay("captions"), highlights: overlay("highlights") },
        };
        let viewport = self.host.call(&tab, NativeBrowserOperation::Evaluate { script: "[innerWidth, innerHeight]".to_owned() }, crate::host::CALL_TIMEOUT).await.ok().and_then(|value| value[0].as_f64().zip(value[1].as_f64())).unwrap_or((1280.0, 800.0));
        let id = uuid::Uuid::now_v7().to_string();
        let mut job = Job { id: id.clone(), tab: tab.clone(), url, options: options.clone(), status: None, stopping: false, copied: false, released: false, pending_caption: None, captions: Vec::new(), marks: Vec::new(), viewport };
        self.save_job(&job)?;
        atomic_write(&self.active_pointer(), id.as_bytes())?;
        let started = Instant::now();
        let value = self.host.call(&tab, NativeBrowserOperation::RecordingStart { recording_id: id.clone(), options }, Duration::from_secs(30)).await.map_err(|error| fixable("record.start_unconfirmed", format!("the engine did not confirm recording {id}: {error}"), "run `qareel record status` to reconcile it; do not start a second recording"))?;
        self.marks = Some(MarkLog { recording_id: id.clone(), started, marks: Vec::new() });
        job = self.accept(job, value).await?;
        if job.copied {
            return Err(fail("record.start_failed", format!("the recording ended as soon as it started: {}", job.status.as_ref().and_then(|status| status.reason.clone()).unwrap_or_else(|| "no reason given".to_owned()))));
        }
        Ok(job)
    }

    pub async fn caption(&mut self, text: &str, caption_id: Option<&str>) -> Result<Caption> {
        let text = text.trim();
        if text.is_empty() || text.len() > 4096 || text.chars().count() > 2048 {
            return Err(fixable("record.invalid_caption", "captions are 1 to 2048 characters", "qareel record caption \"Settings survive a reload\""));
        }
        let id = self.active_id()?.ok_or_else(|| fixable("record.none", "no recording is running", "qareel record start"))?;
        let mut job = self.load_job(&id)?;
        if job.stopping || job.copied {
            return Err(fixable("record.not_recording", "the recording is stopping or finished", "start a new recording with `qareel record start`"));
        }
        let caption_id = caption_id.map(str::to_owned).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        canonical_id(&caption_id)?;
        if let Some(existing) = job.captions.iter().find(|caption| caption.caption_id == caption_id) {
            return Ok(existing.clone());
        }
        if job.pending_caption.as_ref().is_some_and(|pending| pending != &(caption_id.clone(), text.to_owned())) {
            return Err(fixable("record.caption_pending", "an earlier caption was not confirmed", "run `qareel record status` to reconcile it first"));
        }
        job.pending_caption = Some((caption_id.clone(), text.to_owned()));
        self.persist_marks(&mut job);
        self.save_job(&job)?;
        self.apply_caption(&mut job).await
    }

    async fn apply_caption(&mut self, job: &mut Job) -> Result<Caption> {
        let Some((caption_id, text)) = job.pending_caption.clone() else {
            return Err(fail("record.caption_missing", "no caption is waiting"));
        };
        let reply = match self.host.call(&job.tab, NativeBrowserOperation::RecordingCaption { recording_id: job.id.clone(), caption_id: caption_id.clone(), text: text.clone() }, crate::host::CALL_TIMEOUT).await {
            Ok(reply) => reply,
            Err(error) => {
                let message = error.to_string();
                if ["browser.recording_caption_invalid", "browser.recording_caption_limit", "browser.recording_caption_conflict"].iter().any(|code| message.starts_with(code)) {
                    job.pending_caption = None;
                    self.save_job(job)?;
                    return Err(error);
                }
                return Err(fixable("record.caption_unconfirmed", format!("caption {caption_id} may have been shown: {message}"), "run `qareel record status` to reconcile it"));
            }
        };
        let time_ms = reply["time_ms"].as_u64().filter(|time| *time <= u64::from(job.options.max_duration_ms)).filter(|_| reply["caption_id"].as_str() == Some(caption_id.as_str())).ok_or_else(|| fail("record.invalid_reply", "the caption acknowledgement did not match"))?;
        let caption = Caption { caption_id, text, time_ms };
        job.pending_caption = None;
        job.captions.push(caption.clone());
        self.save_job(job)?;
        Ok(caption)
    }

    fn persist_marks(&mut self, job: &mut Job) {
        if let Some(log) = self.marks.as_ref().filter(|log| log.recording_id == job.id) {
            job.marks = log.marks.iter().map(|mark| SavedMark { time_ms: mark.time_ms, x: mark.rect[0], y: mark.rect[1], width: mark.rect[2], height: mark.rect[3] }).collect();
            if let Some(mark) = log.marks.last() {
                job.viewport = mark.viewport;
            }
        }
    }

    pub async fn stop_recording(&mut self, id: &str) -> Result<Job> {
        let mut job = self.load_job(id)?;
        if job.copied {
            return self.reconcile(id, false).await;
        }
        job.stopping = true;
        self.persist_marks(&mut job);
        self.save_job(&job)?;
        let mut job = self.reconcile(id, true).await?;
        let deadline = Instant::now() + STOP_WAIT;
        while !job.copied && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
            job = self.reconcile(id, false).await?;
        }
        Ok(job)
    }

    pub async fn reconcile(&mut self, id: &str, stop: bool) -> Result<Job> {
        let mut job = self.load_job(id)?;
        if job.copied {
            if !job.released {
                return self.release(job).await;
            }
            return Ok(job);
        }
        if !self.host.connected() {
            self.ensure(&job.tab).await.ok();
        }
        let operation = if stop { NativeBrowserOperation::RecordingStop { recording_id: job.id.clone() } } else { NativeBrowserOperation::RecordingStatus { recording_id: job.id.clone() } };
        match self.host.call(&job.tab, operation, crate::host::CALL_TIMEOUT).await {
            Ok(value) => {
                if !stop && job.pending_caption.is_some() {
                    let status: NativeRecordingStatus = serde_json::from_value(value.clone()).context("record.invalid_reply: unreadable recording status")?;
                    validate_status(&job, &status)?;
                    if !terminal(status.phase) {
                        self.apply_caption(&mut job).await?;
                    } else {
                        job.pending_caption = None;
                    }
                }
                self.accept(job, value).await
            }
            Err(error) if error.to_string().contains("browser_recording.not_found") => {
                job.status = Some(NativeRecordingStatus { recording_id: job.id.clone(), phase: NativeRecordingPhase::Interrupted, started_at_unix_ms: 0, duration_ms: 0, frames: 0, audio: job.options.audio, audio_status: NativeRecordingAudioStatus::Disabled, audio_gap_ms: 0, artifacts: Vec::new(), reason: Some("The browser engine no longer has this recording; the job was preserved and not replayed.".to_owned()) });
                job.copied = true;
                job.released = true;
                self.finish_job(&job)?;
                Ok(job)
            }
            Err(error) => Err(error),
        }
    }

    async fn accept(&mut self, mut job: Job, value: Value) -> Result<Job> {
        let status: NativeRecordingStatus = serde_json::from_value(value).context("record.invalid_reply: unreadable recording status")?;
        validate_status(&job, &status)?;
        if job.status.as_ref().is_some_and(|previous| terminal(previous.phase) && previous != &status) {
            return Err(fail("record.terminal_changed", "the engine changed a finished recording's description"));
        }
        let done = terminal(status.phase);
        job.status = Some(status);
        self.persist_marks(&mut job);
        self.save_job(&job)?;
        if !done {
            return Ok(job);
        }
        self.copy_artifacts(&job).await?;
        job.copied = true;
        self.finish_job(&job)?;
        self.release(job).await
    }

    fn finish_job(&mut self, job: &Job) -> Result<()> {
        self.save_job(job)?;
        if self.marks.as_ref().is_some_and(|log| log.recording_id == job.id) {
            self.marks = None;
        }
        Ok(())
    }

    async fn release(&mut self, mut job: Job) -> Result<Job> {
        let artifacts = job.status.as_ref().map(|status| status.artifacts.clone()).unwrap_or_default();
        let released = self.host.call(&job.tab, NativeBrowserOperation::RecordingRelease { recording_id: job.id.clone(), artifacts }, crate::host::CALL_TIMEOUT).await;
        if released.is_ok() || released.as_ref().is_err_and(|error| error.to_string().contains("browser_recording.not_found")) {
            job.released = true;
            self.save_job(&job)?;
        }
        Ok(job)
    }

    async fn copy_artifacts(&mut self, job: &Job) -> Result<()> {
        let status = job.status.as_ref().ok_or_else(|| fail("record.state_invalid", "the recording has no status"))?;
        let directory = self.recording_dir(&job.id);
        for artifact in &status.artifacts {
            let target = directory.join(artifact_name(artifact.kind));
            if std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() == artifact.bytes) && file_hash(&target)?.1 == artifact.sha256 {
                continue;
            }
            let temporary = directory.join(format!("{}.part", artifact_name(artifact.kind)));
            let _ = std::fs::remove_file(&temporary);
            let mut file = std::fs::OpenOptions::new().create_new(true).write(true).open(&temporary)?;
            let mut hash = Sha256::new();
            let mut offset = 0u64;
            while offset < artifact.bytes {
                let value = self.host.call(&job.tab, NativeBrowserOperation::RecordingRead { recording_id: job.id.clone(), artifact: artifact.kind, offset, max_bytes: CHUNK }, crate::host::CALL_TIMEOUT).await?;
                let chunk: NativeRecordingChunk = serde_json::from_value(value).context("record.invalid_chunk: unreadable artifact chunk")?;
                let bytes = base64::engine::general_purpose::STANDARD.decode(&chunk.data_base64).map_err(|_| fail("record.invalid_chunk", "an artifact chunk is not base64"))?;
                let end = offset + bytes.len() as u64;
                if chunk.offset != offset || chunk.total_bytes != artifact.bytes || bytes.len() > CHUNK as usize || end > artifact.bytes || chunk.eof != (end == artifact.bytes) || (bytes.is_empty() && !chunk.eof) {
                    return Err(fail("record.invalid_chunk", "an artifact chunk failed validation"));
                }
                file.write_all(&bytes)?;
                hash.update(&bytes);
                offset = end;
                if chunk.eof {
                    break;
                }
            }
            if crate::paths::hex(&hash.finalize()) != artifact.sha256 {
                drop(file);
                std::fs::remove_file(&temporary)?;
                return Err(fixable("record.hash_mismatch", "a copied recording file did not match the engine's checksum", "run `qareel record status` to copy it again"));
            }
            file.sync_all()?;
            std::fs::rename(&temporary, &target)?;
        }
        let manifest = json!({"recording_id": job.id, "tab": job.tab, "url": job.url, "started_at_unix_ms": status.started_at_unix_ms, "status": status.phase, "duration_seconds": status.duration_ms as f64 / 1000.0, "frames": status.frames, "artifacts": status.artifacts, "reason": status.reason});
        atomic_write(&directory.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)
    }
}
