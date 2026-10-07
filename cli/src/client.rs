use crate::failure::{fail, fixable};
use crate::paths::Layout;
use crate::serve::{MAX_REQUEST_BYTES, Request, Response};
use anyhow::Result;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const START_WAIT: Duration = Duration::from_secs(20);
const REPLY_WAIT: Duration = Duration::from_secs(45 * 60);

fn spawn_server(layout: &Layout) -> Result<()> {
    let executable = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&layout.log)?;
    std::process::Command::new(executable).arg("serve").stdin(Stdio::null()).stdout(Stdio::from(log.try_clone()?)).stderr(Stdio::from(log)).process_group(0).spawn().map_err(|error| fail("serve.start", format!("could not start the qareel session: {error}")))?;
    Ok(())
}

async fn connect(layout: &Layout, start: bool) -> Result<Option<UnixStream>> {
    if let Ok(stream) = UnixStream::connect(&layout.socket).await {
        return Ok(Some(stream));
    }
    if !start {
        return Ok(None);
    }
    spawn_server(layout)?;
    let deadline = Instant::now() + START_WAIT;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(stream) = UnixStream::connect(&layout.socket).await {
            return Ok(Some(stream));
        }
    }
    Err(fixable("serve.start", "the qareel session did not start", format!("read {} for the reason", layout.log.display())))
}

async fn exchange(mut stream: UnixStream, request: &Request) -> Result<Response> {
    let mut line = serde_json::to_vec(request)?;
    if line.len() as u64 > MAX_REQUEST_BYTES {
        return Err(fail("serve.request_too_large", "the request exceeds 16 MiB"));
    }
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.flush().await?;
    let mut reply = Vec::new();
    let mut reader = BufReader::new(stream.take(64 * 1024 * 1024));
    tokio::time::timeout(REPLY_WAIT, reader.read_until(b'\n', &mut reply)).await.map_err(|_| fail("serve.timeout", "the qareel session did not answer"))??;
    serde_json::from_slice(&reply).map_err(|_| fixable("serve.reply_invalid", "the qareel session ended without answering; the command's outcome is unknown", "run `qareel record status` or `qareel demo status` before repeating it"))
}

pub async fn send(request: &Request, start: bool) -> Result<Option<Response>> {
    let layout = Layout::current()?;
    layout.prepare()?;
    for attempt in 0..2 {
        let Some(stream) = connect(&layout, start).await? else { return Ok(None) };
        let response = exchange(stream, request).await?;
        let stale = matches!(&response, Response::Error { code, fix: None, .. } if code == "serve.version");
        if !stale || attempt == 1 {
            return Ok(Some(response));
        }
        let deadline = Instant::now() + START_WAIT;
        while layout.socket.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Ok(None)
}
