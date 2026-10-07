use crate::engine::find_executable;
use crate::failure::{fail, fixable};
use crate::paths::Layout;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

const COMPOSE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimeMap {
    pub fps: u32,
    pub intro_seconds: f64,
    pub lead_gap: f64,
    pub holds: Vec<(u64, u64)>,
    #[serde(default)]
    pub seconds: Option<f64>,
}

pub fn map_seconds(map: &TimeMap, seconds: u32) -> u32 {
    let fps = u64::from(map.fps.max(1));
    let lead = (map.lead_gap * fps as f64).round() as u64;
    let frame = (u64::from(seconds) * fps).saturating_sub(lead);
    let held: u64 = map.holds.iter().filter(|(at, _)| *at < frame).map(|(_, count)| count).sum();
    ((frame + held) as f64 / fps as f64 + map.intro_seconds).floor() as u32
}

fn install_hint(tool: &str) -> String {
    match (tool, cfg!(target_os = "macos")) {
        ("ffmpeg", true) => "brew install ffmpeg".to_owned(),
        ("ffmpeg", false) => "sudo apt install ffmpeg   # or your distribution's package manager".to_owned(),
        (_, true) => "brew install uv   # or: curl -LsSf https://astral.sh/uv/install.sh | sh".to_owned(),
        (_, false) => "curl -LsSf https://astral.sh/uv/install.sh | sh".to_owned(),
    }
}

pub fn tool(name: &str) -> Result<PathBuf> {
    let variable = format!("QAREEL_{}", name.to_uppercase());
    if let Some(path) = std::env::var_os(&variable).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    find_executable(name).ok_or_else(|| fixable("reel.ffmpeg_missing", format!("polishing the video needs {name}, which is not on PATH"), install_hint("ffmpeg")))
}

async fn run(program: &Path, args: &[String], limit: Duration) -> Result<std::process::Output> {
    let mut command = tokio::process::Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let child = command.spawn().with_context(|| format!("reel.start: could not run {}", program.display()))?;
    tokio::time::timeout(limit, child.wait_with_output()).await.map_err(|_| fail("reel.timeout", format!("{} did not finish within {} minutes", program.display(), limit.as_secs() / 60)))?.map_err(Into::into)
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(6)..].join("\n").chars().take(1500).collect()
}

pub async fn python(layout: &Layout) -> Result<(PathBuf, Vec<String>)> {
    if let Some(uv) = find_executable("uv") {
        return Ok((uv, ["run", "--quiet", "--no-project", "--with", "numpy", "--with", "pillow>=9.5", "python"].map(String::from).to_vec()));
    }
    let venv = layout.root.join("venv");
    let interpreter = venv.join("bin/python");
    let ready = venv.join(".ready");
    if ready.is_file() && interpreter.is_file() {
        return Ok((interpreter, Vec::new()));
    }
    let system = find_executable("python3").ok_or_else(|| fixable("reel.python_missing", "polishing the video needs uv or python3", install_hint("uv")))?;
    let _ = std::fs::remove_dir_all(&venv);
    let created = run(&system, &["-m".to_owned(), "venv".to_owned(), venv.to_string_lossy().into_owned()], INSTALL_TIMEOUT).await?;
    if !created.status.success() {
        return Err(fixable("reel.python_setup", format!("could not create a Python environment: {}", tail(&created.stderr)), install_hint("uv")));
    }
    let installed = run(&interpreter, &["-m", "pip", "install", "--quiet", "numpy", "pillow>=9.5"].map(String::from), INSTALL_TIMEOUT).await?;
    if !installed.status.success() {
        return Err(fixable("reel.python_setup", format!("could not install numpy and Pillow: {}", tail(&installed.stderr)), install_hint("uv")));
    }
    std::fs::write(&ready, b"numpy pillow\n")?;
    Ok((interpreter, Vec::new()))
}

pub async fn compose(layout: &Layout, video: &Path, events: &Path, timeline: &Path, out: &Path) -> Result<TimeMap> {
    let reel = crate::paths::reel_dir()?;
    let (ffmpeg, ffprobe) = (tool("ffmpeg")?, tool("ffprobe")?);
    let (program, mut args) = python(layout).await?;
    let partial = out.with_extension("partial.mp4");
    let _ = std::fs::remove_file(&partial);
    let path = |value: &Path| value.to_string_lossy().into_owned();
    args.extend([path(&reel.join("reel.py")), "compose".to_owned(), "--video".to_owned(), path(video), "--events".to_owned(), path(events), "--timeline".to_owned(), path(timeline), "--out".to_owned(), path(&partial), "--font-dir".to_owned(), path(&reel.join("reel_assets")), "--ffmpeg".to_owned(), path(&ffmpeg), "--ffprobe".to_owned(), path(&ffprobe)]);
    let output = run(&program, &args, COMPOSE_TIMEOUT).await?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&partial);
        let detail = tail(&output.stderr);
        if detail.contains("No module named") {
            return Err(fixable("reel.python_setup", format!("the polisher's Python packages are missing: {detail}"), install_hint("uv")));
        }
        return Err(fixable("reel.failed", format!("polishing the video failed: {detail}"), "run `qareel demo finish` again; if it fails the same way, read the message above"));
    }
    let line = String::from_utf8_lossy(&output.stdout).lines().rev().find(|line| line.trim_start().starts_with('{')).map(str::to_owned).ok_or_else(|| fail("reel.output", "the polisher returned no time map"))?;
    let map: TimeMap = serde_json::from_str(&line).context("reel.output: the polisher's time map is unreadable")?;
    std::fs::rename(&partial, out)?;
    Ok(map)
}

pub async fn passthrough(layout: &Layout, params: &[String]) -> Result<i32> {
    let reel = crate::paths::reel_dir()?;
    let (program, mut args) = python(layout).await?;
    args.push(reel.join("reel.py").to_string_lossy().into_owned());
    args.extend(params.iter().cloned());
    if params.first().is_some_and(|command| command == "compose" || command == "frame") {
        for (flag, value) in [("--font-dir", reel.join("reel_assets")), ("--ffmpeg", tool("ffmpeg")?), ("--ffprobe", tool("ffprobe")?)] {
            if !params.iter().any(|param| param == flag) {
                args.extend([flag.to_owned(), value.to_string_lossy().into_owned()]);
            }
        }
    }
    let status = tokio::process::Command::new(&program).args(&args).status().await.with_context(|| format!("reel.start: could not run {}", program.display()))?;
    Ok(status.code().unwrap_or(1))
}
