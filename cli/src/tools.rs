use crate::browser::{Session, flag, number, text, truncate};
use crate::failure::{fail, fixable};
use crate::fetch::{FETCH_METHODS, FETCH_READ_LIMIT, NetworkFilter, fetch_report, network_report};
use crate::host::{CALL_TIMEOUT, PopupEvent};
use crate::scripts::{AUX_CLICK_SCRIPT, DOUBLE_CLICK_SCRIPT, DRAG_SCRIPT, FRAME_LIST_SCRIPT, FRAME_OFFSET_SCRIPT, INPUT_SCRIPT, LOOP_CODE_LIMIT, LOOP_SCRIPT, PAGE_FETCH, POINT_SCRIPT, UPLOAD_CHUNK_BYTES, UPLOAD_LIMIT_BYTES};
use anyhow::Result;
use base64::Engine;
use qareel_protocol::{NativeBrowserOperation, NativeCapability, NativeDialogAction, NativeImplementation, NativePointerPhase, NativePopupDecision};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const OUTPUT_LIMIT: usize = 40_000;
const PAGE_MAP_LIMIT: usize = 12_000;
const NETWORK: &str = include_str!("../assets/browser-network.js");
const BATCH_TOOLS: [&str; 18] = ["open", "back", "forward", "reload", "click", "type", "fill", "select", "press", "hover", "wait", "resize", "eval", "scroll", "drag", "upload", "screenshot", "network"];
const BATCH_READS: [&str; 2] = ["console", "network"];

pub fn gamepad_control(key: &str) -> Option<(String, f64)> {
    let lower: String = key.to_ascii_lowercase().chars().filter(char::is_ascii_alphanumeric).collect();
    let rest = lower.strip_prefix("gamepad").or_else(|| lower.strip_prefix("pad"))?;
    let rest = rest.strip_prefix("dpad").unwrap_or(rest);
    for (stick, axes) in [("leftstick", ("lx", "ly")), ("rightstick", ("rx", "ry")), ("lstick", ("lx", "ly")), ("rstick", ("rx", "ry"))] {
        if let Some(direction) = rest.strip_prefix(stick) {
            return match direction {
                "left" => Some((axes.0.to_owned(), -1.0)),
                "right" => Some((axes.0.to_owned(), 1.0)),
                "up" => Some((axes.1.to_owned(), -1.0)),
                "down" => Some((axes.1.to_owned(), 1.0)),
                _ => None,
            };
        }
    }
    matches!(rest, "a" | "b" | "x" | "y" | "lb" | "rb" | "lt" | "rt" | "l1" | "r1" | "l2" | "r2" | "select" | "back" | "view" | "start" | "menu" | "options" | "ls" | "rs" | "l3" | "r3" | "up" | "down" | "left" | "right" | "home" | "guide" | "cross" | "circle" | "square" | "triangle").then(|| (rest.to_owned(), 1.0))
}

fn step_outcome(body: &str) -> &str {
    let lines = || body.lines().map(str::trim).filter(|line| !line.is_empty());
    lines().find(|line| !line.starts_with('#') && !line.starts_with('-')).or_else(|| lines().find(|line| line.starts_with("- URL:"))).unwrap_or("done")
}

fn resolve_path(cwd: &Path, path: &str) -> Result<PathBuf> {
    let requested = path.trim();
    if requested.is_empty() {
        return Err(fixable("browser.path_invalid", "pass a file path", "qareel upload fixtures/avatar.png"));
    }
    let candidate = if Path::new(requested).is_absolute() { PathBuf::from(requested) } else { cwd.join(requested) };
    let resolved = candidate.canonicalize().map_err(|_| fixable("browser.file_missing", format!("{requested} does not exist"), "pass a path relative to the folder you run qareel from"))?;
    if !resolved.is_file() {
        return Err(fail("browser.file_missing", format!("{requested} is not a file")));
    }
    Ok(resolved)
}

fn paths(args: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    keys.iter().find_map(|key| match args.get(*key) {
        Some(Value::Array(values)) => Some(values.iter().filter_map(|value| value.as_str().map(str::to_owned)).collect()),
        Some(Value::String(path)) if !path.trim().is_empty() => Some(vec![path.clone()]),
        _ => None,
    }).unwrap_or_default()
}

fn upload_mime(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt" | "md" | "log") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("zip") => "application/zip",
        Some("mp4") => "video/mp4",
        Some("mp3") => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

impl Session {
    pub(crate) async fn admit_popups(&mut self) -> String {
        let mut notes = String::new();
        for event in self.host.take_popups() {
            match event {
                PopupEvent::Opened { tab, identity, url } => {
                    let decision = NativeBrowserOperation::PopupDecision { instance_id: identity.instance_id, sequence: identity.sequence, popup_id: identity.popup_id, opener_id: identity.opener_id, decision: NativePopupDecision::Admit };
                    match self.host.call(&tab, decision, CALL_TIMEOUT).await {
                        Ok(_) => {
                            let number = self.adopt_tab(&tab, &url);
                            notes.push_str(&format!("- Popup: the page opened a new window as tab {number} ({url}); switch to it with `qareel tabs select {number}`\n"));
                        }
                        Err(error) => notes.push_str(&format!("- Popup: a new window could not be opened as a tab: {error}\n")),
                    }
                }
                PopupEvent::Closed { tab, identity } => {
                    let decision = NativeBrowserOperation::PopupDecision { instance_id: identity.instance_id, sequence: identity.sequence, popup_id: identity.popup_id, opener_id: identity.opener_id, decision: NativePopupDecision::Closed };
                    let _ = self.host.call(&tab, decision, CALL_TIMEOUT).await;
                    if self.forget_tab(&tab) {
                        notes.push_str("- Popup: a window the page opened has closed itself\n");
                    }
                }
            }
        }
        notes
    }

    pub(crate) async fn drag(&mut self, tab: &str, args: &Map<String, Value>) -> Result<String> {
        self.ensure(tab).await?;
        let steps = number(args, "steps").unwrap_or(12.0).clamp(1.0, 60.0) as usize;
        let mut selectors: Vec<Option<String>> = Vec::new();
        let mut points: Vec<Option<(f64, f64)>> = Vec::new();
        for end in ["from", "to"] {
            if let (Some(x), Some(y)) = (number(args, &format!("{end}_x")), number(args, &format!("{end}_y"))) {
                selectors.push(None);
                points.push(Some((x, y)));
                continue;
            }
            let selector = match (text(args, &format!("{end}_selector")), text(args, &format!("{end}_ref"))) {
                (Some(selector), _) => selector,
                (None, Some(reference)) => self.resolve_ref(tab, &reference)?,
                (None, None) => return Err(fixable("args.invalid", format!("pass {end}_ref, {end}_selector or {end}_x and {end}_y"), "qareel drag from_ref=n3 to_ref=n9")),
            };
            selectors.push(Some(selector));
            points.push(None);
        }
        if !self.host.advertises(NativeCapability::PointerInput) {
            if points.iter().any(Option::is_some) {
                return Err(fail("browser.unsupported", "this browser engine cannot hold the pointer, so a drag needs from and to as refs or selectors"));
            }
            let resolved: Vec<String> = selectors.iter().flatten().cloned().collect();
            let (Some(from), Some(to)) = (resolved.first(), resolved.get(1)) else { return Err(fail("args.invalid", "a drag needs a start and an end")) };
            let result = self.on_pair(tab, from, to, &DRAG_SCRIPT.replace("__STEPS__", &steps.to_string())).await?;
            let value = |index: usize| result[index].as_f64().unwrap_or(0.0);
            return Ok(format!("Dragged from ({:.0}, {:.0}) to ({:.0}, {:.0}) with synthetic pointer{} events; pages that require trusted input may not react.\n{}", value(0), value(1), value(2), value(3), if result[4] == true { " and HTML5 drag" } else { "" }, self.summary(tab).await));
        }
        let mut ends = Vec::new();
        for (selector, point) in selectors.iter().zip(&points) {
            ends.push(match (selector, point) {
                (Some(selector), _) => {
                    let (x, y, rect) = self.point(tab, selector).await?;
                    if ends.is_empty() {
                        self.mark(rect);
                    }
                    (x, y)
                }
                (None, Some(point)) => *point,
                (None, None) => return Err(fail("args.invalid", "a drag end has no position")),
            });
        }
        let ((fx, fy), (tx, ty)) = (ends[0], ends[1]);
        self.call(tab, NativeBrowserOperation::Pointer { phase: NativePointerPhase::Move, x: fx, y: fy }).await?;
        self.call(tab, NativeBrowserOperation::Pointer { phase: NativePointerPhase::Down, x: fx, y: fy }).await?;
        let mut moved = Ok(Value::Null);
        for step in 1..=steps {
            let t = step as f64 / steps as f64;
            moved = self.call(tab, NativeBrowserOperation::Pointer { phase: NativePointerPhase::Move, x: fx + (tx - fx) * t, y: fy + (ty - fy) * t }).await;
            if moved.is_err() {
                break;
            }
        }
        let released = self.call(tab, NativeBrowserOperation::Pointer { phase: NativePointerPhase::Up, x: tx, y: ty }).await;
        moved?;
        released?;
        Ok(format!("Dragged from ({fx:.0}, {fy:.0}) to ({tx:.0}, {ty:.0}).\n{}", self.summary(tab).await))
    }

    async fn on_pair(&mut self, tab: &str, from: &str, to: &str, body: &str) -> Result<Value> {
        let script = format!("const source = {}; const target = {}; if (!source) throw new Error('browser.selector_missing: no element matches ' + {}); if (!target) throw new Error('browser.selector_missing: no element matches ' + {}); {body}", crate::browser::page_locate(from), crate::browser::page_locate(to), json!(from), json!(to));
        self.on_document(tab, &script).await
    }

    async fn on_document(&mut self, tab: &str, body: &str) -> Result<Value> {
        self.evaluate(tab, format!("(async () => {{ {body} }})()")).await
    }

    pub(crate) async fn upload(&mut self, tab: &str, args: &Map<String, Value>, cwd: &Path) -> Result<String> {
        let requested = paths(args, &["paths", "path"]);
        if requested.is_empty() || requested.len() > 20 {
            return Err(fixable("args.invalid", "pass one to twenty file paths", "qareel upload fixtures/avatar.png"));
        }
        let mut files: Vec<(String, &'static str, Vec<u8>)> = Vec::new();
        let mut total = 0usize;
        for path in &requested {
            let full = resolve_path(cwd, path)?;
            let bytes = std::fs::read(&full).map_err(|_| fail("browser.upload_unreadable", format!("{path} could not be read")))?;
            total += bytes.len();
            if total > UPLOAD_LIMIT_BYTES {
                return Err(fail("browser.upload_limit", format!("files may total at most {} MiB per call", UPLOAD_LIMIT_BYTES / 1024 / 1024)));
            }
            let name = full.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_else(|| "upload".to_owned());
            let mime = upload_mime(&name);
            files.push((name, mime, bytes));
        }
        let explicit = self.target(tab, args)?.map(|(selector, _)| selector);
        let selector = explicit.clone().unwrap_or_else(|| "input[type=file]".to_owned());
        self.ensure(tab).await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        for (index, (name, mime, bytes)) in files.iter().enumerate() {
            let first = format!("(() => {{ const store = (globalThis.__commissionUpload ||= {{}}); const entry = (store[{id}] ||= []); entry[{index}] = {{name: {name}, type: {mime}, parts: []}}; return true; }})()", id = json!(id), name = json!(name), mime = json!(mime));
            self.evaluate(tab, first).await?;
            for chunk in bytes.chunks(UPLOAD_CHUNK_BYTES) {
                let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
                self.evaluate(tab, format!("(() => {{ globalThis.__commissionUpload[{id}][{index}].parts.push('{encoded}'); return true; }})()", id = json!(id))).await?;
            }
        }
        let body = format!("const store = globalThis.__commissionUpload?.[{id}]; if (!store) throw new Error('browser.upload_lost: the page was replaced while the files were staged; retry'); delete globalThis.__commissionUpload[{id}]; if ({implicit} && document.querySelectorAll('input[type=file]').length !== 1) throw new Error('browser.upload_target: the page has several file inputs; pass selector=... for the one you want'); let input = node; if (!(input instanceof HTMLInputElement && input.type === 'file')) input = (node.control && node.control.type === 'file') ? node.control : (node.querySelector ? node.querySelector('input[type=file]') : null); if (!input) throw new Error('browser.upload_target: that element is not a file input; pass selector=input[type=file]'); if (store.length > 1 && !input.multiple) throw new Error('browser.upload_multiple: this input accepts one file'); const transfer = new DataTransfer(); for (const item of store) {{ const bytes = Uint8Array.from(atob(item.parts.join('')), (character) => character.charCodeAt(0)); transfer.items.add(new File([bytes], item.name, {{type: item.type}})); }} input.files = transfer.files; input.dispatchEvent(new Event('input', {{bubbles: true}})); input.dispatchEvent(new Event('change', {{bubbles: true}})); return Array.from(input.files).map((file) => file.name + ' (' + file.size + ' bytes)');", id = json!(id), implicit = explicit.is_none());
        let done = self.on_element(tab, &selector, &body).await?;
        let listed = done.as_array().map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        Ok(format!("Attached {listed} to the file input by setting its files and firing input and change events.\n{}", self.summary(tab).await))
    }

    fn stage_uploads(&self, files: &[PathBuf]) -> Result<Vec<String>> {
        let uploads = self.layout.profile.join("uploads");
        crate::paths::private_dir(&uploads)?;
        let cutoff = std::time::SystemTime::now() - Duration::from_secs(3600);
        for entry in std::fs::read_dir(&uploads)?.flatten() {
            if entry.metadata().and_then(|meta| meta.modified()).is_ok_and(|modified| modified < cutoff) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let batch = uploads.join(uuid::Uuid::new_v4().simple().to_string());
        crate::paths::private_dir(&batch)?;
        let mut staged = Vec::new();
        for source in files {
            let name = source.file_name().ok_or_else(|| fail("browser.dialog_files_invalid", format!("{} has no file name", source.display())))?;
            let target = batch.join(name);
            std::fs::copy(source, &target).map_err(|_| fail("browser.dialog_files_invalid", format!("could not stage {} for the browser", source.display())))?;
            staged.push(target.to_string_lossy().to_string());
        }
        Ok(staged)
    }

    pub(crate) async fn dialog_note(&mut self, tab: &str) -> String {
        const GENERIC: &str = "- Dialog: the page opened a dialog; answer it with `qareel dialog accept` or `qareel dialog dismiss`\n";
        if !self.host.advertises(NativeCapability::Dialog) {
            return GENERIC.to_owned();
        }
        match self.host.call(tab, NativeBrowserOperation::Dialog { action: NativeDialogAction::Status }, CALL_TIMEOUT).await {
            Ok(value) if value["pending"] == true => {
                let kind = value["kind"].as_str().unwrap_or("page");
                let files = kind.starts_with("files");
                let message = value["message"].as_str().filter(|message| !message.is_empty()).unwrap_or(if files { "Choose files" } else { "" });
                let default = value["default_text"].as_str().filter(|text| !text.is_empty()).map(|text| format!(" (default answer \"{}\")", truncate(text.to_owned(), 80))).unwrap_or_default();
                let how = if kind == "prompt" { ", adding text=... to type an answer" } else if files { ", or `qareel dialog files='[\"report.pdf\"]'` to choose files" } else { "" };
                format!("- Dialog: a {kind} dialog is open: \"{}\"{default}. Answer it with `qareel dialog accept` or `qareel dialog dismiss`{how}; other commands fail until it is answered\n", truncate(message.to_owned(), 300))
            }
            _ => GENERIC.to_owned(),
        }
    }

    pub(crate) async fn dialog(&mut self, tab: &str, args: &Map<String, Value>, cwd: &Path) -> Result<String> {
        self.require(NativeCapability::Dialog).await?;
        self.ensure(tab).await?;
        let chosen = paths(args, &["files", "paths"]);
        let action = if !chosen.is_empty() {
            let resolved = chosen.iter().map(|path| resolve_path(cwd, path)).collect::<Result<Vec<_>>>()?;
            let staged = if self.host.implementation() == Some(NativeImplementation::WpeWebkit) { self.stage_uploads(&resolved)? } else { resolved.iter().map(|path| path.to_string_lossy().to_string()).collect() };
            NativeDialogAction::Files { paths: staged }
        } else {
            match text(args, "action").as_deref().unwrap_or("status") {
                "status" => NativeDialogAction::Status,
                "accept" => NativeDialogAction::Respond { accept: true, text: args.get("text").and_then(Value::as_str).map(str::to_owned) },
                "dismiss" => NativeDialogAction::Respond { accept: false, text: None },
                _ => return Err(fixable("args.invalid", "dialog takes status, accept, dismiss or files=[...]", "qareel dialog accept")),
            }
        };
        let status = matches!(action, NativeDialogAction::Status);
        let value = self.host.call(tab, NativeBrowserOperation::Dialog { action }, CALL_TIMEOUT).await?;
        if status {
            if value["pending"] != true {
                return Ok("No dialog is open in this tab.".to_owned());
            }
            return Ok(self.dialog_note(tab).await);
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let dialog = &value["dialog"];
        Ok(format!("{} the {} dialog \"{}\".\n{}", if value["accepted"] == true { "Accepted" } else { "Dismissed" }, dialog["kind"].as_str().unwrap_or("page"), truncate(dialog["message"].as_str().unwrap_or("").to_owned(), 200), self.summary(tab).await))
    }

    pub(crate) async fn network(&mut self, tab: &str, args: &Map<String, Value>) -> Result<String> {
        self.require(NativeCapability::Evaluate).await?;
        let value = self.evaluate(tab, NETWORK.replace("__MODE__", "\"report\"")).await?;
        let filter = NetworkFilter { failed: flag(args, "failed"), all: flag(args, "all"), url: text(args, "url"), limit: number(args, "limit").map(|limit| limit as usize) };
        Ok(truncate(network_report(&value, &filter), OUTPUT_LIMIT))
    }

    pub(crate) async fn fetch(&mut self, tab: &str, args: &Map<String, Value>) -> Result<String> {
        let url = text(args, "url").ok_or_else(|| fixable("args.invalid", "a URL path is required", "qareel fetch /api/profile"))?;
        if url.starts_with("//") || (!url.starts_with('/') && !url.starts_with("http://") && !url.starts_with("https://")) {
            return Err(fail("browser.fetch_url_invalid", "pass a path that starts with / or a full http(s) URL on the tab's own site"));
        }
        let method = text(args, "method").unwrap_or_else(|| "GET".to_owned()).to_ascii_uppercase();
        if !FETCH_METHODS.contains(&method.as_str()) {
            return Err(fail("browser.fetch_method_invalid", format!("use one of {}", FETCH_METHODS.join(", "))));
        }
        let mut headers = Map::new();
        for (name, value) in args.get("headers").and_then(Value::as_object).into_iter().flatten() {
            let lower = name.trim().to_ascii_lowercase();
            if lower.is_empty() || matches!(lower.as_str(), "cookie" | "cookie2" | "authorization" | "host" | "origin" | "referer" | "set-cookie") || lower.starts_with("proxy-") || lower.starts_with("sec-") {
                return Err(fail("browser.fetch_header_refused", format!("{name} cannot be set; the request already carries the page's own session")));
            }
            let value = value.as_str().ok_or_else(|| fail("args.invalid", format!("header {name} must be a string")))?;
            headers.insert(name.trim().to_owned(), json!(value));
        }
        let body = match args.get("body") {
            None | Some(Value::Null) => Value::Null,
            Some(_) if matches!(method.as_str(), "GET" | "HEAD") => return Err(fail("browser.fetch_body_refused", format!("{method} requests have no body; put parameters in the URL"))),
            Some(Value::String(text)) => json!(text),
            Some(other) => json!(other.to_string()),
        };
        if body.as_str().is_some_and(|text| text.len() > OUTPUT_LIMIT) {
            return Err(fail("browser.input_limit", format!("body exceeds {OUTPUT_LIMIT} bytes")));
        }
        let limit = number(args, "max_chars").map(|limit| (limit as usize).clamp(500, OUTPUT_LIMIT)).unwrap_or(PAGE_MAP_LIMIT);
        self.require(NativeCapability::Evaluate).await?;
        let request = json!({"url": url, "method": method, "headers": headers, "body": body, "limit": FETCH_READ_LIMIT});
        let value = self.evaluate(tab, format!("({PAGE_FETCH})({request})")).await?;
        if !value.is_object() {
            return Err(fail("browser.fetch_failed", "the page returned no response"));
        }
        Ok(fetch_report(&method, &value, limit, flag(args, "raw")))
    }

    pub(crate) async fn batch(&mut self, args: &Map<String, Value>, cwd: &Path) -> Result<String> {
        let steps = args.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
        if steps.is_empty() || steps.len() > 40 {
            return Err(fixable("args.invalid", "batch needs one to forty steps", "qareel batch steps='[{\"tool\":\"click\",\"args\":{\"selector\":\"text/Save\"}},{\"tool\":\"wait\",\"args\":{\"text\":\"Saved\"}}]'"));
        }
        let mut plan: Vec<(String, Map<String, Value>)> = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            let tool = step["tool"].as_str().unwrap_or_default().to_owned();
            let fault = |detail: String| fixable("batch.invalid", format!("step {} ({tool}) is invalid: {detail}; nothing ran", index + 1), "fix that step and run the batch again");
            let raw = step.get("args").cloned().unwrap_or_else(|| json!({}));
            let Value::Object(step_args) = raw else { return Err(fault("args must be a JSON object".to_owned())) };
            let allowed = BATCH_TOOLS.contains(&tool.as_str()) || (tool == "console" && step_args.get("action").and_then(Value::as_str).is_none_or(|action| action == "read")) || (tool == "record" && step_args.get("action").and_then(Value::as_str) == Some("caption"));
            if !allowed {
                return Err(fault("batches run browser actions, console reads and recording captions only".to_owned()));
            }
            if tool != "record" {
                let spec = crate::browser::spec(&tool).ok_or_else(|| fault("unknown command".to_owned()))?;
                crate::args::parse(&spec, &[Value::Object(step_args.clone()).to_string()]).map_err(|error| fault(crate::failure::describe(&error).message))?;
            }
            plan.push((tool, step_args));
        }
        let mut done: Vec<String> = Vec::new();
        let mut extra: Vec<String> = Vec::new();
        for (index, (tool, step_args)) in plan.iter().enumerate() {
            let outcome = if tool == "record" {
                let caption = step_args.get("text").or_else(|| step_args.get("caption")).and_then(Value::as_str).unwrap_or_default().to_owned();
                self.caption(&caption, None).await.map(|caption| format!("Caption shown at {:.1}s.", caption.time_ms as f64 / 1000.0))
            } else {
                Box::pin(self.run(tool, step_args, cwd)).await
            };
            let output = outcome.map_err(|error| fail("batch.step_failed", format!("step {} ({tool}) failed: {}\nCompleted steps:\n{}", index + 1, crate::failure::describe(&error).message, if done.is_empty() { "none".to_owned() } else { done.join("\n") })))?;
            done.push(format!("{}. {tool}: {}", index + 1, truncate(step_outcome(&output).to_owned(), 300)));
            if BATCH_READS.contains(&tool.as_str()) {
                extra.push(format!("### Step {} ({tool})\n{output}", index + 1));
            }
        }
        let tab = self.current_tab()?;
        let finish = match (args.get("snapshot_diff"), args.get("snapshot")) {
            (Some(Value::Bool(true)), _) => Some(json!({"diff": true})),
            (Some(Value::Object(options)), _) => Some(Value::Object(options.clone())).map(|mut options| { options["diff"] = json!(true); options }),
            (_, Some(Value::Bool(true))) => Some(json!({})),
            (_, Some(Value::Object(options))) => Some(Value::Object(options.clone())),
            _ => None,
        };
        let tail = match finish {
            Some(Value::Object(options)) => Box::pin(self.run("snapshot", &options, cwd)).await?,
            _ => self.summary(&tab).await,
        };
        Ok(format!("Completed {} steps:\n{}\n{tail}{}", done.len(), done.join("\n"), extra.iter().map(|block| format!("\n{block}")).collect::<String>()))
    }

    pub(crate) async fn look(&mut self, tab: &str, args: &Map<String, Value>) -> Result<String> {
        self.require(NativeCapability::Screenshot).await?;
        let value = self.call(tab, NativeBrowserOperation::Screenshot).await?;
        let data = value["data"].as_str().ok_or_else(|| fail("browser.screenshot_invalid", "the engine returned no image"))?;
        let png = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| fail("browser.screenshot_invalid", "the page image was not valid"))?;
        let (width, height) = (value["width"].as_f64().unwrap_or(0.0), value["height"].as_f64().unwrap_or(0.0));
        let viewport = (value["viewport_width"].as_f64().filter(|width| *width > 0.0).unwrap_or(width), value["viewport_height"].as_f64().filter(|height| *height > 0.0).unwrap_or(height));
        let region = match (number(args, "x"), number(args, "y"), number(args, "width"), number(args, "height")) {
            (Some(x), Some(y), Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((x, y, w, h)),
            (None, None, None, None) => None,
            _ => return Err(fixable("args.invalid", "a region needs x, y, width and height in CSS pixels", "qareel look x=400 y=200 width=480 height=320")),
        };
        let options = crate::look::LookOptions { columns: number(args, "columns").unwrap_or(64.0).clamp(8.0, 160.0) as u32, region, max_objects: number(args, "max_objects").unwrap_or(24.0).clamp(1.0, 80.0) as usize, grid: args.get("grid") != Some(&Value::Bool(false)) };
        let url = self.host.tab_state(tab).map(|state| state.url).unwrap_or_default();
        let previous = if region.is_none() && args.get("since") != Some(&Value::Bool(false)) { self.looks.get(tab).filter(|(seen, _)| *seen == url).map(|(_, objects)| objects.clone()) } else { None };
        let look = crate::look::describe(&png, viewport, options, previous.as_deref()).map_err(|error| fail("browser.look_failed", format!("{error:#}")))?;
        if region.is_none() {
            self.looks.insert(tab.to_owned(), (url, look.objects.clone()));
        }
        Ok(format!("{}{}", look.text, self.summary(tab).await))
    }

    async fn frame_call(&mut self, tab: &str, frame: &str, script: String) -> Result<Value> {
        if self.host.advertises(NativeCapability::Frames) {
            match self.call(tab, NativeBrowserOperation::FrameEvaluate { frame: frame.to_owned(), script: script.clone() }).await {
                Err(error) if error.to_string().contains("frame_unavailable") => {
                    let elements = self.evaluate(tab, FRAME_LIST_SCRIPT.to_owned()).await.unwrap_or(Value::Null);
                    let source = elements.as_array().into_iter().flatten().find(|entry| entry["name"] == frame).and_then(|entry| entry["src"].as_str()).filter(|source| !source.is_empty()).map(str::to_owned);
                    if let Some(source) = source {
                        return self.call(tab, NativeBrowserOperation::FrameEvaluate { frame: source, script }).await;
                    }
                }
                other => return other,
            }
        }
        let fallback = format!("(async () => {{ const wanted = {}; const frames = [...document.querySelectorAll('iframe,frame')]; const match = frames.find((frame) => frame.name === wanted || frame.id === wanted) || frames.find((frame) => (frame.src || '').includes(wanted)); if (!match) throw new Error('browser.frame_unavailable: no frame matches ' + wanted + '; list them with qareel eval frame=list'); let view; try {{ view = match.contentWindow; void view.document.body; }} catch (error) {{ throw new Error('browser.frame_unavailable: that frame is cross-origin and this browser cannot run code inside it'); }} return await view.eval({}); }})()", json!(frame), json!(script));
        self.evaluate(tab, fallback).await
    }

    async fn frame_offset(&mut self, tab: &str, frame: &str) -> Result<(f64, f64)> {
        let mut url = Value::Null;
        if self.host.advertises(NativeCapability::Frames)
            && let Ok(Value::Array(frames)) = self.call(tab, NativeBrowserOperation::Frames).await
        {
            url = frames.iter().rev().find(|entry| entry["id"] == frame || entry["name"] == frame).or_else(|| frames.iter().rev().find(|entry| entry["url"].as_str().is_some_and(|href| href.contains(frame)))).map(|entry| entry["url"].clone()).unwrap_or(Value::Null);
        }
        let script = FRAME_OFFSET_SCRIPT.replace("__WANTED__", &json!(frame).to_string()).replace("__URL__", &url.to_string());
        let offset = self.evaluate(tab, script).await?;
        Ok((offset[0].as_f64().unwrap_or(0.0), offset[1].as_f64().unwrap_or(0.0)))
    }

    pub(crate) async fn eval_frame(&mut self, tab: &str, frame: &str, args: &Map<String, Value>) -> Result<String> {
        self.require(NativeCapability::Evaluate).await?;
        if frame == "list" {
            let elements = self.evaluate(tab, FRAME_LIST_SCRIPT.to_owned()).await?;
            let documents = if self.host.advertises(NativeCapability::Frames) { self.call(tab, NativeBrowserOperation::Frames).await.unwrap_or(Value::Null) } else { Value::Null };
            return Ok(truncate(serde_json::to_string_pretty(&json!({"frames": elements, "documents": documents, "use": "pass frame=<name, id, or part of its URL> to qareel eval or qareel loop"}))?, OUTPUT_LIMIT));
        }
        let source = args.get("function").and_then(Value::as_str).map(|source| source.trim().trim_end_matches(';').trim().to_owned()).filter(|source| !source.is_empty()).ok_or_else(|| fixable("args.invalid", "a function is required", "qareel eval \"() => document.title\" frame=checkout"))?;
        if source.len() > OUTPUT_LIMIT {
            return Err(fail("browser.input_limit", format!("the function exceeds {OUTPUT_LIMIT} bytes")));
        }
        let value = self.frame_call(tab, frame, format!("(async () => {{ const value = ({source}); return typeof value === 'function' ? await value() : value; }})()")).await?;
        Ok(truncate(serde_json::to_string_pretty(&value)?, OUTPUT_LIMIT))
    }

    pub(crate) async fn run_loop(&mut self, tab: &str, args: &Map<String, Value>) -> Result<String> {
        let action = text(args, "action").unwrap_or_else(|| "read".to_owned());
        let mut request = json!({"action": action, "name": text(args, "name").unwrap_or_else(|| "main".to_owned()), "tail": number(args, "tail").unwrap_or(20.0).clamp(0.0, 200.0) as u64});
        if action == "start" {
            let code = text(args, "code").ok_or_else(|| fixable("args.invalid", "code is required to start a loop: a function such as (api, tick) => { ... }", "qareel loop start code='(api, tick) => tick'"))?;
            if code.len() > LOOP_CODE_LIMIT {
                return Err(fail("browser.input_limit", format!("loop code exceeds {LOOP_CODE_LIMIT} bytes")));
            }
            request["code"] = json!(code);
            request["every"] = match number(args, "every") {
                Some(ms) if (4.0..=5000.0).contains(&ms) => json!(ms as u64),
                _ => json!("frame"),
            };
            request["max_ms"] = json!(number(args, "max_ms").unwrap_or(60_000.0).clamp(1000.0, 600_000.0) as u64);
        } else if !matches!(action.as_str(), "read" | "stop" | "list") {
            return Err(fixable("args.invalid", "loop takes start, read, stop or list", "qareel loop read"));
        }
        self.require(NativeCapability::Evaluate).await?;
        if action == "start" {
            let input = self.host.advertises(NativeCapability::KeyInput).then(|| uuid::Uuid::new_v4().simple().to_string());
            request["input"] = json!(input);
            request["touch"] = json!(if self.host.implementation() == Some(NativeImplementation::WpeWebkit) { "native" } else { "synthetic" });
            let ms = request["max_ms"].as_u64().unwrap_or(60_000) + 2000;
            let _ = self.call(tab, NativeBrowserOperation::Hold { ms: ms as u32, input }).await;
        }
        let frame = text(args, "frame");
        if let Some(frame) = &frame
            && action == "start"
        {
            let (x, y) = self.frame_offset(tab, frame).await?;
            request["offset"] = json!([x, y]);
        }
        let script = LOOP_SCRIPT.replace("__INPUT__", INPUT_SCRIPT).replace("__REQUEST__", &request.to_string());
        let value = match &frame {
            Some(frame) => self.frame_call(tab, frame, script).await?,
            None => self.evaluate(tab, script).await?,
        };
        if action == "stop" {
            let list = LOOP_SCRIPT.replace("__INPUT__", INPUT_SCRIPT).replace("__REQUEST__", &json!({"action": "list", "name": "main"}).to_string());
            let running = self.evaluate(tab, list).await.ok().and_then(|value| value.as_array().map(|loops| loops.iter().any(|entry| entry["running"] == true))).unwrap_or(false);
            if !running {
                let _ = self.call(tab, NativeBrowserOperation::Hold { ms: 0, input: None }).await;
            }
        }
        Ok(truncate(serde_json::to_string(&value)?, OUTPUT_LIMIT))
    }

    pub(crate) async fn aux_click(&mut self, tab: &str, selector: &str, label: &str, button: &str) -> Result<String> {
        let code = if button == "right" { 2 } else { 1 };
        self.on_element(tab, selector, &AUX_CLICK_SCRIPT.replace("__BUTTON__", &code.to_string())).await?;
        Ok(format!("{}-clicked {label} with synthetic events (contextmenu or auxclick); the browser's own context menu does not open.\n{}", if code == 2 { "Right" } else { "Middle" }, self.summary(tab).await))
    }

    pub(crate) async fn double_click_event(&mut self, tab: &str, selector: &str) -> Result<()> {
        self.on_element(tab, selector, DOUBLE_CLICK_SCRIPT).await.map(|_| ())
    }

    pub(crate) async fn synthetic_point(&mut self, tab: &str, x: f64, y: f64, hover: bool, repeats: usize) -> Result<()> {
        let script = POINT_SCRIPT.replace("__X__", &x.to_string()).replace("__Y__", &y.to_string()).replace("__KIND__", &json!(if hover { "move" } else { "click" }).to_string()).replace("__COUNT__", &repeats.to_string());
        self.evaluate(tab, script).await.map(|_| ())
    }

    pub(crate) async fn hover_look(&mut self, tab: &str, dx: f64, dy: f64) -> Result<String> {
        self.require(NativeCapability::Evaluate).await?;
        let locked = self.evaluate(tab, format!("(() => {{ const input = {INPUT_SCRIPT}; input.look({dx}, {dy}); return input.locked(); }})()")).await?;
        let lock = if locked == true { "the page holds pointer lock" } else { "the page does not hold pointer lock; click the game first if it needs it" };
        Ok(format!("Moved the view by {dx:+.0},{dy:+.0} (relative mouse movement; {lock}).\n{}", self.summary(tab).await))
    }

    pub(crate) async fn gamepad(&mut self, tab: &str, key: &str, control: (String, f64), action: &str, hold: Duration) -> Result<String> {
        self.require(NativeCapability::Evaluate).await?;
        let set = |value: f64| {
            let mut state = Map::new();
            state.insert(control.0.clone(), json!(value * control.1));
            format!("(() => {{ const input = {INPUT_SCRIPT}; return input.pad({}, 0); }})()", Value::Object(state))
        };
        if action != "up" {
            self.evaluate(tab, set(1.0)).await?;
        }
        if action == "press" {
            tokio::time::sleep(if hold.is_zero() { Duration::from_millis(120) } else { hold }).await;
        }
        if action != "down" {
            self.evaluate(tab, set(0.0)).await?;
        }
        let what = match action {
            "down" => format!("Holding {key} on virtual gamepad 0; release it with action=up."),
            "up" => format!("Released {key} on virtual gamepad 0."),
            _ => format!("Pressed {key} on virtual gamepad 0."),
        };
        Ok(format!("{what} The page sees a standard gamepad through navigator.getGamepads.\n{}", self.summary(tab).await))
    }
}
