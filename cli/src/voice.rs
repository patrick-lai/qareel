use crate::demo::Demo;
use crate::failure::{fail, fixable};
use crate::paths::{Layout, atomic_write};
use crate::reel::{TimeMap, map_time};
use crate::script::QaOutcome;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const MIX_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const CAPTION_LEAD: f64 = 0.4;
const INTRO_AT: f64 = 0.5;
pub const VIDEO: &str = "demo.mp4";
pub const KEEP: &str = "demo.no-voice.mp4";
const NARRATED: &str = "demo.narrated.mp4";
const CUES: &str = "voiceover-cues.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Placed {
    pub id: String,
    pub start: f64,
    pub end: f64,
    pub late: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Narration {
    pub engine: String,
    pub voice: String,
    pub lines: usize,
    pub voice_seconds: f64,
    pub extended_seconds: f64,
    pub placed: Vec<Placed>,
    #[serde(default)]
    pub omitted: Vec<String>,
}

pub struct Cues {
    pub lines: Vec<Value>,
    pub omitted: Vec<String>,
}

fn outcome_words(outcome: QaOutcome) -> &'static str {
    match outcome {
        QaOutcome::Passed => "passed",
        QaOutcome::Failed => "failed",
        QaOutcome::NotChecked => "was not checked",
    }
}

pub fn cues(demo: &Demo, map: &TimeMap) -> Cues {
    let mut lines = Vec::new();
    let mut omitted = Vec::new();
    if let Some(text) = demo.plan.intro_narration() {
        lines.push(json!({"id": "intro", "text": text, "at": INTRO_AT}));
    }
    for (index, shot) in demo.plan.script.shots.iter().enumerate() {
        let Some(text) = demo.plan.shot_narration(index) else { continue };
        let number = index + 1;
        let Some(shown) = demo.shown.iter().rev().find(|shown| shown.shot == index) else {
            omitted.push(format!("shot {number} (its caption was never shown)"));
            continue;
        };
        let outcome = demo.criterion_index(&shot.criterion).ok().and_then(|position| demo.checks.get(position)).map(|check| check.outcome);
        match outcome {
            Some(QaOutcome::Passed) => {
                let at = map_time(map, shown.time_ms as f64 / 1000.0) + CAPTION_LEAD;
                lines.push(json!({"id": format!("shot-{number}"), "text": text, "at": (at * 1000.0).round() / 1000.0}));
            }
            Some(other) => omitted.push(format!("shot {number} (its check {})", outcome_words(other))),
            None => omitted.push(format!("shot {number} (no matching check)")),
        }
    }
    if let Some(text) = demo.plan.outro_narration() {
        lines.push(json!({"id": "outro", "text": text, "end": true}));
    }
    Cues { lines, omitted }
}

fn reason(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text.lines().rev().find_map(|line| line.strip_prefix("reel: ")).map(str::to_owned);
    line.unwrap_or_else(|| crate::reel::tail(stderr))
}

pub struct Options<'a> {
    pub engine: Option<&'a str>,
    pub voice: Option<&'a str>,
}

pub async fn apply(layout: &Layout, demo: &Demo, map: &TimeMap, directory: &Path, options: &Options<'_>) -> Result<Option<Narration>> {
    let planned = cues(demo, map);
    if planned.lines.is_empty() {
        return Ok(None);
    }
    let video = directory.join(VIDEO);
    let keep = directory.join(KEEP);
    let source = if keep.is_file() { keep.clone() } else { video.clone() };
    let voice = options.voice.map(str::to_owned).or_else(|| demo.plan.voiceover.as_ref().and_then(|voiceover| voiceover.voice.clone()));
    let cue_file = directory.join(CUES);
    atomic_write(&cue_file, &serde_json::to_vec_pretty(&json!({"voice": voice, "cues": planned.lines}))?)?;
    let reel = crate::paths::reel_dir()?;
    let (ffmpeg, ffprobe) = (crate::reel::tool("ffmpeg")?, crate::reel::tool("ffprobe")?);
    let (program, mut args) = crate::reel::python(layout).await?;
    let narrated = directory.join(NARRATED);
    let path = |value: &Path| value.to_string_lossy().into_owned();
    args.extend([path(&reel.join("reel.py")), "voiceover".to_owned(), "--video".to_owned(), path(&source), "--cues".to_owned(), path(&cue_file), "--out".to_owned(), path(&narrated), "--ffmpeg".to_owned(), path(&ffmpeg), "--ffprobe".to_owned(), path(&ffprobe)]);
    if let Some(engine) = options.engine {
        args.extend(["--engine".to_owned(), engine.to_owned()]);
    }
    let output = crate::reel::run(&program, &args, MIX_TIMEOUT).await?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&narrated);
        return Err(fixable("voice.failed", reason(&output.stderr), "fix the voice setup (`qareel doctor` lists what is installed), then run `qareel demo narrate`"));
    }
    let line = String::from_utf8_lossy(&output.stdout).lines().rev().find(|line| line.trim_start().starts_with('{')).map(str::to_owned).ok_or_else(|| fail("voice.output", "the voice-over step returned no result"))?;
    let mut narration: Narration = serde_json::from_str(&line).context("voice.output: the voice-over result is unreadable")?;
    narration.omitted = planned.omitted;
    if !keep.is_file() {
        std::fs::rename(&video, &keep).context("voice.replace: could not keep the video without voice-over")?;
    }
    std::fs::rename(&narrated, &video).context("voice.replace: could not put the narrated video in place")?;
    Ok(Some(narration))
}

pub fn summary(narration: &Narration) -> String {
    let late: Vec<String> = narration.placed.iter().filter(|line| line.late >= 3.0).map(|line| format!("{} ran {:.0}s late", line.id, line.late)).collect();
    let mut out = format!("Voice-over: {} lines, {:.0}s of speech, {} voice ({}).", narration.lines, narration.voice_seconds, narration.engine, narration.voice);
    if narration.extended_seconds > 0.0 {
        out.push_str(&format!(" The video was extended by {:.1}s on its last frame so the voice could finish.", narration.extended_seconds));
    }
    if !late.is_empty() {
        out.push_str(&format!(" Late: {}; shorten that narration next time.", late.join(", ")));
    }
    if !narration.omitted.is_empty() {
        out.push_str(&format!(" Left out: {}.", narration.omitted.join("; ")));
    }
    out
}

pub fn piper_voice_installed() -> bool {
    if std::env::var_os("QAREEL_PIPER_MODEL").is_some_and(|path| Path::new(&path).is_file()) {
        return true;
    }
    let Ok(layout) = Layout::current() else { return false };
    std::fs::read_dir(layout.root.join("voices")).is_ok_and(|entries| entries.flatten().any(|entry| entry.path().extension().is_some_and(|extension| extension == "onnx")))
}
