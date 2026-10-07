# qareel

Record polished, trustworthy QA demo videos of a web app from any coding agent.

```sh
npx qareel@latest guide
```

qareel drives a real WebKit browser, records it, and turns the recording into a 4K video with a title card, numbered caption pills and click marks. It enforces the discipline that makes the video evidence rather than a showreel: a written plan that can fail, captions that match the plan, every check reported with a time inside the video, and a recording bound to one git commit. `qareel demo finish` writes `demo.mp4` and `evidence.md`, a criteria table with timestamps that is ready to paste into a pull request.

An agent needs no skill or plugin: `qareel guide` teaches the whole workflow, and every command's `--help` points back to it. Repositories that want a nudge can run `qareel init --claude` (writes `.claude/skills/qareel/SKILL.md`) or `qareel init --agents` (adds a section to `AGENTS.md`).

## Requirements

- macOS 14 or later, or Linux with rootless podman (`qareel install` downloads the browser image).
- ffmpeg, and uv or python3, for the polish step. `qareel doctor` checks everything and prints the exact fix.

## Layout

| Path | What it is |
|---|---|
| `protocol/` | `qareel-protocol`, the NDJSON wire types between the CLI and its browser engines (serde only; feature `ts` adds ts-rs bindings) |
| `cli/` | the `qareel` binary: session server, browser commands, recording, demo workflow, guide |
| `host-macos/` | `qareel-host`, the WKWebView engine and H.264 recorder for macOS |
| `host-linux/` | the WPE WebKit engine and its container image for Linux |
| `reel/` | the polish step (`reel.py`), its fonts and tests |
| `npm/`, `scripts/`, `install.sh` | distribution: npm packages with per-platform binaries, release tarballs |

## How it runs

The first command starts `qareel serve` in the background. It owns the browser engine (a child process speaking newline-delimited JSON over stdio) and listens on a private Unix socket under `~/.qareel/run/`. Later commands connect to it, so the browser keeps its tabs and sign-ins between commands. It exits after 15 idle minutes, never while a recording is running. If the engine crashes, the command reports `browser.host_exited` and the next command restarts it, at most three times in five minutes.

State lives in `~/.qareel` (`QAREEL_HOME` overrides it): the browser profile, recordings, demos and `serve.log`.

## Development

```sh
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build -c release --package-path host-macos
uv run --with numpy --with pillow --with pytest python -m pytest reel/tests -q -p no:cacheprovider
scripts/build-dist.sh && scripts/pack-npm.sh --local
```

A development build finds `host-macos/.build/release/qareel-host` and `reel/` next to the repository automatically; `QAREEL_HOST` and `QAREEL_REEL_DIR` override them.
