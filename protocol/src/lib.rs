use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativePresentation { EmbeddedMac, Stream, None }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeImplementation { AppkitWebkit, WpeWebkit }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeCapability { Core, Snapshot, Evaluate, ReferenceInput, Screenshot, Automation, Console, Popups, RecordingVideo, RecordingAudioApp, StreamInput, PointerInput, PointerTap, Dialog, Cookies, KeyInput, WheelInput, Frames }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeDialogAction {
    Status,
    Respond { accept: bool, #[serde(default)] text: Option<String> },
    Files { paths: Vec<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub session: bool,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub expires: Option<f64>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub same_site: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeCookieKey {
    pub name: String,
    pub domain: String,
    pub path: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativePointerPhase { Down, Move, Up }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeKeyPhase { Down, Up }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NativeCapabilities {
    pub version: u16,
    pub implementation: NativeImplementation,
    pub presentation: NativePresentation,
    pub operations: Vec<NativeCapability>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeAutomationBinding {
    pub generation: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub navigation_epoch: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub control_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeConsoleAction {
    Read,
    Configure { binding: NativeAutomationBinding, enabled: bool },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "operation", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NativeAutomationAction {
    Click { target: String },
    TypeText { target: String, text: String },
    Select { target: String, option: String },
    ScrollUp,
    ScrollDown,
    Back,
    Reload,
    Wait,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativePopupIdentity {
    pub instance_id: String,
    pub sequence: u64,
    pub opener_id: String,
    pub popup_id: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativePopupDecision {
    Admit,
    Reject,
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeRecordingScope {
    Local,
    Origins { origins: Vec<String> },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeRecordingControlPolicy { Agent, User }

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeRecordingAudio { #[default] Off, App, Microphone, AppAndMicrophone }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeRecordingOverlays {
    pub cursor: bool,
    pub clicks: bool,
    pub captions: bool,
    pub highlights: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeRecordingOptions {
    pub scope: NativeRecordingScope,
    pub fps: u16,
    pub max_duration_ms: u32,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub max_bytes: u64,
    pub max_dimension: u16,
    pub control_policy: NativeRecordingControlPolicy,
    pub audio: NativeRecordingAudio,
    pub overlays: NativeRecordingOverlays,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeRecordingPhase { Starting, Recording, Stopping, Complete, Interrupted, Failed }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeRecordingAudioStatus { Disabled, Pending, Captured, Failed }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NativeRecordingArtifactKind { Video, CaptionsSrt, CaptionsVtt, Events }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeRecordingArtifact {
    pub kind: NativeRecordingArtifactKind,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeRecordingStatus {
    pub recording_id: String,
    pub phase: NativeRecordingPhase,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub started_at_unix_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub duration_ms: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub frames: u64,
    pub audio: NativeRecordingAudio,
    pub audio_status: NativeRecordingAudioStatus,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub audio_gap_ms: u64,
    pub artifacts: Vec<NativeRecordingArtifact>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeRecordingChunk {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub offset: u64,
    pub data_base64: String,
    pub eof: bool,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeBrowserOperation {
    Console { action: NativeConsoleAction },
    RecordingStart { recording_id: String, options: NativeRecordingOptions },
    RecordingCaption { recording_id: String, caption_id: String, text: String },
    RecordingStop { recording_id: String },
    RecordingStatus { recording_id: String },
    RecordingRead { recording_id: String, artifact: NativeRecordingArtifactKind, #[cfg_attr(feature = "ts", ts(type = "number"))] offset: u64, max_bytes: u32 },
    RecordingRelease { recording_id: String, artifacts: Vec<NativeRecordingArtifact> },
    PopupDecision { instance_id: String, sequence: u64, popup_id: String, opener_id: String, decision: NativePopupDecision },
    Ensure { workspace_id: String, profile_id: String, url: String },
    Navigate { url: String },
    Back,
    Forward,
    Reload,
    Close,
    Snapshot,
    Evaluate { script: String },
    Click { reference: String },
    Type { text: String, reference: Option<String> },
    Press { key: String },
    Pointer { phase: NativePointerPhase, x: f64, y: f64 },
    Tap { x: f64, y: f64 },
    Hold { ms: u32, #[serde(default, skip_serializing_if = "Option::is_none")] input: Option<String> },
    Key { phase: NativeKeyPhase, key: String },
    Wheel { x: f64, y: f64, dx: f64, dy: f64, #[serde(default)] zoom: bool },
    Frames,
    FrameEvaluate { frame: String, script: String },
    Dialog { action: NativeDialogAction },
    CookiesExport { offset: u32, limit: u32 },
    CookiesApply { add: Vec<NativeCookie>, remove: Vec<NativeCookieKey> },
    Screenshot,
    Control { human: bool },
    Resize { width: Option<u32>, height: Option<u32> },
    AutomationObserve { bootstrap: String },
    AutomationExecute { binding: NativeAutomationBinding, token: String, action: NativeAutomationAction },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeBrowserMessage {
    Hello { generation: String, #[serde(default, skip_serializing_if = "Vec::is_empty")] features: Vec<String> },
    Flushed { generation: String, id: String },
    Command { generation: String, id: String, tab_id: String, agent: bool, control_epoch: u64, deadline_ms: u64, operation: NativeBrowserOperation },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeBrowserOutcome {
    Ok { value: serde_json::Value },
    Error { message: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NativeTabState {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub control_epoch: u64,
    pub human: bool,
    #[serde(default)]
    pub attention: Option<String>,
    #[serde(default)]
    pub attention_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeBrowserHostMessage {
    Flush { generation: String, id: String },
    Ready { generation: String, instance_id: String, #[serde(default)] capabilities: Option<NativeCapabilities> },
    Popup { generation: String, instance_id: String, sequence: u64, opener_id: String, tab_id: String, popup_id: String, state: NativeTabState },
    PopupClosed { generation: String, instance_id: String, sequence: u64, opener_id: String, tab_id: String, popup_id: String },
    Session { generation: String, profile_id: String, error: Option<String> },
    Reply { generation: String, id: String, outcome: NativeBrowserOutcome },
    Tab { generation: String, tab_id: String, state: NativeTabState },
    Takeover { generation: String, tab_id: String, control_epoch: u64 },
}
