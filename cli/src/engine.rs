use crate::failure::{fail, fixable};
use crate::host::Launch;
use crate::paths::{Layout, atomic_write, private_dir};
use anyhow::Result;
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const CONTAINER_SECURITY_OPTIONS: [&str; 3] = ["--cap-drop=all", "--security-opt=no-new-privileges", "--security-opt=unmask=/proc/*"];

pub fn default_image() -> String {
    std::env::var("QAREEL_LINUX_IMAGE").ok().filter(|image| !image.is_empty()).or_else(|| option_env!("QAREEL_LINUX_IMAGE").map(str::to_owned)).unwrap_or_else(|| format!("ghcr.io/patrick-lai/qareel-browser:{}", env!("CARGO_PKG_VERSION")))
}

pub fn profile_id(layout: &Layout) -> Result<String> {
    let path = layout.root.join("profile.id");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let text = text.trim();
        if let Ok(id) = uuid::Uuid::parse_str(text)
            && id.to_string() == text
        {
            return Ok(text.to_owned());
        }
        let backup = layout.root.join(format!("profile.id.corrupt-{}", uuid::Uuid::new_v4()));
        std::fs::rename(&path, backup)?;
    }
    let id = uuid::Uuid::new_v4().to_string();
    atomic_write(&path, id.as_bytes())?;
    Ok(id)
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|directory| directory.join(name)).find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn base_env(home: &Path) -> Vec<(String, OsString)> {
    let mut env = vec![("PATH".to_owned(), OsString::from("/usr/local/bin:/usr/bin:/bin")), ("LANG".to_owned(), OsString::from("en_US.UTF-8")), ("HOME".to_owned(), home.as_os_str().to_owned())];
    for name in ["TMPDIR", "XDG_RUNTIME_DIR"] {
        if let Some(value) = std::env::var_os(name) {
            env.push((name.to_owned(), value));
        }
    }
    env
}

pub fn launch(layout: &Layout) -> Result<Launch> {
    private_dir(&layout.profile)?;
    let profile_id = profile_id(layout)?;
    let profile = layout.profile.to_str().filter(|text| !text.contains([':', ',', '\n', '\0'])).ok_or_else(|| fixable("browser.profile_path", "the qareel home path contains characters the browser engine cannot use", "export QAREEL_HOME=$HOME/.qareel"))?.to_owned();
    let user_home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| layout.root.clone());
    if cfg!(target_os = "macos") {
        return Ok(Launch { program: crate::paths::host_binary()?, args: Vec::new(), env: base_env(&user_home), bootstrap: json!({"profile_dir": profile, "profile_id": profile_id}), log: layout.log.clone() });
    }
    if !cfg!(target_os = "linux") {
        return Err(fail("browser.platform", "qareel records on macOS and Linux only"));
    }
    if let Some(host) = std::env::var_os("QAREEL_HOST").filter(|value| !value.is_empty()) {
        let tool = |name: &str, fallback: &str| std::env::var(format!("QAREEL_{}", name.to_uppercase())).ok().or_else(|| find_executable(fallback).map(|path| path.to_string_lossy().into_owned()));
        let mut bootstrap = json!({"profile_dir": profile, "profile_id": profile_id});
        for (key, binary) in [("ffmpeg", "ffmpeg"), ("pulseaudio", "pulseaudio"), ("dbus_daemon", "dbus-daemon")] {
            if let Some(path) = tool(key, binary) {
                bootstrap[key] = json!(path);
            }
        }
        return Ok(Launch { program: PathBuf::from(host), args: Vec::new(), env: base_env(&layout.profile), bootstrap, log: layout.log.clone() });
    }
    let podman = find_executable("podman").ok_or_else(|| fixable("browser.podman_missing", "qareel runs its Linux browser in a podman container and podman is not installed", "sudo apt install podman   # or your distribution's package manager, then run `qareel install`"))?;
    let image = default_image();
    let present = std::process::Command::new(&podman).args(["image", "exists", &image]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|status| status.success());
    if !present {
        return Err(fixable("browser.engine_missing", format!("the browser image {image} is not downloaded yet"), "qareel install"));
    }
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut args: Vec<String> = ["run", "--rm", "--interactive", "--init", "--pull=never", "--sig-proxy=true", "--network=host", "--userns=keep-id", "--log-driver=none", "--http-proxy=false"].map(String::from).to_vec();
    args.extend([format!("--user={uid}:{gid}"), format!("--name=qareel-browser-{}", std::process::id()), "--stop-timeout=5".to_owned()]);
    args.extend(CONTAINER_SECURITY_OPTIONS.iter().map(|option| (*option).to_owned()));
    args.extend(["--env".to_owned(), format!("HOME={profile}"), "--volume".to_owned(), format!("{profile}:{profile}"), image]);
    let bootstrap = json!({"profile_dir": profile, "profile_id": profile_id, "ffmpeg": "/usr/bin/ffmpeg", "pulseaudio": "/usr/bin/pulseaudio", "dbus_daemon": "/usr/bin/dbus-daemon"});
    Ok(Launch { program: podman, args, env: base_env(&user_home), bootstrap, log: layout.log.clone() })
}
