use crate::failure::{fail, fixable};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const SOCKET_LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
    pub run: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub log: PathBuf,
    pub profile: PathBuf,
    pub recordings: PathBuf,
    pub demos: PathBuf,
    pub screenshots: PathBuf,
}

impl Layout {
    pub fn at(root: PathBuf) -> Result<Self> {
        if !root.is_absolute() {
            return Err(fixable("qareel.home_invalid", "QAREEL_HOME must be an absolute path", "export QAREEL_HOME=$HOME/.qareel"));
        }
        let run = root.join("run");
        let preferred = run.join("serve.sock");
        let socket = if preferred.as_os_str().len() <= SOCKET_LIMIT {
            preferred
        } else {
            let digest = hex(&Sha256::digest(root.as_os_str().as_encoded_bytes()))[..12].to_owned();
            PathBuf::from("/tmp").join(format!("qareel-{}-{digest}", unsafe { libc::getuid() })).join("serve.sock")
        };
        Ok(Self {
            lock: run.join("serve.lock"),
            log: root.join("serve.log"),
            profile: root.join("profile"),
            recordings: root.join("recordings"),
            demos: root.join("demos"),
            screenshots: root.join("screenshots"),
            socket,
            run,
            root,
        })
    }

    pub fn current() -> Result<Self> {
        let root = match std::env::var_os("QAREEL_HOME").filter(|value| !value.is_empty()) {
            Some(value) => PathBuf::from(value),
            None => {
                let home = std::env::var_os("HOME").filter(|home| Path::new(home).is_absolute()).ok_or_else(|| fixable("qareel.home_missing", "HOME is not set to an absolute path", "export QAREEL_HOME=/absolute/path/for/qareel"))?;
                PathBuf::from(home).join(".qareel")
            }
        };
        Self::at(root)
    }

    pub fn prepare(&self) -> Result<()> {
        for directory in [&self.root, &self.run, &self.recordings, &self.demos, &self.screenshots] {
            private_dir(directory)?;
        }
        if let Some(parent) = self.socket.parent() {
            private_dir(parent)?;
        }
        Ok(())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path).with_context(|| format!("qareel.storage: cannot create {}", path.display()))?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(fail("qareel.storage", format!("{} is not a directory", path.display())));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| fail("qareel.storage", "a file path has no folder"))?;
    private_dir(parent)?;
    let staging = parent.join(format!(".{}.part", uuid::Uuid::new_v4()));
    let written = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&staging)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&staging, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    written.with_context(|| format!("qareel.storage: cannot save {}", path.display()))
}

fn search(relative: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let executable = std::fs::canonicalize(&executable).unwrap_or(executable);
    executable.ancestors().skip(1).map(|ancestor| ancestor.join(relative)).find(|candidate| candidate.exists())
}

pub fn host_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("QAREEL_HOST").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    ["libexec/qareel-host", "host-macos/.build/release/qareel-host", "host-macos/.build/debug/qareel-host"]
        .into_iter()
        .find_map(search)
        .ok_or_else(|| fixable("browser.host_missing", "the qareel browser engine is not installed next to this binary", "reinstall with `npx qareel@latest` or set QAREEL_HOST to a built qareel-host"))
}

pub fn reel_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("QAREEL_REEL_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    ["share/qareel/reel", "reel"]
        .into_iter()
        .filter_map(search)
        .find(|root| root.join("reel.py").is_file() && root.join("reel_assets").is_dir())
        .ok_or_else(|| fixable("reel.missing", "the video polisher is not installed next to this binary", "reinstall with `npx qareel@latest` or set QAREEL_REEL_DIR to a folder with reel.py and reel_assets"))
}
