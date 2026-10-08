# qareel

Record polished, trustworthy QA demo videos of a web app from any coding agent.

```sh
npx @patrick-lai/qareel@latest guide
```

qareel drives a real WebKit browser, records it, and turns the recording into a 4K video with a title card, numbered caption pills and click marks. It enforces the discipline that makes the video evidence rather than a showreel: a written plan that can fail, captions that match the plan, every check reported with a time inside the video, and a recording bound to one git commit. `qareel demo finish` writes `demo.mp4` and `evidence.md`, a criteria table with timestamps that is ready to paste into a pull request.

An agent needs no skill or plugin: `qareel guide` teaches the whole workflow, and every command's `--help` points back to it. Repositories that want a nudge can run `qareel init --claude` (writes `.claude/skills/qareel/SKILL.md`) or `qareel init --agents` (adds a section to `AGENTS.md`).

## Voice-over

Add `narration` to a plan's shots (and optionally `voiceover.intro` / `voiceover.outro`) and the finished video talks, over the ambient music qareel already generates (the music dips under the voice). The plan's script is shown to the person first, as Markdown, so they can change it before anything is recorded:

```sh
qareel demo plan --file plan.json   # plan with narration
qareel demo script                  # writes script.md and prints it: show it to the person
qareel demo wait                    # repeat while it says "still waiting"
qareel demo approve                 # they said it is fine (or: qareel demo revise --file plan.json)
qareel demo start                   # refuses until the script is approved
qareel demo finish                  # polish, then voice-over; the silent cut is kept as demo.no-voice.mp4
```

If nobody replies within the waiting time (`afk=SECONDS` on `demo script`, default 300, `0` waits for ever), `demo wait` or `demo start` approves the script automatically and `evidence.md` says so, so an unattended run carries on. Narration describes what is being checked, never the result; a shot whose check fails or is not run keeps no narration in the video.

Voices are local: the best installed Mac system voice, [Piper](https://github.com/OHF-Voice/piper1-gpl) with a voice in `~/.qareel/voices` (natural, works on Linux), espeak as a last resort, or any tool through `QAREEL_TTS_COMMAND` (reads the text on stdin, writes audio to `{out}`). `qareel doctor` reports which is in use; `qareel demo narrate` redoes the voice on a finished demo. Each line is trimmed, levelled and gently compressed, and the video stream is copied, not re-encoded, unless the voice outlasts the video (then the last frame is held). Plans without narration behave exactly as before.

## Parity with CommissionAI

qareel ships the same WebKit recorder, page scripts and polish step as CommissionAI's QA demos, and the same generic browser tools: open, back, forward, reload, snapshot (with refs and diffs), click (left, right, middle, double), hover (including pointer-lock look), type, fill, select, press (keys, chords, held keys and a virtual gamepad), scroll (including pinch zoom), drag, upload and native file choosers, wait, eval (including inside iframes), screenshot, look, loop, resize, console, network, fetch, dialogs, tabs and popups, batch, and recording with captions and click marks.

These stay in CommissionAI because they need its daemon: the goal-driven browser run, taught site skills, bookmarks, sign-in sync with devboxes, recording grants for other origins, publishing to Loom, Artifacts and pull requests, and the Jira board tab.

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
| `reel/` | the polish step (`reel.py`), the music and click sounds (`reel_sound.py`), the voice-over (`reel_voice.py`), fonts and tests |
| `npm/`, `scripts/`, `install.sh` | distribution: npm packages with per-platform binaries, release tarballs |

## How it runs

The first command starts `qareel serve` in the background. It owns the browser engine (a child process speaking newline-delimited JSON over stdio) and listens on a private Unix socket under `~/.qareel/run/`. Later commands connect to it, so the browser keeps its tabs and sign-ins between commands. It exits after 15 idle minutes, never while a recording is running. If the engine crashes, the command reports `browser.host_exited` and the next command restarts it, at most three times in five minutes.

State lives in `~/.qareel` (`QAREEL_HOME` overrides it): the browser profile, recordings, demos and `serve.log`.

Other overrides: `QAREEL_HOST` (engine binary), `QAREEL_REEL_DIR` (polisher folder), `QAREEL_PYTHON` (an interpreter that already has numpy and Pillow), `QAREEL_LINUX_IMAGE` (browser image) and `QAREEL_IDLE_MINUTES`. Tools that embed qareel read `qareel version --json` for its version, engine protocol and Linux image.

## Development

```sh
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build -c release --package-path host-macos
uv run --with numpy --with pillow --with pytest python -m pytest reel/tests -q -p no:cacheprovider
scripts/build-dist.sh && scripts/pack-npm.sh --local
```

A development build finds `host-macos/.build/release/qareel-host` and `reel/` next to the repository automatically; `QAREEL_HOST` and `QAREEL_REEL_DIR` override them.
