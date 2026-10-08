use crate::failure::fixable;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QaOutcome {
    Passed,
    Failed,
    NotChecked,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QaCheck {
    pub criterion: String,
    pub outcome: QaOutcome,
    pub evidence: String,
    pub video_seconds: Option<u32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QaShotKind {
    Happy,
    Boundary,
    Negative,
    Persistence,
    Regression,
    Permission,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QaSurface {
    Browser,
    Api,
    Cli,
    Data,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QaShot {
    pub criterion: String,
    pub kind: QaShotKind,
    pub surface: QaSurface,
    pub setup: String,
    #[serde(rename = "do")]
    pub action: String,
    pub expect: String,
    pub falsifier: String,
    pub capture: String,
    pub caption: String,
    pub est_seconds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narration: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QaUnverified {
    pub item: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QaScript {
    pub changed_behaviors: Vec<String>,
    pub risks: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    pub shots: Vec<QaShot>,
    #[serde(default)]
    pub not_demonstrable: Vec<QaUnverified>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Voiceover {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outro: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    pub criteria: Vec<String>,
    pub script: QaScript,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voiceover: Option<Voiceover>,
}

const NARRATION_LIMIT: usize = 600;
const WORDS_PER_SECOND: f64 = 2.5;
const SPEECH_PADDING: f64 = 0.4;

pub fn speech_seconds(text: &str) -> f64 {
    let words = text.split_whitespace().count() as f64;
    let pauses = text.chars().filter(|character| matches!(character, '.' | '!' | '?' | ';' | ':')).count() as f64 * 0.25;
    words / WORDS_PER_SECOND + pauses + SPEECH_PADDING
}

fn spoken(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|text| !text.is_empty())
}

impl Plan {
    pub fn shot_narration(&self, index: usize) -> Option<&str> {
        spoken(&self.script.shots.get(index)?.narration)
    }

    pub fn intro_narration(&self) -> Option<&str> {
        spoken(&self.voiceover.as_ref()?.intro)
    }

    pub fn outro_narration(&self) -> Option<&str> {
        spoken(&self.voiceover.as_ref()?.outro)
    }

    pub fn has_narration(&self) -> bool {
        self.intro_narration().is_some() || self.outro_narration().is_some() || (0..self.script.shots.len()).any(|index| self.shot_narration(index).is_some())
    }
}

fn invalid(message: impl Into<String>) -> anyhow::Error {
    fixable("demo.plan_invalid", message, "fix the plan and run `qareel demo plan --file plan.json` again; `qareel guide` shows the format")
}

pub fn validate_plan(plan: &Plan) -> Result<()> {
    if plan.title.trim().is_empty() || plan.title.len() > 120 || plan.subtitle.as_ref().is_some_and(|subtitle| subtitle.len() > 200) {
        return Err(invalid("give a title under 120 bytes (for example the ticket key or change name) and an optional subtitle under 200 bytes"));
    }
    validate_criteria(&plan.criteria)?;
    validate_script(&plan.criteria, &plan.script)?;
    validate_voiceover(plan)
}

fn validate_voiceover(plan: &Plan) -> Result<()> {
    let too_long = |text: &str| text.len() > NARRATION_LIMIT;
    let mut lines: Vec<(&str, &str)> = plan.script.shots.iter().filter_map(|shot| shot.narration.as_deref().map(|text| (shot.caption.as_str(), text))).collect();
    if let Some(voiceover) = &plan.voiceover {
        lines.extend(voiceover.intro.as_deref().map(|text| ("the intro", text)));
        lines.extend(voiceover.outro.as_deref().map(|text| ("the outro", text)));
        if voiceover.voice.as_ref().is_some_and(|voice| voice.trim().is_empty() || voice.len() > 80) {
            return Err(invalid("voiceover.voice names a voice in under 80 bytes, or leave it out for the default"));
        }
    }
    for (place, text) in lines {
        if text.trim().is_empty() || too_long(text) {
            return Err(invalid(format!("narration must be non-empty and under {NARRATION_LIMIT} bytes; check the narration for {place}")));
        }
    }
    if !plan.has_narration() && plan.voiceover.is_some() {
        return Err(invalid("voiceover is set but nothing is narrated; add narration to a shot, an intro or an outro, or remove voiceover"));
    }
    Ok(())
}

pub fn validate_criteria(criteria: &[String]) -> Result<()> {
    if criteria.is_empty() || criteria.len() > 30 || criteria.iter().any(|item| item.trim().is_empty() || item.len() > 1000) {
        return Err(invalid("provide 1 to 30 named acceptance criteria, each under 1000 bytes"));
    }
    let distinct: std::collections::HashSet<_> = criteria.iter().map(|item| item.trim()).collect();
    if distinct.len() != criteria.len() {
        return Err(invalid("criteria must be distinct"));
    }
    Ok(())
}

pub fn validate_script(criteria: &[String], script: &QaScript) -> Result<()> {
    let text = |value: &str, limit: usize| !value.trim().is_empty() && value.len() <= limit;
    if script.changed_behaviors.is_empty() || script.risks.is_empty() {
        return Err(invalid("list the behaviors the change affects and the ways it could be wrong before planning shots"));
    }
    if script.changed_behaviors.len() > 30
        || script.risks.len() > 30
        || script.assumptions.len() > 30
        || script.not_demonstrable.len() > 30
        || script.changed_behaviors.iter().chain(&script.risks).chain(&script.assumptions).any(|item| !text(item, 1000))
        || script.not_demonstrable.iter().any(|item| !text(&item.item, 1000) || !text(&item.reason, 1000))
    {
        return Err(invalid("behaviors, risks, assumptions and not_demonstrable take at most 30 non-empty entries of 1000 bytes each"));
    }
    if script.shots.is_empty() || script.shots.len() > 40 {
        return Err(invalid("provide 1 to 40 shots"));
    }
    let mut seconds = 0u32;
    for shot in &script.shots {
        if !criteria.iter().any(|criterion| criterion.trim() == shot.criterion.trim()) {
            return Err(invalid(format!("a shot names a criterion that is not in the checklist: {}", shot.criterion)));
        }
        let fields = [&shot.setup, &shot.action, &shot.expect, &shot.falsifier, &shot.capture];
        if fields.into_iter().any(|value| !text(value, 2000)) || !text(&shot.caption, 160) {
            return Err(invalid(format!("every shot needs setup, do, expect, falsifier and capture (each under 2000 bytes) and a caption under 160 bytes: {}", shot.criterion)));
        }
        if shot.expect.trim().eq_ignore_ascii_case(shot.falsifier.trim()) {
            return Err(invalid(format!("a falsifier must describe what you would see if the change were broken, not repeat the expectation: {}", shot.criterion)));
        }
        if !(3..=120).contains(&shot.est_seconds) {
            return Err(invalid(format!("est_seconds must be between 3 and 120: {}", shot.criterion)));
        }
        seconds += shot.est_seconds;
    }
    if seconds > 300 {
        return Err(invalid(format!("the shots add up to {seconds} seconds; the recording is limited to 300")));
    }
    for criterion in criteria {
        let shown = script.shots.iter().any(|shot| shot.criterion.trim() == criterion.trim());
        let parked = script.not_demonstrable.iter().any(|item| item.item.trim() == criterion.trim());
        if !shown && !parked {
            return Err(invalid(format!("every criterion needs a shot, or an exact entry in not_demonstrable with its reason; missing: {criterion}")));
        }
    }
    if script.shots.len() >= 3 && script.shots.iter().all(|shot| shot.kind == QaShotKind::Happy) {
        return Err(invalid("a script of only happy-path shots cannot fail; add boundary, negative, persistence or permission shots"));
    }
    Ok(())
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn missing_captions(script: &QaScript, captions: &str, checks: &[QaCheck]) -> Vec<String> {
    let seen = squash(captions);
    let mut missing = Vec::new();
    for shot in &script.shots {
        let ran = checks.iter().any(|check| check.criterion.trim() == shot.criterion.trim() && check.outcome != QaOutcome::NotChecked);
        let caption = squash(&shot.caption);
        if ran && !seen.contains(&caption) && !missing.contains(&caption) {
            missing.push(caption);
        }
    }
    missing
}

pub fn validate_checks(checks: &[QaCheck], duration_seconds: f64) -> Result<()> {
    for check in checks {
        if check.evidence.trim().is_empty() || check.evidence.len() > 4000 {
            return Err(fixable("demo.evidence", format!("give observed evidence under 4000 bytes for: {}", check.criterion), "qareel demo check N passed \"expected X, observed X\""));
        }
        match check.video_seconds {
            None if check.outcome == QaOutcome::Passed => return Err(fixable("demo.timestamp", format!("a passed check has no time in the recording: {}", check.criterion), "report it again with `qareel demo check` while the recording is running")),
            Some(second) if f64::from(second) > duration_seconds || second > 300 => return Err(fixable("demo.timestamp", format!("a check timestamp is outside the recorded video: {}", check.criterion), "report the check again during the recording, or record again")),
            _ => {}
        }
    }
    Ok(())
}
