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
