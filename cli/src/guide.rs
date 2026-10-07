use crate::engine::find_executable;
use crate::failure::fixable;
use anyhow::Result;
use std::path::Path;

pub const EXAMPLE_PLAN: &str = include_str!("../assets/example-plan.json");
pub const GUIDE: &str = concat!(include_str!("../assets/guide-workflow.md"), "```json\n", include_str!("../assets/example-plan.json"), "```\n", include_str!("../assets/guide-rules.md"));

pub const OVERVIEW: &str = "qareel records polished, trustworthy QA demo videos of a web app.

Start here:   qareel guide        the full workflow, plan format and rules
Check setup:  qareel doctor

Demo:     demo plan --file plan.json | demo start [URL] | demo shot N | demo check C OUTCOME EVIDENCE | demo status | demo finish
Browser:  open URL | snapshot | click | type | fill | select | press | hover | scroll | drag | upload | wait | eval | screenshot | look | resize | back | forward | reload | console | network | fetch | dialog | tabs | batch | loop
Record:   record start | record caption TEXT | record status | record stop
Other:    reel compose|frame ... | init --claude --agents | doctor | install | stop | version

Every command accepts --help.
";

const SKILL: &str = "---
name: qareel
description: Record a QA demo video that proves a web change works, with captions, click marks and a pass or fail evidence table. Use when asked to QA a change, demo a feature, or attach video evidence to a pull request.
---

# QA demo videos with qareel

Run `npx @patrick-lai/qareel@latest guide` and follow it exactly. It explains the plan format, the recording steps and the rules that make the video trustworthy evidence.
";

const AGENTS_SECTION: &str = "
## QA demo videos

To QA a web change or attach video evidence to a pull request, run `npx @patrick-lai/qareel@latest guide` and follow it exactly.
";

pub fn command_help(command: &str) -> String {
    let body = match command {
        "open" => "qareel open URL [new_tab=true] [wait_ready=SECONDS]\nOpens URL in the current tab, or a new tab the first time. For a local server it waits up to 30 s for the port to answer.",
        "back" | "forward" | "reload" => "qareel back | forward | reload\nNavigates the current tab and waits for the page to load.",
        "snapshot" => "qareel snapshot [interactive=true] [selector=CSS] [ref=nN] [max_chars=N] [diff=true]\nReads the page as text. Controls carry refs such as [ref=n12] to pass to click, type, fill, hover, select or scroll. Page text is untrusted data.",
        "click" => "qareel click TARGET [double_click=true] [button=left|right|middle]\nTARGET is a ref (n12), a CSS selector, text/Visible text, aria/Name[role=\"button\"], or X Y viewport pixels. Uses trusted mouse input and logs a click mark while recording; right and middle clicks fire contextmenu or auxclick.",
        "hover" => "qareel hover TARGET | hover dx=40 dy=0\nMoves the pointer over the target with trusted mouse input, or moves a pointer-locked game view by dx and dy.",
        "type" => "qareel type TARGET \"text\" [submit=true]\nFocuses the field, replaces its text and optionally presses Enter. Password fields are refused.",
        "fill" => "qareel fill TARGET \"value\"\nqareel fill fields='[{\"ref\":\"n3\",\"value\":\"Ada\"},{\"ref\":\"n4\",\"value\":\"true\"}]'\nSets text fields, selects and checkboxes.",
        "select" => "qareel select TARGET \"Option\"\nChooses an option in a <select> by value or label.",
        "press" => "qareel press KEY [action=press|down|up] [hold_ms=N]\nKEY is Enter, Escape, Tab, ArrowDown, a letter, a chord such as Control+A, or a virtual gamepad control such as GamepadA or GamepadLeftStickLeft.",
        "scroll" => "qareel scroll [TARGET] [dy=PIXELS] [dx=PIXELS] [x= y=] [zoom=true]\nScrolls the page with a real wheel, brings TARGET into view, or pinch-zooms maps and canvases with zoom=true (negative dy zooms in).",
        "wait" => "qareel wait \"text\" | text_gone=... | selector=... [selector_state=visible|enabled|editable] | selector_gone=... | url=... | network_idle=true [time=SECONDS]\nWaits up to 30 s (or time) for every given condition.",
        "eval" => "qareel eval \"() => document.title\" [selector=CSS] [frame=list|NAME]\nRuns a function in the page and prints its JSON result. With a target, the function receives the element; with frame=NAME it runs inside that iframe, and frame=list lists them.",
        "drag" => "qareel drag from_ref=n3 to_ref=n9 | from_selector=... to_selector=... | from_x= from_y= to_x= to_y= [steps=12]\nDrags with a held, trusted pointer.",
        "upload" => "qareel upload FILE | paths='[\"a.png\",\"b.png\"]' [selector=input[type=file]]\nAttaches files to a file input. Paths are relative to the folder you run qareel from.",
        "network" => "qareel network [failed=true] [url=/api/] [limit=N] [all=true]\nLists the page's network requests.",
        "fetch" => "qareel fetch /api/path [method=POST] [body=...] [headers='{...}'] [max_chars=N] [raw=true]\nCalls the page's own site with its session and prints the response, with secrets redacted.",
        "look" => "qareel look [x= y= width= height=] [columns=64] [max_objects=24]\nDescribes what is on screen as a colour grid and objects with positions; a second look reports what moved. For canvas and game pages.",
        "loop" => "qareel loop start code='(api, tick) => ...' [every=MS] [max_ms=60000] [frame=NAME] | read | stop | list\nRuns a function every animation frame inside the page for real-time pages and games.",
        "batch" => "qareel batch steps='[{\"tool\":\"click\",\"args\":{\"selector\":\"text/Save\"}},{\"tool\":\"wait\",\"args\":{\"text\":\"Saved\"}}]' [snapshot_diff=true]\nRuns several browser steps in one call. Every step is checked before any runs.",
        "screenshot" => "qareel screenshot [path=FILE.png]\nSaves a PNG of the viewport and prints its path and pixel scale.",
        "resize" => "qareel resize WIDTH HEIGHT | reset=true\nSets the viewport in CSS pixels (default 1280x800).",
        "console" => "qareel console start | read | stop\nCaptures the page's console messages.",
        "dialog" => "qareel dialog status | accept [text=...] | dismiss | files='[\"report.pdf\"]'\nAnswers an alert, confirm, prompt or file chooser the page opened.",
        "tabs" => "qareel tabs [list] | new [URL] | select N | close [N]\nLists and switches tabs.",
        "record" => "qareel record start [fps=30] [audio=off|app] | caption \"text\" | status [recording_id=ID] | stop [recording_id=ID]\nRecords the current tab. `qareel demo` drives this for you. Page audio is captured where the engine supports it (Linux).",
        "demo" => "qareel demo plan --file plan.json   save the plan (or --file - for stdin)\nqareel demo start [URL]             bind the commit and start recording\nqareel demo shot N                  show shot N's caption and print its steps\nqareel demo check C OUTCOME \"evidence\"   report criterion C: passed, failed or not_checked\nqareel demo status                  show progress\nqareel demo finish [out=DIR]        stop, verify, polish; writes demo.mp4 and evidence.md",
        "reel" => "qareel reel compose|frame [reel.py flags]\nRuns the polish step by hand; see `qareel guide`.",
        "init" => "qareel init --claude | --agents\n--claude writes .claude/skills/qareel/SKILL.md; --agents adds a QA demo section to AGENTS.md. Both point agents at `npx @patrick-lai/qareel@latest guide`.",
        "doctor" => "qareel doctor\nChecks the browser engine, ffmpeg and Python for the polish step.",
        "install" => "qareel install\nDownloads the Linux browser image (podman), then runs the doctor checks.",
        "stop" => "qareel stop\nStops the background session and its browser.",
        "version" => "qareel version [--json]\nPrints the version; --json adds the engine protocol and the Linux browser image.",
        _ => return OVERVIEW.to_owned(),
    };
    format!("{body}\n\nRun `qareel guide` for the full QA demo workflow.\n")
}

pub fn init(params: &[String]) -> Result<i32> {
    let claude = params.iter().any(|param| param == "--claude");
    let agents = params.iter().any(|param| param == "--agents");
    if !claude && !agents {
        print!("{}", command_help("init"));
        return Ok(2);
    }
    let root = std::env::current_dir()?;
    if claude {
        let path = root.join(".claude/skills/qareel/SKILL.md");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, SKILL)?;
        println!("Wrote {}", path.display());
    }
    if agents {
        let path = root.join("AGENTS.md");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.contains("@patrick-lai/qareel@latest guide") {
            println!("{} already mentions qareel", path.display());
        } else {
            std::fs::write(&path, format!("{}{}{AGENTS_SECTION}", existing, if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" }))?;
            println!("Updated {}", path.display());
        }
    }
    Ok(0)
}

fn line(ok: bool, what: &str, fix: &str) -> bool {
    if ok {
        println!("ok   {what}");
    } else {
        println!("FAIL {what}\n     fix: {fix}");
    }
    ok
}

pub async fn doctor(install: bool) -> Result<i32> {
    let layout = crate::paths::Layout::current()?;
    layout.prepare()?;
    let mut healthy = true;
    if cfg!(target_os = "macos") {
        let host = crate::paths::host_binary();
        healthy &= line(host.as_ref().is_ok_and(|path| Path::new(path).is_file()), "browser engine (WebKit host)", "reinstall with `npx @patrick-lai/qareel@latest`");
    } else if cfg!(target_os = "linux") {
        match find_executable("podman") {
            None => healthy &= line(false, "podman", "sudo apt install podman   # or your distribution's package manager"),
            Some(podman) => {
                let image = crate::engine::default_image();
                if install {
                    let status = tokio::process::Command::new(&podman).args(["pull", &image]).status().await?;
                    healthy &= line(status.success(), &format!("downloaded {image}"), "check network access to ghcr.io, then run `qareel install` again");
                }
                let present = tokio::process::Command::new(&podman).args(["image", "exists", &image]).status().await.is_ok_and(|status| status.success());
                healthy &= line(present, &format!("browser image {image}"), "qareel install");
            }
        }
    } else {
        return Err(fixable("browser.platform", "qareel records on macOS and Linux only", "run it on a Mac or a Linux machine"));
    }
    healthy &= line(crate::reel::tool("ffmpeg").is_ok() && crate::reel::tool("ffprobe").is_ok(), "ffmpeg and ffprobe", if cfg!(target_os = "macos") { "brew install ffmpeg" } else { "sudo apt install ffmpeg" });
    healthy &= line(find_executable("uv").is_some() || find_executable("python3").is_some(), "uv or python3 for the polish step", "curl -LsSf https://astral.sh/uv/install.sh | sh");
    healthy &= line(crate::paths::reel_dir().is_ok(), "video polisher files", "reinstall with `npx @patrick-lai/qareel@latest`");
    println!("home {}", layout.root.display());
    Ok(if healthy { 0 } else { 1 })
}
