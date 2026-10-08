use crate::paths::Layout;
use crate::script::{Plan, speech_seconds};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_AFK_SECONDS: u64 = 300;
pub const MAX_AFK_SECONDS: u64 = 86_400;
const DEFAULT_WAIT_SECONDS: u64 = 45;
const MAX_WAIT_SECONDS: u64 = 100;
const OVERRUN_ALLOWANCE: f64 = 0.9;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    pub by: String,
    pub at: u64,
    pub auto: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Review {
    pub plan_hash: String,
    pub shown_at: u64,
    pub afk_seconds: u64,
    pub revisions: u32,
    pub approval: Option<Approval>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    NotRequired,
    NotShown,
    Approved(Approval),
    Waiting { left: Option<u64> },
    Expired,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

pub fn plan_hash(plan: &Plan) -> String {
    let bytes = serde_json::to_vec(plan).unwrap_or_default();
    crate::paths::hex(&Sha256::digest(bytes))
}

pub fn default_afk() -> u64 {
    std::env::var("QAREEL_AFK_SECONDS").ok().and_then(|value| value.trim().parse::<u64>().ok()).map_or(DEFAULT_AFK_SECONDS, |seconds| seconds.min(MAX_AFK_SECONDS))
}

impl Review {
    pub fn new(plan: &Plan, afk_seconds: u64, revisions: u32, now: u64) -> Self {
        Self { plan_hash: plan_hash(plan), shown_at: now, afk_seconds: afk_seconds.min(MAX_AFK_SECONDS), revisions, approval: None }
    }

    pub fn deadline(&self) -> Option<u64> {
        (self.afk_seconds > 0).then(|| self.shown_at + self.afk_seconds)
    }
}

pub fn decide(plan: &Plan, review: Option<&Review>, now: u64) -> Decision {
    if !plan.has_narration() {
        return Decision::NotRequired;
    }
    let Some(review) = review.filter(|review| review.plan_hash == plan_hash(plan)) else { return Decision::NotShown };
    if let Some(approval) = &review.approval {
        return Decision::Approved(approval.clone());
    }
    match review.deadline() {
        None => Decision::Waiting { left: None },
        Some(deadline) if now >= deadline => Decision::Expired,
        Some(deadline) => Decision::Waiting { left: Some(deadline - now) },
    }
}

pub fn auto_approval(review: &Review, now: u64) -> Approval {
    Approval { by: format!("qareel, because nobody replied within {}", span(review.afk_seconds)), at: now, auto: true }
}

pub fn span(seconds: u64) -> String {
    match seconds {
        0..=89 => format!("{seconds}s"),
        90..=5399 => format!("{} min", (seconds + 30) / 60),
        _ => format!("{:.1} h", seconds as f64 / 3600.0),
    }
}

pub fn describe_approval(approval: &Approval) -> String {
    if approval.auto { format!("approved automatically by {}", approval.by) } else { format!("approved by {}", approval.by) }
}

fn clock(seconds: f64) -> String {
    let whole = seconds.round().max(0.0) as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

fn quote(text: &str) -> String {
    format!("> 🎙 {}", text.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub fn warnings(plan: &Plan) -> Vec<String> {
    let mut found = Vec::new();
    for (index, shot) in plan.script.shots.iter().enumerate() {
        let Some(text) = plan.shot_narration(index) else { continue };
        let needs = speech_seconds(text);
        if needs > f64::from(shot.est_seconds) * OVERRUN_ALLOWANCE {
            found.push(format!("Shot {} narration takes about {needs:.0}s to say but the shot is planned for {}s. Shorten the narration or raise est_seconds, otherwise the voice runs into the next shot.", index + 1, shot.est_seconds));
        }
    }
    found
}

pub fn preview(plan: &Plan, review: &Review, now: u64) -> String {
    let mut out = format!("# Script review: {}\n\n", plan.title);
    if let Some(subtitle) = plan.subtitle.as_deref().filter(|text| !text.trim().is_empty()) {
        out.push_str(&format!("_{subtitle}_\n\n"));
    }
    out.push_str(&match decide(plan, Some(review), now) {
        Decision::Approved(approval) => format!("**Status: {}.**\n\n", describe_approval(&approval)),
        Decision::Waiting { left: Some(left) } => format!("**Status: waiting for your OK.** Reply with changes, or say it looks good. If nobody replies, this script is approved automatically in {}.\n\n", span(left)),
        Decision::Waiting { left: None } => "**Status: waiting for your OK.** Reply with changes, or say it looks good. It will not start until you do.\n\n".to_owned(),
        Decision::Expired | Decision::NotShown | Decision::NotRequired => "**Status: waiting for your OK.**\n\n".to_owned(),
    });
    let on_camera: u32 = plan.script.shots.iter().map(|shot| shot.est_seconds).sum();
    let voice = plan.voiceover.as_ref().and_then(|voiceover| voiceover.voice.as_deref()).unwrap_or("the best voice on this machine");
    out.push_str(&format!("About **{}** on camera, plus the title and end cards. Voice: {voice}. Music: a soft ambient bed that dips under the voice. Revision {}.\n\n", clock(f64::from(on_camera)), review.revisions + 1));
    let problems = warnings(plan);
    if !problems.is_empty() {
        out.push_str("### Timing to fix\n\n");
        for problem in &problems {
            out.push_str(&format!("- ⚠️ {problem}\n"));
        }
        out.push('\n');
    }
    out.push_str("## Intro (title card)\n\n");
    out.push_str(&plan.intro_narration().map_or("_No voice-over._".to_owned(), quote));
    out.push_str("\n\n");
    for (index, shot) in plan.script.shots.iter().enumerate() {
        out.push_str(&format!("## Shot {}: {}\n\n", index + 1, shot.caption));
        out.push_str(&format!("- Proves: {}\n- Do: {}\n- Expect: {}\n- Broken if: {}\n- Planned length: {}s\n\n", shot.criterion, shot.action, shot.expect, shot.falsifier, shot.est_seconds));
        match plan.shot_narration(index) {
            Some(text) => out.push_str(&format!("{}\n\n_About {:.0}s spoken._\n\n", quote(text), speech_seconds(text))),
            None => out.push_str("_No voice-over: this shot is silent apart from the music._\n\n"),
        }
    }
    out.push_str("## Outro\n\n");
    out.push_str(&plan.outro_narration().map_or("_No voice-over._".to_owned(), quote));
    out.push_str("\n\n");
    if !plan.script.not_demonstrable.is_empty() {
        out.push_str("## Not shown\n\n");
        for item in &plan.script.not_demonstrable {
            out.push_str(&format!("- {}: {}\n", item.item, item.reason));
        }
        out.push('\n');
    }
    out.push_str("---\n\nVoice-over is written before recording, so it describes what is being checked and never claims a result. If a check fails or is not run, its narration is left out of the video.\n");
    out
}

pub fn wait_budget(params: &[String]) -> Duration {
    let seconds = params.iter().skip(1).filter_map(|param| param.trim_start_matches("seconds=").trim().parse::<u64>().ok()).next().unwrap_or(DEFAULT_WAIT_SECONDS);
    Duration::from_secs(seconds.clamp(1, MAX_WAIT_SECONDS))
}

pub fn waiting_text(left: Option<u64>) -> String {
    let timing = match left {
        Some(left) => format!("It is approved automatically in {} if nobody replies.", span(left)),
        None => "Auto-approval is off, so it keeps waiting.".to_owned(),
    };
    format!("Still waiting for the person's OK on the script. {timing}\nIf they replied: OK means `qareel demo approve`; changes mean `qareel demo revise --file plan.json`, then `qareel demo wait` again. If they have not replied, run `qareel demo wait` again. Do not start recording and do not approve for them.")
}

pub async fn wait(layout: &Layout, params: &[String]) -> anyhow::Result<(String, i32)> {
    let budget = wait_budget(params);
    let started = std::time::Instant::now();
    loop {
        let demo = crate::demo::load_current(layout)?;
        match decide(&demo.plan, demo.review.as_ref(), now()) {
            Decision::NotRequired => return Err(crate::failure::fixable("demo.no_narration", "this plan has no voice-over, so there is no script to wait for", "run `qareel demo start`")),
            Decision::NotShown => return Err(crate::failure::fixable("demo.review_needed", "the script has not been shown yet", "run `qareel demo script`, show it to the person, then `qareel demo wait`")),
            Decision::Approved(approval) => return Ok((format!("Script {}. Next: start the app, open the first page, then `qareel demo start`.", describe_approval(&approval)), 0)),
            Decision::Expired => {
                let request = crate::serve::Request { version: env!("CARGO_PKG_VERSION").to_owned(), command: "demo".to_owned(), params: vec!["approve".to_owned(), "auto=true".to_owned()], input: None, cwd: std::env::current_dir()? };
                return match crate::client::send(&request, true).await? {
                    Some(crate::serve::Response::Ok { text }) => Ok((text, 0)),
                    Some(crate::serve::Response::Error { code, message, fix }) => Err(crate::failure::Failure { code, message, fix }.into()),
                    None => Err(crate::failure::fail("serve.unavailable", "the qareel session is not running")),
                };
            }
            Decision::Waiting { left } => {
                if started.elapsed() >= budget {
                    return Ok((waiting_text(left), 0));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
