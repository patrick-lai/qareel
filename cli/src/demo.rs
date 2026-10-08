use crate::browser::Session;
use crate::failure::{fail, fixable};
use crate::paths::atomic_write;
use crate::reel::TimeMap;
use crate::record::{Job, SavedMark};
use crate::review::{self, Approval, Decision, Review};
use crate::script::{Plan, QaCheck, QaOutcome, missing_captions, validate_checks, validate_plan};
use crate::voice::{self, Narration};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Planned,
    Running,
    Recorded,
    Polished,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Revision {
    pub root: String,
    pub head: String,
    pub dirty: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Shown {
    pub shot: usize,
    pub time_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Demo {
    pub id: String,
    pub plan: Plan,
    pub cwd: PathBuf,
    pub stage: Stage,
    pub checks: Vec<QaCheck>,
    pub reported: Vec<bool>,
    pub revision: Option<Revision>,
    pub recording_id: Option<String>,
    pub shown: Vec<Shown>,
    pub missing_captions: Vec<String>,
    pub polished_seconds: Vec<Option<u32>>,
    pub output: Option<PathBuf>,
    #[serde(default)]
    pub review: Option<Review>,
    #[serde(default)]
    pub timemap: Option<TimeMap>,
    #[serde(default)]
    pub narration: Option<Narration>,
}

impl Demo {
    pub fn new(plan: Plan, cwd: PathBuf) -> Self {
        let checks = plan.criteria.iter().map(|criterion| QaCheck { criterion: criterion.clone(), outcome: QaOutcome::NotChecked, evidence: "Not yet exercised".to_owned(), video_seconds: None }).collect();
        let reported = vec![false; plan.criteria.len()];
        Self { id: uuid::Uuid::now_v7().to_string(), plan, cwd, stage: Stage::Planned, checks, reported, revision: None, recording_id: None, shown: Vec::new(), missing_captions: Vec::new(), polished_seconds: Vec::new(), output: None, review: None, timemap: None, narration: None }
    }

    pub fn criterion_index(&self, wanted: &str) -> Result<usize> {
        let wanted = wanted.trim();
        if let Ok(number) = wanted.parse::<usize>()
            && (1..=self.checks.len()).contains(&number)
        {
            return Ok(number - 1);
        }
        self.checks.iter().position(|check| check.criterion.trim() == wanted).ok_or_else(|| fixable("demo.criterion", format!("`{wanted}` is not a planned criterion"), "pass the criterion's number from `qareel demo status` or its exact text"))
    }

    pub fn report(&mut self, index: usize, outcome: QaOutcome, evidence: &str, video_seconds: Option<u32>) -> Result<()> {
        let evidence = evidence.trim();
        if evidence.is_empty() || evidence.len() > 4000 {
            return Err(fixable("demo.evidence", "evidence is required and must be under 4000 bytes", "describe expected versus observed with exact values"));
        }
        let check = self.checks.get_mut(index).ok_or_else(|| fail("demo.criterion", "that criterion does not exist"))?;
        check.outcome = outcome;
        check.evidence = evidence.to_owned();
        check.video_seconds = video_seconds;
        if let Some(reported) = self.reported.get_mut(index) {
            *reported = true;
        }
        Ok(())
    }

    pub fn shot_time(&self, index: usize) -> Option<u64> {
        let criterion = self.checks.get(index)?.criterion.trim().to_owned();
        self.shown.iter().rev().find(|shown| self.plan.script.shots.get(shown.shot).is_some_and(|shot| shot.criterion.trim() == criterion)).map(|shown| shown.time_ms)
    }

    pub fn unreported(&self) -> Vec<String> {
        self.checks.iter().zip(&self.reported).filter(|(_, reported)| !**reported).map(|(check, _)| check.criterion.clone()).collect()
    }
}

pub fn load_current(layout: &crate::paths::Layout) -> Result<Demo> {
    let id = std::fs::read_to_string(layout.demos.join("current")).map_err(|_| fixable("demo.none", "no demo is planned", "write a plan and run `qareel demo plan --file plan.json`; `qareel guide` shows the format"))?;
    crate::record::canonical_id(id.trim())?;
    let bytes = std::fs::read(layout.demos.join(id.trim()).join("demo.json")).context("demo.corrupt: the saved demo is missing")?;
    serde_json::from_slice(&bytes).context("demo.corrupt: the saved demo is unreadable")
}

fn clock(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn cell(text: &str) -> String {
    text.replace(['\n', '\r'], " ").replace('|', "\\|")
}

pub fn evidence_markdown(demo: &Demo, video_name: &str, duration_seconds: f64) -> String {
    let mut out = format!("### QA demo: {}\n\n", cell(&demo.plan.title));
    if let Some(subtitle) = demo.plan.subtitle.as_deref().filter(|subtitle| !subtitle.trim().is_empty()) {
        out.push_str(&format!("{}\n\n", cell(subtitle)));
    }
    let revision = demo.revision.as_ref().map_or("not a git checkout".to_owned(), |revision| format!("`{}`", revision.head));
    out.push_str(&format!("Video: `{video_name}` ({}). Revision: {revision}.\n\n", clock(duration_seconds.round() as u32)));
    out.push_str("| # | Criterion | Result | Evidence | Video |\n|---|---|---|---|---|\n");
    for (index, check) in demo.checks.iter().enumerate() {
        let result = match check.outcome {
            QaOutcome::Passed => "✅ Passed",
            QaOutcome::Failed => "❌ Failed",
            QaOutcome::NotChecked => "⚪ Not checked",
        };
        let at = demo.polished_seconds.get(index).copied().flatten().or(check.video_seconds).map(clock).unwrap_or_default();
        out.push_str(&format!("| {} | {} | {result} | {} | {at} |\n", index + 1, cell(&check.criterion), cell(&check.evidence)));
    }
    if !demo.plan.script.not_demonstrable.is_empty() {
        out.push_str("\n**Not demonstrable**\n\n");
        for item in &demo.plan.script.not_demonstrable {
            out.push_str(&format!("- {}: {}\n", cell(&item.item), cell(&item.reason)));
        }
    }
    if !demo.missing_captions.is_empty() {
        out.push_str(&format!("\n> The planned caption is missing from the video for: {}.\n", demo.missing_captions.iter().map(|caption| cell(caption)).collect::<Vec<_>>().join("; ")));
    }
    if demo.plan.has_narration() {
        if let Some(approval) = demo.review.as_ref().and_then(|review| review.approval.as_ref()) {
            out.push_str(&format!("\nVoice-over script {}.\n", review::describe_approval(approval)));
        }
        if let Some(narration) = &demo.narration {
            out.push_str(&format!("\n{}\n", voice::summary(narration)));
        }
    }
    out.push_str("\nOutcomes are reported by the agent that ran the demo. qareel verified that the recording is complete, every criterion was reported with a time inside the video, and the checkout did not change while recording.\n");
    out
}

pub fn merged_events(events: &Value, job: &Job) -> Value {
    let mut merged = events.clone();
    let empty = events["marks"].as_array().is_none_or(Vec::is_empty);
    if empty && !job.marks.is_empty() {
        merged["marks"] = json!(job.marks.iter().map(|mark: &SavedMark| json!({"time_ms": mark.time_ms, "x": mark.x, "y": mark.y, "width": mark.width, "height": mark.height})).collect::<Vec<_>>());
        if events["viewport_width"].as_f64().is_none_or(|width| width <= 0.0) {
            merged["viewport_width"] = json!(job.viewport.0);
            merged["viewport_height"] = json!(job.viewport.1);
        }
    }
    merged
}

fn display_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => format!("{}{}", url.origin().ascii_serialization(), url.path()),
        _ => String::new(),
    }
}

async fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(cwd).args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(GIT_TIMEOUT, command.output()).await.ok()?.ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub async fn revision(cwd: &Path) -> Result<Option<Revision>> {
    let Some(root) = git(cwd, &["rev-parse", "--show-toplevel"]).await else { return Ok(None) };
    let head = git(cwd, &["rev-parse", "HEAD"]).await.ok_or_else(|| fixable("demo.revision", "the checkout has no commit yet", "commit the change under test, then start the demo"))?;
    let status = match git(cwd, &["status", "--porcelain=v1", "--untracked-files=no"]).await {
        Some(status) => status,
        None => git(cwd, &["-c", "core.fsmonitor=false", "status", "--porcelain=v1", "--untracked-files=no"]).await.ok_or_else(|| fail("demo.revision", "git status failed in the demo folder"))?,
    };
    Ok(Some(Revision { root, head, dirty: !status.is_empty() }))
}

impl Session {
    fn demo_dir(&self, id: &str) -> PathBuf {
        self.layout.demos.join(id)
    }

    fn current_pointer(&self) -> PathBuf {
        self.layout.demos.join("current")
    }

    pub fn current_demo(&self) -> Result<Demo> {
        load_current(&self.layout)
    }

    fn save_demo(&self, demo: &Demo) -> Result<()> {
        atomic_write(&self.demo_dir(&demo.id).join("demo.json"), &serde_json::to_vec_pretty(demo)?)
    }

    pub async fn demo(&mut self, args: &Map<String, Value>, input: Option<&str>, cwd: &Path) -> Result<String> {
        match args.get("action").and_then(Value::as_str).unwrap_or("status") {
            "plan" => self.demo_plan(input, cwd),
            "start" => self.demo_start(args).await,
            "shot" => self.demo_shot(args).await,
            "check" => self.demo_check(args).await,
            "status" => self.demo_status(),
            "finish" => self.demo_finish(args, cwd).await,
            "script" => self.demo_script(args),
            "revise" => self.demo_revise(input),
            "approve" => self.demo_approve(args),
            "narrate" => self.demo_narrate(args).await,
            other => Err(fixable("args.invalid", format!("`{other}` is not a demo action"), "use `qareel demo plan|script|wait|approve|revise|start|shot|check|status|finish|narrate`")),
        }
    }

    fn demo_plan(&mut self, input: Option<&str>, cwd: &Path) -> Result<String> {
        let text = input.ok_or_else(|| fixable("args.invalid", "the plan JSON is required", "qareel demo plan --file plan.json   (or --file - to read stdin)"))?;
        let plan: Plan = serde_json::from_str(text).map_err(|error| fixable("demo.plan_invalid", format!("the plan is not valid: {error}"), "`qareel guide` shows the plan format"))?;
        validate_plan(&plan)?;
        if let Ok(existing) = self.current_demo()
            && existing.stage == Stage::Running
            && self.recording_active()
        {
            return Err(fixable("demo.running", "the current demo is still recording", "finish it with `qareel demo finish` before planning another"));
        }
        let demo = Demo::new(plan, cwd.to_path_buf());
        self.save_demo(&demo)?;
        atomic_write(&self.current_pointer(), demo.id.as_bytes())?;
        let seconds: u32 = demo.plan.script.shots.iter().map(|shot| shot.est_seconds).sum();
        let next = if demo.plan.has_narration() { "This plan has voice-over, so show the person its script first: run `qareel demo script`, then follow what it prints." } else { "Next: start the app, `qareel open URL` on the page you will show first, then `qareel demo start`." };
        Ok(format!("Plan saved: {} criteria, {} shots, about {seconds}s on camera.\n{next}", demo.checks.len(), demo.plan.script.shots.len()))
    }

    fn show_script(&self, demo: &Demo, now: u64) -> Result<String> {
        let review = demo.review.as_ref().ok_or_else(|| fixable("demo.review_needed", "the script has not been shown yet", "run `qareel demo script`"))?;
        let text = review::preview(&demo.plan, review, now);
        atomic_write(&self.demo_dir(&demo.id).join("script.md"), text.as_bytes())?;
        Ok(text)
    }

    fn next_for_review(&self, demo: &Demo, now: u64) -> String {
        let path = self.demo_dir(&demo.id).join("script.md");
        match review::decide(&demo.plan, demo.review.as_ref(), now) {
            Decision::Approved(approval) => format!("Script {}. Next: start the app, open the first page, then `qareel demo start`.", review::describe_approval(&approval)),
            _ => format!("Saved to {}.\nNext: show this script to the person (paste it into your reply, it is Markdown) and ask whether it is good or what to change. Then run `qareel demo wait` and repeat it until it says the script is approved.\n- They say it is fine: `qareel demo approve`.\n- They want changes: edit the plan JSON, run `qareel demo revise --file plan.json`, show the new script, wait again.\n- They do not reply: `qareel demo wait` approves the script by itself once the waiting time is up. Do not approve for them and do not start recording before it says approved.", path.display()),
        }
    }

    fn demo_script(&mut self, args: &Map<String, Value>) -> Result<String> {
        let mut demo = self.current_demo()?;
        if !demo.plan.has_narration() {
            return Err(fixable("demo.no_narration", "the plan has no voice-over, so there is no script to review", "add `narration` to shots (and `voiceover.intro` / `voiceover.outro` if you want them), save with `qareel demo revise --file plan.json`, then run `qareel demo script`"));
        }
        let now = review::now();
        let asked = args.get("afk").map(|value| match value { Value::String(text) => text.trim().parse::<u64>().ok(), other => other.as_u64() });
        if asked == Some(None) {
            return Err(fixable("args.invalid", "afk is the number of seconds to wait before approving automatically, or 0 to wait for ever", "qareel demo script afk=300"));
        }
        let hash = review::plan_hash(&demo.plan);
        let afk = asked.flatten().map(|seconds| seconds.min(review::MAX_AFK_SECONDS)).or_else(|| demo.review.as_ref().map(|review| review.afk_seconds)).unwrap_or_else(review::default_afk);
        let keep = demo.review.as_ref().is_some_and(|review| review.plan_hash == hash && review.afk_seconds == afk);
        if !keep {
            let revisions = demo.review.as_ref().map_or(0, |review| review.revisions);
            demo.review = Some(Review::new(&demo.plan, afk, revisions, now));
            self.save_demo(&demo)?;
        }
        let text = self.show_script(&demo, now)?;
        Ok(format!("{text}\n{}", self.next_for_review(&demo, now)))
    }

    fn demo_revise(&mut self, input: Option<&str>) -> Result<String> {
        let text = input.ok_or_else(|| fixable("args.invalid", "the revised plan JSON is required", "qareel demo revise --file plan.json"))?;
        let plan: Plan = serde_json::from_str(text).map_err(|error| fixable("demo.plan_invalid", format!("the plan is not valid: {error}"), "`qareel guide` shows the plan format"))?;
        validate_plan(&plan)?;
        let mut demo = self.current_demo()?;
        if demo.stage != Stage::Planned {
            return Err(fixable("demo.recorded", "this demo has started recording, so its script can no longer change", "save a new plan with `qareel demo plan --file plan.json`"));
        }
        let now = review::now();
        let changed = review::plan_hash(&plan) != review::plan_hash(&demo.plan);
        let revisions = demo.review.as_ref().map_or(0, |review| review.revisions + u32::from(changed));
        let afk = demo.review.as_ref().map_or_else(review::default_afk, |review| review.afk_seconds);
        let fresh = Demo::new(plan.clone(), demo.cwd.clone());
        demo.plan = plan;
        demo.checks = fresh.checks;
        demo.reported = fresh.reported;
        if changed || demo.review.is_none() {
            demo.review = demo.plan.has_narration().then(|| Review::new(&demo.plan, afk, revisions, now));
        }
        self.save_demo(&demo)?;
        if !demo.plan.has_narration() {
            return Ok("Plan revised. It has no voice-over, so nothing needs approval. Next: start the app, open the first page, then `qareel demo start`.".to_owned());
        }
        if !changed {
            return Ok(format!("The plan is unchanged, so the earlier review stands.\n{}", self.next_for_review(&demo, now)));
        }
        let text = self.show_script(&demo, now)?;
        Ok(format!("{text}\n{}", self.next_for_review(&demo, now)))
    }

    fn demo_approve(&mut self, args: &Map<String, Value>) -> Result<String> {
        let mut demo = self.current_demo()?;
        let now = review::now();
        let auto = args.get("auto").and_then(Value::as_bool).unwrap_or(false);
        let decision = review::decide(&demo.plan, demo.review.as_ref(), now);
        let approval = match decision {
            Decision::NotRequired => return Err(fixable("demo.no_narration", "this plan has no voice-over, so there is nothing to approve", "run `qareel demo start`")),
            Decision::NotShown => return Err(fixable("demo.review_needed", "the script has not been shown yet", "run `qareel demo script`, show it to the person, then `qareel demo wait`")),
            Decision::Approved(approval) => return Ok(format!("Script {}. Nothing more to approve.", review::describe_approval(&approval))),
            Decision::Waiting { left } if auto => return Err(fixable("demo.review_pending", format!("the waiting time is not up yet{}", left.map(|left| format!(" ({} left)", review::span(left))).unwrap_or_default()), "automatic approval only happens after nobody replied for the whole waiting time")),
            Decision::Waiting { .. } | Decision::Expired => {
                let review = demo.review.as_ref().ok_or_else(|| fail("demo.state", "the review is missing"))?;
                if auto { review::auto_approval(review, now) } else { Approval { by: args.get("by").and_then(Value::as_str).map(str::trim).filter(|name| !name.is_empty()).unwrap_or("the person").to_owned(), at: now, auto: false } }
            }
        };
        if let Some(review) = demo.review.as_mut() {
            review.approval = Some(approval.clone());
        }
        self.save_demo(&demo)?;
        self.show_script(&demo, now)?;
        Ok(format!("Script {}. Next: start the app, `qareel open URL` on the first page, then `qareel demo start`.", review::describe_approval(&approval)))
    }

    async fn demo_narrate(&mut self, args: &Map<String, Value>) -> Result<String> {
        let mut demo = self.current_demo()?;
        if !demo.plan.has_narration() {
            return Err(fixable("demo.no_narration", "this plan has no voice-over", "add narration with `qareel demo revise --file plan.json` before recording"));
        }
        let (Some(output), Some(map)) = (demo.output.clone(), demo.timemap.clone()) else {
            return Err(fixable("demo.not_finished", "`narrate` adds the voice-over again to a finished demo", "run `qareel demo finish` first; it adds the voice-over itself"));
        };
        let engine = args.get("engine").and_then(Value::as_str);
        let voice_name = args.get("voice").and_then(Value::as_str);
        let narration = voice::apply(&self.layout, &demo, &map, &output, &voice::Options { engine, voice: voice_name }).await?.ok_or_else(|| fixable("demo.nothing_to_voice", "every narrated shot was left out because its check did not pass", "fix the failed checks and record again"))?;
        let note = voice::summary(&narration);
        demo.narration = Some(narration.clone());
        self.save_demo(&demo)?;
        let duration = map.seconds.unwrap_or(0.0) + narration.extended_seconds;
        atomic_write(&output.join("evidence.md"), evidence_markdown(&demo, "demo.mp4", duration).as_bytes())?;
        Ok(format!("Voice-over added.\nVideo: {}\n{note}\nThe video without voice-over is kept as {}.", output.join(voice::VIDEO).display(), output.join(voice::KEEP).display()))
    }

    async fn demo_start(&mut self, args: &Map<String, Value>) -> Result<String> {
        let mut demo = self.current_demo()?;
        let retry = demo.stage != Stage::Planned;
        if retry {
            let id = demo.recording_id.clone().ok_or_else(|| fail("demo.state", "the demo has no recording"))?;
            let job = self.load_job(&id)?;
            if job.state() == "complete" || demo.stage == Stage::Polished {
                return Err(fixable("demo.recorded", "this demo already has a complete recording, which is kept as evidence", "finish it with `qareel demo finish`, or save a new plan to record another demo"));
            }
            if !job.copied {
                self.stop_recording(&id).await?;
            }
        }
        let mut notice = String::new();
        let now = review::now();
        match review::decide(&demo.plan, demo.review.as_ref(), now) {
            Decision::NotRequired | Decision::Approved(_) => {}
            Decision::NotShown => return Err(fixable("demo.review_needed", "this plan has voice-over, so its script must be shown to the person before recording", "run `qareel demo script`, show it to them, then `qareel demo wait`")),
            Decision::Waiting { left } => return Err(fixable("demo.review_pending", format!("the script is waiting for the person's OK{}", left.map(|left| format!(" ({} left before it is approved automatically)", review::span(left))).unwrap_or_default()), "run `qareel demo wait`; approve with `qareel demo approve` only after the person says it is fine")),
            Decision::Expired => {
                if let Some(review) = demo.review.as_mut() {
                    let approval = review::auto_approval(review, now);
                    notice = format!("Nobody replied to the script, so qareel {} (the evidence says so).\n", review::describe_approval(&approval));
                    review.approval = Some(approval);
                }
                self.save_demo(&demo)?;
            }
        }
        let revision = revision(&demo.cwd).await?;
        if revision.as_ref().is_some_and(|revision| revision.dirty) {
            return Err(fixable("demo.dirty", "the checkout has uncommitted tracked changes, so the video would not prove one exact revision", "commit or stash tracked changes, then run `qareel demo start` again"));
        }
        if let Some(url) = args.get("url").and_then(Value::as_str).filter(|url| !url.trim().is_empty()) {
            self.run("open", &Map::from_iter([("url".to_owned(), json!(url))]), &demo.cwd).await?;
        }
        let job = self.start_recording(&Map::new()).await?;
        demo.revision = revision;
        demo.recording_id = Some(job.id.clone());
        demo.stage = Stage::Running;
        demo.shown.clear();
        demo.checks = Demo::new(demo.plan.clone(), demo.cwd.clone()).checks;
        demo.reported = vec![false; demo.checks.len()];
        self.save_demo(&demo)?;
        let mut out = format!("{notice}Recording {} started{}.\nFor each shot: run `qareel demo shot N` (shows its caption), do the actions, then report with `qareel demo check N passed|failed|not_checked \"expected ..., observed ...\"` where N is the criterion number.\n", job.id, if retry { " again after the previous recording failed" } else { "" });
        for (index, shot) in demo.plan.script.shots.iter().enumerate() {
            let criterion = demo.criterion_index(&shot.criterion).map(|index| index + 1).unwrap_or(0);
            out.push_str(&format!("- Shot {} (criterion {criterion}): {}\n", index + 1, shot.caption));
        }
        Ok(out)
    }

    async fn demo_shot(&mut self, args: &Map<String, Value>) -> Result<String> {
        let mut demo = self.current_demo()?;
        if demo.stage != Stage::Running {
            return Err(fixable("demo.not_running", "the demo is not recording", "qareel demo start"));
        }
        let number = args.get("shot").and_then(|value| value.as_u64().or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))).filter(|number| (1..=demo.plan.script.shots.len() as u64).contains(number)).ok_or_else(|| fixable("args.invalid", format!("pass a shot number from 1 to {}", demo.plan.script.shots.len()), "qareel demo shot 1"))? as usize;
        let shot = demo.plan.script.shots[number - 1].clone();
        let caption = self.caption(&shot.caption, None).await?;
        demo.shown.push(crate::demo::Shown { shot: number - 1, time_ms: caption.time_ms });
        self.save_demo(&demo)?;
        let criterion = demo.criterion_index(&shot.criterion).map(|index| index + 1).unwrap_or(0);
        Ok(format!("Shot {number} caption shown at {:.1}s: {}\nSetup: {}\nDo: {}\nExpect: {}\nIt is broken if: {}\nThen: qareel demo check {criterion} passed|failed|not_checked \"expected ..., observed ...\"", caption.time_ms as f64 / 1000.0, shot.caption, shot.setup, shot.action, shot.expect, shot.falsifier))
    }

    async fn demo_check(&mut self, args: &Map<String, Value>) -> Result<String> {
        let mut demo = self.current_demo()?;
        if !matches!(demo.stage, Stage::Running | Stage::Recorded) {
            return Err(fixable("demo.not_running", "report checks after `qareel demo start`", "qareel demo start"));
        }
        let wanted = args.get("criterion").map(|value| match value { Value::String(text) => text.clone(), other => other.to_string() }).ok_or_else(|| fixable("args.invalid", "the criterion number is required", "qareel demo check 1 passed \"expected X, observed X\""))?;
        let index = demo.criterion_index(&wanted)?;
        let outcome = match args.get("outcome").and_then(Value::as_str).map(str::trim) {
            Some("passed" | "pass") => QaOutcome::Passed,
            Some("failed" | "fail") => QaOutcome::Failed,
            Some("not_checked" | "skip") => QaOutcome::NotChecked,
            _ => return Err(fixable("args.invalid", "the outcome is passed, failed or not_checked", "qareel demo check 1 failed \"expected 3 rows, observed 2\"")),
        };
        let evidence = args.get("evidence").and_then(Value::as_str).unwrap_or_default().to_owned();
        let live = self.marks.as_ref().filter(|log| Some(&log.recording_id) == demo.recording_id.as_ref()).map(|log| log.started.elapsed().as_millis() as u64);
        let time_ms = demo.shot_time(index).or(live);
        demo.report(index, outcome, &evidence, time_ms.map(|time| (time / 1000) as u32))?;
        self.save_demo(&demo)?;
        let left = demo.unreported().len();
        let at = time_ms.map(|time| format!(" at {}", clock((time / 1000) as u32))).unwrap_or_default();
        Ok(format!("Criterion {} recorded as {}{at}. {}", index + 1, serde_json::to_value(outcome)?.as_str().unwrap_or("reported"), if left == 0 { "Every criterion is reported; run `qareel demo finish`.".to_owned() } else { format!("{left} criteria left to report.") }))
    }

    fn demo_status(&self) -> Result<String> {
        let demo = self.current_demo()?;
        let mut out = format!("Demo {}: {:?}\n", demo.plan.title, demo.stage).replace("Planned", "planned").replace("Running", "recording").replace("Recorded", "recorded").replace("Polished", "polished");
        for (index, (check, reported)) in demo.checks.iter().zip(&demo.reported).enumerate() {
            let state = if *reported { serde_json::to_value(check.outcome)?.as_str().unwrap_or_default().to_owned() } else { "pending".to_owned() };
            out.push_str(&format!("{}. [{state}] {}\n", index + 1, check.criterion));
        }
        for (index, shot) in demo.plan.script.shots.iter().enumerate() {
            let shown = if demo.shown.iter().any(|shown| shown.shot == index) { "shown" } else { "not shown" };
            out.push_str(&format!("Shot {} ({shown}): {}\n", index + 1, shot.caption));
        }
        match review::decide(&demo.plan, demo.review.as_ref(), review::now()) {
            Decision::NotRequired => {}
            Decision::NotShown => out.push_str("Script: voice-over needs review; run `qareel demo script`\n"),
            Decision::Approved(approval) => out.push_str(&format!("Script: {}\n", review::describe_approval(&approval))),
            Decision::Waiting { left } => out.push_str(&format!("Script: waiting for the person's OK{}\n", left.map(|left| format!(", approved automatically in {}", review::span(left))).unwrap_or_default())),
            Decision::Expired => out.push_str("Script: the waiting time is up; `qareel demo wait` or `qareel demo start` approves it\n"),
        }
        if let Some(narration) = &demo.narration {
            out.push_str(&format!("{}\n", voice::summary(narration)));
        }
        if let Some(output) = &demo.output {
            out.push_str(&format!("Output: {}\n", output.display()));
        }
        Ok(out)
    }

    async fn demo_finish(&mut self, args: &Map<String, Value>, cwd: &Path) -> Result<String> {
        let mut demo = self.current_demo()?;
        let output = match args.get("out").and_then(Value::as_str).filter(|out| !out.trim().is_empty()) {
            Some(out) if Path::new(out).is_absolute() => PathBuf::from(out),
            Some(out) => cwd.join(out),
            None => self.demo_dir(&demo.id),
        };
        if demo.stage == Stage::Polished && demo.output.as_ref() == Some(&output) {
            let evidence = std::fs::read_to_string(output.join("evidence.md")).unwrap_or_default();
            return Ok(format!("Already finished.\nVideo: {}\nEvidence: {}\n\n{evidence}", output.join("demo.mp4").display(), output.join("evidence.md").display()));
        }
        let id = demo.recording_id.clone().ok_or_else(|| fixable("demo.not_running", "the demo has no recording", "qareel demo start"))?;
        let mut job = self.load_job(&id)?;
        if !job.copied {
            job = self.stop_recording(&id).await?;
        }
        let status = job.status.clone().ok_or_else(|| fail("demo.recording_incomplete", "the recording has no final state"))?;
        if job.state() != "complete" || status.frames == 0 || status.duration_ms < 1000 {
            return Err(fixable("demo.recording_incomplete", format!("the recording ended as {} with {} frames ({}); an incomplete recording is not evidence", job.state(), status.frames, status.reason.clone().unwrap_or_else(|| "no reason given".to_owned())), "fix the cause, then record again with `qareel demo start`"));
        }
        let unreported = demo.unreported();
        if !unreported.is_empty() {
            return Err(fixable("demo.unreported", format!("report every planned criterion, including failed and untested ones; missing: {}", unreported.join("; ")), "qareel demo check N passed|failed|not_checked \"evidence\", then `qareel demo finish` again"));
        }
        let duration = status.duration_ms as f64 / 1000.0;
        validate_checks(&demo.checks, duration)?;
        if let Some(bound) = &demo.revision {
            let now = revision(&demo.cwd).await?;
            if now.as_ref().is_none_or(|now| now.head != bound.head || now.dirty) {
                return Err(fixable("demo.stale", "the checkout changed during the demo, so the video no longer proves the current revision", "commit nothing while recording; plan and record again with `qareel demo plan` and `qareel demo start`"));
            }
        }
        let directory = self.recording_dir(&id);
        let captions = std::fs::read_to_string(directory.join("captions.srt")).unwrap_or_default();
        demo.missing_captions = missing_captions(&demo.plan.script, &captions, &demo.checks);
        demo.stage = Stage::Recorded;
        self.save_demo(&demo)?;
        let work = self.demo_dir(&demo.id);
        let events: Value = serde_json::from_slice(&std::fs::read(directory.join("events.json")).context("demo.events: the recording has no events.json")?).context("demo.events: events.json is unreadable")?;
        let events_path = work.join("reel-events.json");
        atomic_write(&events_path, &serde_json::to_vec(&merged_events(&events, &job))?)?;
        let timeline_path = work.join("timeline.json");
        atomic_write(&timeline_path, &serde_json::to_vec(&json!({"title": demo.plan.title, "subtitle": demo.plan.subtitle.clone().unwrap_or_default(), "badge": "QA demo", "url": display_url(&job.url)}))?)?;
        crate::paths::private_dir(&output)?;
        let video = output.join("demo.mp4");
        let map = crate::reel::compose(&self.layout, &directory.join("recording.mp4"), &events_path, &timeline_path, &video).await?;
        demo.polished_seconds = demo.checks.iter().map(|check| check.video_seconds.map(|seconds| crate::reel::map_seconds(&map, seconds))).collect();
        demo.timemap = Some(map.clone());
        demo.narration = None;
        let _ = std::fs::remove_file(output.join(voice::KEEP));
        let mut voice_note = String::new();
        if demo.plan.has_narration() {
            match voice::apply(&self.layout, &demo, &map, &output, &voice::Options { engine: None, voice: None }).await {
                Ok(Some(narration)) => {
                    voice_note = format!("\n{}", voice::summary(&narration));
                    demo.narration = Some(narration);
                }
                Ok(None) => voice_note = "\nNo voice-over was added: every narrated shot was left out because its check did not pass.".to_owned(),
                Err(error) => {
                    let failure = crate::failure::describe(&error);
                    voice_note = format!("\nVoice-over was not added, so the video has music only: {}\nfix: {}", failure.message, failure.fix.unwrap_or_default());
                }
            }
        }
        let extended = demo.narration.as_ref().map_or(0.0, |narration| narration.extended_seconds);
        let markdown = evidence_markdown(&demo, "demo.mp4", map.seconds.unwrap_or(duration) + extended);
        atomic_write(&output.join("evidence.md"), markdown.as_bytes())?;
        demo.stage = Stage::Polished;
        demo.output = Some(output.clone());
        self.save_demo(&demo)?;
        let warning = if demo.missing_captions.is_empty() { String::new() } else { format!("\nWarning: the planned caption never appeared for: {}.", demo.missing_captions.join("; ")) };
        Ok(format!("Demo finished.\nVideo: {}\nEvidence: {}{warning}{voice_note}\n\n{markdown}", video.display(), output.join("evidence.md").display()))
    }
}
