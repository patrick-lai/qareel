use qareel::demo::{Demo, Shown};
use qareel::failure::describe;
use qareel::paths::Layout;
use qareel::reel::TimeMap;
use qareel::script::{Plan, QaOutcome, Voiceover};
use qareel::voice::{KEEP, Options, VIDEO, apply, summary};
use std::path::{Path, PathBuf};
use std::process::Command;

const STUB: &str = "import math, struct, sys, wave
words = len(sys.stdin.read().split())
rate = 22050
body = [int(9000 * math.sin(2 * math.pi * 220 * n / rate)) for n in range(int(words * 0.2 * rate))]
with wave.open(sys.argv[1], 'wb') as out:
    out.setnchannels(1)
    out.setsampwidth(2)
    out.setframerate(rate)
    out.writeframes(struct.pack('<%dh' % len(body), *body))
";

fn python_with_numpy() -> Option<PathBuf> {
    let found = Command::new("python3").args(["-c", "import numpy, sys; print(sys.executable)"]).output().ok().filter(|output| output.status.success())?;
    Some(PathBuf::from(String::from_utf8_lossy(&found.stdout).trim()))
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|output| output.status.success()) && Command::new("ffprobe").arg("-version").output().is_ok_and(|output| output.status.success())
}

fn narrated() -> Plan {
    let mut plan: Plan = serde_json::from_str(qareel::guide::EXAMPLE_PLAN).expect("example plan");
    for shot in &mut plan.script.shots {
        shot.narration = Some("We do this thing and look at the page".to_owned());
    }
    plan.voiceover = Some(Voiceover { intro: Some("A quick check".to_owned()), outro: Some("That is all".to_owned()), voice: None });
    plan
}

fn video(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=30:duration=12", "-f", "lavfi", "-i", "sine=frequency=440:duration=12", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(path)
        .status()
        .expect("ffmpeg runs");
    assert!(status.success());
}

fn audio_streams(path: &Path) -> String {
    let output = Command::new("ffprobe").args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=codec_name", "-of", "csv=p=0"]).arg(path).output().expect("ffprobe runs");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[tokio::test]
async fn the_voice_stage_adds_speech_keeps_the_silent_cut_and_never_damages_the_video_when_it_fails() {
    let Some(python) = python_with_numpy().filter(|_| have_ffmpeg()) else {
        eprintln!("skipped: needs ffmpeg and python3 with numpy");
        return;
    };
    let scratch = tempfile::tempdir().expect("temporary folder");
    let stub = scratch.path().join("speak.py");
    std::fs::write(&stub, STUB).expect("stub voice");
    unsafe {
        std::env::set_var("QAREEL_PYTHON", &python);
        std::env::set_var("QAREEL_TTS_COMMAND", format!("{} {} {{out}}", python.display(), stub.display()));
    }
    let layout = Layout::at(scratch.path().join("state")).expect("layout");
    layout.prepare().expect("prepare");
    let directory = scratch.path().join("out");
    std::fs::create_dir_all(&directory).expect("output folder");
    video(&directory.join(VIDEO));
    let original = std::fs::read(directory.join(VIDEO)).expect("video");

    let mut demo = Demo::new(narrated(), scratch.path().to_path_buf());
    demo.shown = vec![Shown { shot: 0, time_ms: 1000 }, Shown { shot: 1, time_ms: 4000 }, Shown { shot: 2, time_ms: 7000 }];
    for (index, criterion) in demo.plan.criteria.clone().iter().enumerate() {
        let outcome = if criterion.starts_with("Risk:") { QaOutcome::Failed } else { QaOutcome::Passed };
        demo.report(index, outcome, "expected a, observed a", Some(index as u32)).expect("check");
    }
    let map = TimeMap { fps: 30, intro_seconds: 1.0, lead_gap: 0.1, holds: Vec::new(), seconds: Some(12.0) };

    let narration = apply(&layout, &demo, &map, &directory, &Options { engine: None, voice: None }).await.expect("voice-over is added").expect("there are lines");
    assert_eq!(narration.engine, "command");
    assert_eq!(narration.lines, 4);
    assert_eq!(narration.omitted, vec!["shot 3 (its check failed)".to_owned()]);
    assert_eq!(narration.placed.iter().map(|line| line.id.as_str()).collect::<Vec<_>>(), ["intro", "shot-1", "shot-2", "outro"]);
    assert!(narration.placed.windows(2).all(|pair| pair[0].end <= pair[1].start), "{:?}", narration.placed);
    assert_eq!(std::fs::read(directory.join(KEEP)).expect("kept silent cut"), original);
    assert_ne!(std::fs::read(directory.join(VIDEO)).expect("narrated video"), original);
    assert_eq!(audio_streams(&directory.join(VIDEO)), "aac");
    let text = summary(&narration);
    assert!(text.contains("4 lines") && text.contains("Left out: shot 3 (its check failed)"), "{text}");

    let again = apply(&layout, &demo, &map, &directory, &Options { engine: None, voice: Some("calm") }).await.expect("voice-over can be redone").expect("there are lines");
    assert_eq!(again.lines, 4);
    assert_eq!(std::fs::read(directory.join(KEEP)).expect("silent cut is still the original"), original, "redoing the voice starts from the silent cut");

    std::fs::write(&stub, "import sys\nsys.stdin.read()\n").expect("broken voice");
    let before = std::fs::read(directory.join(VIDEO)).expect("narrated video");
    let failure = describe(&apply(&layout, &demo, &map, &directory, &Options { engine: None, voice: None }).await.expect_err("a voice that writes nothing fails"));
    assert_eq!(failure.code, "voice.failed");
    assert!(failure.message.contains("shot-1") || failure.message.contains("intro"), "{}", failure.message);
    assert!(failure.fix.is_some_and(|fix| fix.contains("qareel demo narrate")));
    assert_eq!(std::fs::read(directory.join(VIDEO)).expect("video untouched"), before);
    assert!(!directory.join("demo.narrated.mp4").exists());
}

#[tokio::test]
async fn a_demo_where_every_narrated_shot_failed_has_nothing_to_voice() {
    let scratch = tempfile::tempdir().expect("temporary folder");
    let layout = Layout::at(scratch.path().join("state")).expect("layout");
    let mut plan = narrated();
    plan.voiceover = None;
    let demo = Demo::new(plan, scratch.path().to_path_buf());
    let map = TimeMap { fps: 30, intro_seconds: 1.0, lead_gap: 0.1, holds: Vec::new(), seconds: Some(12.0) };
    assert!(apply(&layout, &demo, &map, scratch.path(), &Options { engine: None, voice: None }).await.expect("no error").is_none());
}
