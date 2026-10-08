use qareel::args::{Spec, parse};
use qareel::demo::{Demo, evidence_markdown, merged_events};
use qareel::failure::describe;
use qareel::record::{Job, SavedMark, validate_status};
use qareel::reel::{TimeMap, map_seconds, map_time};
use qareel::review::{Decision, Review, auto_approval, decide, plan_hash, preview, warnings};
use qareel::script::{Plan, QaCheck, QaOutcome, QaShotKind, Voiceover, missing_captions, speech_seconds, validate_checks, validate_plan};
use qareel_protocol::{NativeRecordingAudio, NativeRecordingAudioStatus, NativeRecordingControlPolicy, NativeRecordingOptions, NativeRecordingOverlays, NativeRecordingPhase, NativeRecordingScope, NativeRecordingStatus};
use serde_json::json;
use std::path::PathBuf;

fn example() -> Plan {
    serde_json::from_str(qareel::guide::EXAMPLE_PLAN).expect("the guide's example plan parses")
}

fn rejection(plan: &Plan) -> String {
    let error = validate_plan(plan).expect_err("the plan is rejected");
    describe(&error).message
}

fn check(criterion: &str, outcome: QaOutcome, video_seconds: Option<u32>) -> QaCheck {
    QaCheck { criterion: criterion.to_owned(), outcome, evidence: "expected 1, observed 1".to_owned(), video_seconds }
}

fn job(marks: Vec<SavedMark>) -> Job {
    let options = NativeRecordingOptions { scope: NativeRecordingScope::Local, fps: 30, max_duration_ms: 300_000, max_bytes: 100 * 1024 * 1024, max_dimension: 1280, control_policy: NativeRecordingControlPolicy::Agent, audio: NativeRecordingAudio::Off, overlays: NativeRecordingOverlays { cursor: false, clicks: false, captions: false, highlights: false } };
    Job { id: "01a113c7-f949-76c3-b357-9686886d8e59".to_owned(), tab: "t1".to_owned(), url: "http://localhost:3000/profile".to_owned(), options, status: None, stopping: false, copied: false, released: false, pending_caption: None, captions: Vec::new(), marks, viewport: (1280.0, 800.0) }
}

#[test]
fn the_guide_example_plan_passes_validation() {
    validate_plan(&example()).expect("the example in `qareel guide` is a valid plan");
}

#[test]
fn plans_that_cannot_fail_are_rejected() {
    let mut happy = example();
    for shot in &mut happy.script.shots {
        shot.kind = QaShotKind::Happy;
    }
    assert!(rejection(&happy).contains("only happy-path"));

    let mut echo = example();
    echo.script.shots[0].falsifier = echo.script.shots[0].expect.to_uppercase();
    assert!(rejection(&echo).contains("falsifier"));

    let mut uncovered = example();
    uncovered.script.shots.retain(|shot| !shot.criterion.starts_with("Risk:"));
    assert!(rejection(&uncovered).contains("every criterion needs a shot"));

    let mut long = example();
    for shot in &mut long.script.shots {
        shot.est_seconds = 120;
    }
    assert!(rejection(&long).contains("limited to 300"));
}

#[test]
fn a_criterion_parked_with_a_reason_needs_no_shot() {
    let mut plan = example();
    plan.script.shots.retain(|shot| !shot.criterion.starts_with("Risk:"));
    plan.script.not_demonstrable.push(qareel::script::QaUnverified { item: plan.criteria[2].clone(), reason: "The API rejects empty names before the form can send one".to_owned() });
    validate_plan(&plan).expect("a parked criterion counts as covered");
}

#[test]
fn only_captions_of_shots_that_ran_are_required() {
    let plan = example();
    let srt = "1\n00:00:01,500 --> 00:00:05,500\nSaving a new name\nupdates   the header\n\n";
    let checks = [check(&plan.criteria[0], QaOutcome::Passed, Some(1)), check(&plan.criteria[1], QaOutcome::NotChecked, None), check(&plan.criteria[2], QaOutcome::Failed, Some(9))];
    assert_eq!(missing_captions(&plan.script, srt, &checks), vec!["An empty name is rejected and nothing is saved".to_owned()]);
}

#[test]
fn passed_checks_need_a_time_inside_the_video() {
    assert!(validate_checks(&[check("a", QaOutcome::Passed, None)], 10.0).is_err());
    assert!(validate_checks(&[check("a", QaOutcome::Passed, Some(11))], 10.0).is_err());
    assert!(validate_checks(&[check("a", QaOutcome::Passed, Some(9)), check("b", QaOutcome::Failed, None)], 10.0).is_ok());
}

#[test]
fn finishing_requires_every_criterion_reported_by_number_or_text() {
    let plan = example();
    let mut demo = Demo::new(plan.clone(), PathBuf::from("/tmp"));
    demo.report(demo.criterion_index("2").expect("numbered criterion"), QaOutcome::Failed, "expected Grace Hopper, observed Ada", Some(4)).expect("report");
    assert_eq!(demo.unreported(), vec![plan.criteria[0].clone(), plan.criteria[2].clone()]);
    assert_eq!(demo.criterion_index(&plan.criteria[2]).expect("exact text"), 2);
    assert!(demo.criterion_index("4").is_err());
    assert!(demo.report(0, QaOutcome::Passed, "   ", Some(1)).is_err());
}

#[test]
fn evidence_lists_every_outcome_with_polished_times_and_safe_cells() {
    let plan = example();
    let mut demo = Demo::new(plan.clone(), PathBuf::from("/tmp"));
    demo.report(0, QaOutcome::Passed, "observed a | b\nnext line", Some(2)).expect("report");
    demo.report(1, QaOutcome::Failed, "expected Grace Hopper, observed Ada", Some(9)).expect("report");
    demo.report(2, QaOutcome::NotChecked, "the API was unreachable", None).expect("report");
    demo.polished_seconds = vec![Some(5), Some(70), None];
    let markdown = evidence_markdown(&demo, "demo.mp4", 95.0);
    assert!(markdown.contains("| 1 | Saving a new display name shows it in the page header | ✅ Passed | observed a \\| b next line | 0:05 |"));
    assert!(markdown.contains("| ❌ Failed | expected Grace Hopper, observed Ada | 1:10 |"));
    assert!(markdown.contains("| ⚪ Not checked | the API was unreachable |  |"));
    assert!(markdown.contains("Revision: not a git checkout"));
}

#[test]
fn cli_click_marks_are_used_only_when_the_engine_logged_none() {
    let mark = SavedMark { time_ms: 1500, x: 10.0, y: 20.0, width: 30.0, height: 40.0 };
    let engine = json!({"marks": [{"time_ms": 900, "x": 1, "y": 2, "width": 3, "height": 4}], "viewport_width": 1280, "viewport_height": 800});
    assert_eq!(merged_events(&engine, &job(vec![mark.clone()])), engine);
    let silent = json!({"marks": [], "captions": [], "viewport_width": 0, "viewport_height": 0});
    let merged = merged_events(&silent, &job(vec![mark]));
    assert_eq!(merged["marks"], json!([{"time_ms": 1500, "x": 10.0, "y": 20.0, "width": 30.0, "height": 40.0}]));
    assert_eq!(merged["viewport_width"], json!(1280.0));
}

#[test]
fn a_completed_recording_without_frames_is_not_accepted() {
    let job = job(Vec::new());
    let status = NativeRecordingStatus { recording_id: job.id.clone(), phase: NativeRecordingPhase::Complete, started_at_unix_ms: 1, duration_ms: 0, frames: 0, audio: NativeRecordingAudio::Off, audio_status: NativeRecordingAudioStatus::Disabled, audio_gap_ms: 0, artifacts: Vec::new(), reason: None };
    assert_eq!(describe(&validate_status(&job, &status).expect_err("rejected")).code, "record.empty");
}

#[test]
fn check_times_move_past_the_title_card_and_click_holds() {
    let map = TimeMap { fps: 30, intro_seconds: 2.6, lead_gap: 0.1, holds: vec![(42, 60), (117, 60)], seconds: None };
    assert_eq!(map_seconds(&map, 1), 3);
    assert_eq!(map_seconds(&map, 3), 7);
    assert_eq!(map_seconds(&map, 5), 11);
}

#[test]
fn arguments_take_refs_selectors_points_and_key_values() {
    let click = qareel::browser::spec("click").expect("click spec");
    assert_eq!(parse(&click, &["n12".to_owned()]).expect("ref")["ref"], json!("n12"));
    assert_eq!(parse(&click, &["Save".to_owned()]).expect("text")["selector"], json!("text/Save"));
    let point = parse(&click, &["640".to_owned(), "360".to_owned()]).expect("point");
    assert_eq!((point["x"].clone(), point["y"].clone()), (json!(640.0), json!(360.0)));
    let double = parse(&click, &["#save".to_owned(), "double_click=true".to_owned()]).expect("flag");
    assert_eq!((double["selector"].clone(), double["double_click"].clone()), (json!("#save"), json!(true)));
    let typed = parse(&qareel::browser::spec("type").expect("type spec"), &["n3".to_owned(), "a=b".to_owned()]).expect("text with equals");
    assert_eq!(typed["text"], json!("a=b"));
    assert!(parse(&click, &["n1".to_owned(), "ref=n2".to_owned()]).is_err());
    let object = parse(&click, &[r#"{"ref":"n4","element":"Save"}"#.to_owned()]).expect("json object");
    assert_eq!(object["element"], json!("Save"));
    let check = Spec { command: "demo", positional: &["action", "criterion", "outcome", "evidence"], strings: &["criterion"], values: &[], pointing: false };
    assert_eq!(parse(&check, &["check".to_owned(), "1".to_owned(), "passed".to_owned(), "observed 3".to_owned()]).expect("check")["criterion"], json!("1"));
}

#[test]
fn fetched_json_hides_secrets_and_link_noise() {
    let response = json!({"path": "/api/me?token=abc123&page=2", "status": 200, "status_text": "OK", "type": "application/json", "bytes": 90, "ms": 4, "body": r#"{"name":"Ada","accessToken":"tok-1","self":"http://x","nested":{"Client_Secret":"s"},"empty":""}"#});
    let report = qareel::fetch::fetch_report("GET", &response, 4000, false);
    assert!(report.contains(r#""accessToken":"[redacted]""#) && report.contains(r#""Client_Secret":"[redacted]""#), "{report}");
    assert!(!report.contains("tok-1") && !report.contains("abc123") && !report.contains("\"self\"") && !report.contains("\"empty\""), "{report}");
    assert!(report.contains("page=2"));
}

fn narrated() -> Plan {
    let mut plan = example();
    let lines = ["Now we save a new display name and watch the header.", "Next we reload the page and look for the name again.", "Last we clear the field, save, and see whether the form pushes back."];
    for (shot, line) in plan.script.shots.iter_mut().zip(lines) {
        shot.narration = Some(line.to_owned());
    }
    plan.voiceover = Some(Voiceover { intro: Some("This is a quick check of the display name change.".to_owned()), outro: Some("That covers the three checks.".to_owned()), voice: None });
    plan
}

#[test]
fn narration_is_optional_and_a_plan_without_it_is_unchanged() {
    let plan = example();
    assert!(!plan.has_narration());
    let text = serde_json::to_string(&plan).expect("plan serializes");
    assert!(!text.contains("narration") && !text.contains("voiceover"), "{text}");
    validate_plan(&narrated()).expect("a narrated plan is valid");
    assert!(narrated().has_narration());
}

#[test]
fn bad_narration_is_rejected_with_a_fix() {
    let mut long = narrated();
    long.script.shots[0].narration = Some("word ".repeat(200));
    assert!(rejection(&long).contains("under 600 bytes"));

    let mut blank = narrated();
    blank.script.shots[1].narration = Some("   ".to_owned());
    assert!(rejection(&blank).contains("non-empty"));

    let mut nothing = example();
    nothing.voiceover = Some(Voiceover::default());
    assert!(rejection(&nothing).contains("nothing is narrated"));

    let mut voice = narrated();
    voice.voiceover.as_mut().expect("voiceover").voice = Some("x".repeat(100));
    assert!(rejection(&voice).contains("voiceover.voice"));
}

#[test]
fn longer_speech_takes_longer_to_say() {
    assert!(speech_seconds("Save the name.") < speech_seconds("Save the new display name, then reload the page and look again."));
    assert!(speech_seconds("One. Two. Three.") > speech_seconds("One Two Three"));
}

#[test]
fn a_plan_without_voice_over_never_needs_review_and_a_narrated_one_always_does() {
    assert_eq!(decide(&example(), None, 100), Decision::NotRequired);
    assert_eq!(decide(&narrated(), None, 100), Decision::NotShown);
}

#[test]
fn nobody_replying_approves_the_script_exactly_when_the_waiting_time_is_up() {
    let plan = narrated();
    let review = Review::new(&plan, 300, 0, 1000);
    assert_eq!(decide(&plan, Some(&review), 1000), Decision::Waiting { left: Some(300) });
    assert_eq!(decide(&plan, Some(&review), 1299), Decision::Waiting { left: Some(1) });
    assert_eq!(decide(&plan, Some(&review), 1300), Decision::Expired);
    let approval = auto_approval(&review, 1300);
    assert!(approval.auto && approval.by.contains("5 min"), "{approval:?}");
    let mut approved = review.clone();
    approved.approval = Some(approval.clone());
    assert_eq!(decide(&plan, Some(&approved), 99_999), Decision::Approved(approval));
}

#[test]
fn a_waiting_time_of_zero_waits_for_ever() {
    let plan = narrated();
    let review = Review::new(&plan, 0, 0, 1000);
    assert_eq!(decide(&plan, Some(&review), 1000 + 10_000_000), Decision::Waiting { left: None });
}

#[test]
fn changing_the_script_after_approval_asks_again() {
    let plan = narrated();
    let mut review = Review::new(&plan, 300, 0, 1000);
    review.approval = Some(auto_approval(&review, 1300));
    let mut edited = plan.clone();
    edited.script.shots[0].narration = Some("A different sentence entirely.".to_owned());
    assert_ne!(plan_hash(&plan), plan_hash(&edited));
    assert_eq!(decide(&edited, Some(&review), 1301), Decision::NotShown);
}

#[test]
fn the_preview_is_a_readable_script_with_timing_warnings() {
    let mut plan = narrated();
    plan.script.shots[1].narration = None;
    plan.script.shots[2].narration = Some("This line is far too long to say in a shot that was only planned for a few seconds, so the voice would run into whatever comes next.".to_owned());
    plan.script.shots[2].est_seconds = 5;
    let review = Review::new(&plan, 300, 0, 1000);
    let text = preview(&plan, &review, 1000);
    assert!(text.starts_with("# Script review: Profile: display name survives reload"));
    assert!(text.contains("waiting for your OK") && text.contains("approved automatically in 5 min"));
    assert!(text.contains("> 🎙 Now we save a new display name and watch the header."));
    assert!(text.contains("> 🎙 This is a quick check") && text.contains("> 🎙 That covers the three checks."));
    assert!(text.contains("No voice-over: this shot is silent"));
    assert!(text.contains("## Shot 3: An empty name is rejected and nothing is saved"));
    assert!(text.contains("Timing to fix") && text.contains("Shot 3 narration takes about"), "{text}");
    assert_eq!(warnings(&plan).len(), 1);
    assert!(warnings(&narrated()).is_empty(), "{:?}", warnings(&narrated()));
}

#[test]
fn an_approved_preview_says_who_approved_it() {
    let plan = narrated();
    let mut review = Review::new(&plan, 300, 2, 1000);
    review.approval = Some(auto_approval(&review, 1300));
    let text = preview(&plan, &review, 1300);
    assert!(text.contains("Status: approved automatically by qareel") && text.contains("Revision 3"), "{text}");
}

fn map() -> TimeMap {
    TimeMap { fps: 30, intro_seconds: 2.0, lead_gap: 0.1, holds: vec![(60, 30), (300, 15)], seconds: None }
}

#[test]
fn fractional_times_agree_with_the_whole_second_mapping() {
    let map = map();
    for seconds in 0..30u32 {
        assert_eq!(map_time(&map, f64::from(seconds)).floor() as u32, map_seconds(&map, seconds), "second {seconds}");
    }
    let mut last = 0.0;
    for tenth in 0..300 {
        let mapped = map_time(&map, f64::from(tenth) / 10.0);
        assert!(mapped >= last, "time went backwards at {tenth}");
        last = mapped;
    }
    assert!((map_time(&map, 1.0) - (2.0 + (30.0 - 3.0) / 30.0)).abs() < 1e-9);
    assert!((map_time(&map, 5.0) - (2.0 + (150.0 - 3.0 + 30.0) / 30.0)).abs() < 1e-9);
}

#[test]
fn a_shots_voice_lands_just_after_its_caption_and_a_failed_or_missing_shot_stays_silent() {
    let plan = narrated();
    let mut demo = Demo::new(plan.clone(), PathBuf::from("/work"));
    demo.shown = vec![qareel::demo::Shown { shot: 0, time_ms: 1000 }, qareel::demo::Shown { shot: 1, time_ms: 9000 }];
    demo.report(0, QaOutcome::Passed, "expected Grace, observed Grace", Some(1)).expect("first check");
    demo.report(1, QaOutcome::Failed, "expected Grace, observed Ada", Some(9)).expect("second check");
    let cues = qareel::voice::cues(&demo, &map());
    let ids: Vec<&str> = cues.lines.iter().map(|line| line["id"].as_str().expect("id")).collect();
    assert_eq!(ids, ["intro", "shot-1", "outro"]);
    assert_eq!(cues.lines[0]["at"], json!(0.5));
    let expected = map_time(&map(), 1.0) + 0.4;
    assert!((cues.lines[1]["at"].as_f64().expect("time") - expected).abs() < 0.001, "{:?}", cues.lines[1]);
    assert_eq!(cues.lines[2]["end"], json!(true));
    assert_eq!(cues.omitted, vec!["shot 2 (its check failed)".to_owned(), "shot 3 (its caption was never shown)".to_owned()]);
}
