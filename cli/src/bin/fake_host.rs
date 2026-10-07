use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn state(url: &str) -> Value {
    json!({"url": url, "title": "Fake page", "loading": false, "can_go_back": false, "can_go_forward": false, "control_epoch": 0, "human": false})
}

fn recording(id: &str, phase: &str) -> Value {
    json!({"recording_id": id, "phase": phase, "started_at_unix_ms": 0, "duration_ms": 0, "frames": 0, "audio": "off", "audio_status": "disabled", "audio_gap_ms": 0, "artifacts": [], "reason": if phase == "recording" { Value::Null } else { json!("The fake engine captures no frames") }})
}

fn main() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut out = std::io::stdout();
    let mut write = |value: Value| {
        let _ = writeln!(out, "{value}");
        let _ = out.flush();
    };
    if !lines.next().is_some_and(|line| line.is_ok_and(|line| serde_json::from_str::<Value>(&line).is_ok_and(|value| value["profile_dir"].is_string()))) {
        std::process::exit(1);
    }
    let mut url = "about:blank".to_owned();
    for line in lines {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { std::process::exit(1) };
        let generation = message["generation"].clone();
        if message["type"] == "hello" {
            write(json!({"type": "ready", "generation": generation, "instance_id": "6f1d1a3e-9a0b-4c55-8d1e-2a7b9c0d1e2f", "capabilities": {"version": 1, "implementation": "appkit_webkit", "presentation": "none", "operations": ["core", "snapshot", "evaluate", "recording_video", "pointer_tap"]}}));
            continue;
        }
        let operation = &message["operation"];
        let id = operation["recording_id"].as_str().unwrap_or_default().to_owned();
        let outcome = match operation["kind"].as_str().unwrap_or_default() {
            "ensure" | "navigate" => {
                url = operation["url"].as_str().unwrap_or(&url).to_owned();
                Ok(state(&url))
            }
            "resize" | "reload" => Ok(state(&url)),
            "evaluate" if operation["script"].as_str().is_some_and(|script| script.contains("qareel-fake-crash")) => std::process::exit(3),
            "evaluate" => Ok(json!(1)),
            "recording_start" => Ok(recording(&id, "recording")),
            "recording_caption" => Ok(json!({"caption_id": operation["caption_id"], "time_ms": 0})),
            "recording_stop" | "recording_status" => Ok(recording(&id, "interrupted")),
            "recording_release" => Ok(Value::Null),
            other => Err(format!("browser.capability_unavailable: the fake engine does not implement {other}")),
        };
        let outcome = match outcome {
            Ok(value) => json!({"kind": "ok", "value": value}),
            Err(message) => json!({"kind": "error", "message": message}),
        };
        write(json!({"type": "reply", "generation": generation, "id": message["id"], "outcome": outcome}));
    }
}
