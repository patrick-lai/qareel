use std::path::Path;
use std::process::{Command, Output};

struct Home {
    directory: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self { directory: tempfile::Builder::new().prefix("qareel-test").tempdir().expect("temporary home") }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_qareel"))
            .args(args)
            .current_dir(self.path())
            .env("QAREEL_HOME", self.path().join("state"))
            .env("QAREEL_HOST", env!("CARGO_BIN_EXE_qareel-fake-host"))
            .env("QAREEL_IDLE_MINUTES", "1")
            .output()
            .expect("qareel runs")
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "qareel {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn error(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(1), "qareel {args:?} should fail: {}", String::from_utf8_lossy(&output.stdout));
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = self.run(&["stop"]);
    }
}

#[test]
fn a_crashed_engine_is_reported_restarted_and_bounded() {
    let home = Home::new();
    home.ok(&["open", "http://localhost:9/profile", "wait_ready=0"]);
    assert!(home.error(&["eval", "() => 'qareel-fake-crash'"]).contains("error[browser.host_exited]"));
    assert_eq!(home.ok(&["eval", "() => 1"]).trim(), "1");
    home.error(&["eval", "() => 'qareel-fake-crash'"]);
    home.error(&["eval", "() => 'qareel-fake-crash'"]);
    let limited = home.error(&["eval", "() => 1"]);
    assert!(limited.contains("error[browser.restart_limit]") && limited.contains("fix:"), "{limited}");
}

#[test]
fn a_demo_without_frames_cannot_be_finished() {
    let home = Home::new();
    let plan = home.path().join("plan.json");
    std::fs::write(&plan, qareel::guide::EXAMPLE_PLAN).expect("plan file");
    home.ok(&["demo", "plan", "--file", plan.to_str().expect("utf-8 path")]);
    home.ok(&["open", "http://localhost:9/profile", "wait_ready=0"]);
    home.ok(&["demo", "start"]);
    assert!(home.ok(&["demo", "shot", "1"]).contains("Saving a new name updates the header"));
    home.ok(&["demo", "check", "1", "passed", "expected Grace Hopper, observed Grace Hopper"]);
    let finished = home.error(&["demo", "finish"]);
    assert!(finished.contains("error[demo.recording_incomplete]") && finished.contains("0 frames"), "{finished}");
    assert!(home.ok(&["record", "status"]).contains("interrupted"));
}

#[test]
fn unknown_commands_and_bad_plans_explain_the_fix() {
    let home = Home::new();
    let unknown = home.run(&["frobnicate"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("qareel guide"));
    let plan = home.path().join("plan.json");
    std::fs::write(&plan, r#"{"title":"x","criteria":[],"script":{"changed_behaviors":[],"risks":[],"shots":[]}}"#).expect("plan file");
    let rejected = home.error(&["demo", "plan", "--file", plan.to_str().expect("utf-8 path")]);
    assert!(rejected.contains("error[demo.plan_invalid]") && rejected.contains("fix:"), "{rejected}");
}

#[test]
fn a_batch_with_a_bad_step_runs_nothing() {
    let home = Home::new();
    let rejected = home.error(&["batch", r#"steps=[{"tool":"open","args":{"url":"http://localhost:9/x","wait_ready":0}},{"tool":"frobnicate","args":{}}]"#]);
    assert!(rejected.contains("error[batch.invalid]") && rejected.contains("step 2"), "{rejected}");
    assert!(home.ok(&["tabs"]).contains("No tabs are open"));
}

fn narrated_plan(home: &Home) -> String {
    let mut plan: serde_json::Value = serde_json::from_str(qareel::guide::EXAMPLE_PLAN).expect("example plan");
    let lines = ["Now we save a new display name and watch the header.", "Next we reload the page and look for the name again.", "Last we clear the field, save, and see whether the form pushes back."];
    for (shot, line) in plan["script"]["shots"].as_array_mut().expect("shots").iter_mut().zip(lines) {
        shot["narration"] = serde_json::json!(line);
    }
    plan["voiceover"] = serde_json::json!({"intro": "A quick check of the display name change."});
    let path = home.path().join("narrated.json");
    std::fs::write(&path, serde_json::to_vec(&plan).expect("plan json")).expect("plan file");
    path.to_str().expect("utf-8 path").to_owned()
}

#[test]
fn a_narrated_demo_will_not_record_until_the_person_has_seen_and_approved_the_script() {
    let home = Home::new();
    let plan = narrated_plan(&home);
    let saved = home.ok(&["demo", "plan", "--file", &plan]);
    assert!(saved.contains("qareel demo script"), "{saved}");
    home.ok(&["open", "http://localhost:9/profile", "wait_ready=0"]);
    assert!(home.error(&["demo", "start"]).contains("error[demo.review_needed]"));

    let shown = home.ok(&["demo", "script", "afk=0"]);
    assert!(shown.contains("# Script review:") && shown.contains("> 🎙 Now we save a new display name") && shown.contains("qareel demo wait"), "{shown}");
    let saved_script = std::fs::read_dir(home.path().join("state/demos")).expect("demos").flatten().map(|entry| entry.path().join("script.md")).find(|path| path.is_file());
    assert!(saved_script.is_some_and(|path| std::fs::read_to_string(path).is_ok_and(|text| text.contains("Script review"))));

    let blocked = home.error(&["demo", "start"]);
    assert!(blocked.contains("error[demo.review_pending]") && blocked.contains("fix:"), "{blocked}");
    assert!(home.error(&["demo", "approve", "auto=true"]).contains("error[demo.review_pending]"), "an agent cannot use the automatic approval early");
    assert!(home.ok(&["demo", "wait", "seconds=1"]).contains("Auto-approval is off"));

    assert!(home.ok(&["demo", "approve", "by=Patrick"]).contains("approved by Patrick"));
    assert!(home.ok(&["demo", "status"]).contains("Script: approved by Patrick"));
    assert!(home.ok(&["demo", "wait"]).contains("approved by Patrick"));
    assert!(home.ok(&["demo", "start"]).contains("Recording"));
}

#[test]
fn when_nobody_replies_the_script_approves_itself_and_the_evidence_says_so() {
    let home = Home::new();
    let plan = narrated_plan(&home);
    home.ok(&["demo", "plan", "--file", &plan]);
    home.ok(&["demo", "script", "afk=1"]);
    let waited = home.ok(&["demo", "wait", "seconds=30"]);
    assert!(waited.contains("approved automatically") && waited.contains("because nobody replied"), "{waited}");
    assert!(home.ok(&["demo", "status"]).contains("Script: approved automatically"));
    home.ok(&["open", "http://localhost:9/profile", "wait_ready=0"]);
    home.ok(&["demo", "start"]);
}

#[test]
fn starting_after_the_waiting_time_approves_it_without_a_separate_wait() {
    let home = Home::new();
    let plan = narrated_plan(&home);
    home.ok(&["demo", "plan", "--file", &plan]);
    home.ok(&["demo", "script", "afk=1"]);
    home.ok(&["open", "http://localhost:9/profile", "wait_ready=0"]);
    std::thread::sleep(std::time::Duration::from_millis(2200));
    let started = home.ok(&["demo", "start"]);
    assert!(started.contains("Nobody replied to the script") && started.contains("Recording"), "{started}");
}

#[test]
fn revising_the_script_asks_for_a_new_ok_but_resending_the_same_plan_does_not() {
    let home = Home::new();
    let plan = narrated_plan(&home);
    home.ok(&["demo", "plan", "--file", &plan]);
    home.ok(&["demo", "script", "afk=0"]);
    home.ok(&["demo", "approve"]);
    assert!(home.ok(&["demo", "revise", "--file", &plan]).contains("unchanged"));
    assert!(home.ok(&["demo", "status"]).contains("Script: approved"));

    let mut changed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&plan).expect("plan")).expect("json");
    changed["script"]["shots"][0]["narration"] = serde_json::json!("A softer way to say the first thing.");
    let edited = home.path().join("edited.json");
    std::fs::write(&edited, serde_json::to_vec(&changed).expect("json")).expect("file");
    let revised = home.ok(&["demo", "revise", "--file", edited.to_str().expect("utf-8 path")]);
    assert!(revised.contains("A softer way to say the first thing.") && revised.contains("Revision 2"), "{revised}");
    assert!(home.ok(&["demo", "status"]).contains("Script: waiting for the person's OK"));
    assert!(home.error(&["demo", "start"]).contains("error[demo.review_pending]"));
}

#[test]
fn a_plan_without_voice_over_records_exactly_as_before() {
    let home = Home::new();
    let plan = home.path().join("plan.json");
    std::fs::write(&plan, qareel::guide::EXAMPLE_PLAN).expect("plan file");
    let saved = home.ok(&["demo", "plan", "--file", plan.to_str().expect("utf-8 path")]);
    assert!(saved.contains("qareel demo start") && !saved.contains("demo script"), "{saved}");
    home.ok(&["open", "http://localhost:9/profile", "wait_ready=0"]);
    assert!(home.ok(&["demo", "start"]).contains("Recording"));
    assert!(home.error(&["demo", "wait"]).contains("error[demo.no_narration]"));
}
