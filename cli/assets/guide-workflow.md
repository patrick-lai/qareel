# qareel: prove a web change works with a QA demo video

qareel drives a real WebKit browser (the engine inside Safari), records it, and polishes the recording into a 4K demo video with a title card, numbered caption pills and crayon click marks. It also enforces what makes that video trustworthy evidence: a written plan that can fail, captions that match the plan, every check reported with a time inside the video, and a recording bound to one exact git commit.

Below, `qareel` means however you started it: `npx @patrick-lai/qareel@latest` works anywhere with Node 18+, and `npm install -g @patrick-lai/qareel` puts `qareel` on PATH. Run every command from the repository you are testing. A background session starts on the first command, keeps the browser and its sign-ins between commands, and stops by itself after 15 idle minutes (`qareel stop` ends it now). Run `qareel doctor` once to check the machine: it needs ffmpeg and uv (or python3) for the polish step.

## The workflow, in order

1. Understand the change. Read the request or ticket AND the diff. Search (read-only) for other callers, pages, endpoints, strings and tests that touch what changed.
2. Write the plan as a JSON file (format and example below) and save it: `qareel demo plan --file plan.json`. qareel rejects plans that cannot fail; fix what it says and save again.
3. Commit the change under test. Tracked files must be clean: the video proves one exact commit, and `demo finish` refuses if the checkout changes while recording. Untracked files are fine.
4. Start the app with the repository's documented development command and wait until it answers. Reuse a server that is already running for this checkout.
5. Open the first page you will show: `qareel open http://localhost:3000/settings`. Check the viewport with `qareel eval "() => [innerWidth, innerHeight]"` (expect 1280x800; fix it with `qareel resize 1280 800`). Look at the page with `qareel snapshot` and `qareel screenshot`. Prepare data and state before recording, so the first frames already show the feature. Never record a sign-in screen.
6. `qareel demo start` binds the commit and starts recording.
7. For each shot, in order:
   - `qareel demo shot N` shows that shot's caption on screen and prints what to do.
   - Do the actions with the browser commands below (`snapshot`, `click`, `type`, `fill`, `press`, `wait`, ...). Keep shots short and deliberate; do not idle.
   - Report the criterion at once: `qareel demo check C passed|failed|not_checked "expected X, observed Y"`, where C is the criterion number. The time is taken from the shot's caption automatically.
8. `qareel demo finish` stops the recording, verifies it, polishes it and prints the paths of `demo.mp4` and `evidence.md` (a criteria table with video timestamps, ready to paste into a pull request). If it reports a problem, it says how to fix it; run it again after fixing. qareel never uploads anything: attach the video and paste the evidence yourself, or hand both to the person who asked.

If the recording fails (zero frames, the engine stopped, the page left its origin), fix the cause and run `qareel demo start` again; that records afresh. A complete recording is kept as evidence and cannot be replaced: to record a different demo, save a new plan.

## Plan format

- `title` (required, under 120 bytes): shown on the title card, for example the ticket key or change name. `subtitle` (optional, under 200 bytes): one line on what changed.
- `criteria` (1 to 30, distinct): the acceptance checklist, each a short exact sentence. Add each uncovered risk as its own criterion prefixed `Risk:`.
- `script.changed_behaviors`, `script.risks` (required, non-empty), `script.assumptions` (optional): lists of plain sentences.
- `script.shots` (1 to 40): each has `criterion` (exact text from `criteria`), `kind` (happy, boundary, negative, persistence, regression or permission), `surface` (browser, api, cli or data), `setup`, `do`, `expect`, `falsifier`, `capture`, `caption` (under 160 bytes, shown on screen) and `est_seconds` (3 to 120; all shots together at most 300).
- `script.not_demonstrable` (optional): `{"item": "exact criterion or risk", "reason": "why the live app cannot show it"}`.

- `narration` (optional, on a shot, under 600 bytes): what the voice says over that shot. `voiceover` (optional): `intro` (said over the title card), `outro` (said before the video ends) and `voice` (a voice name; omit it for the best voice on the machine). See "Voice-over" below.

## Voice-over: script first, then the person's OK, then recording

Add `narration` to the plan when the video should talk. qareel then adds a calm voice-over to the video, on top of the ambient music it already generates (the music dips under the voice). No other setup is needed on a Mac. Because a voice can say things a caption cannot, the person sees the script before anything is recorded:

1. Save the plan with narration as usual: `qareel demo plan --file plan.json`.
2. `qareel demo script` writes `script.md` (a Markdown preview: every shot with its narration, how long each line takes to say, and timing warnings) and prints it. Paste that Markdown into your reply, ask whether it is good or what to change, and say how long you will wait. `qareel demo script afk=600` sets the waiting time in seconds (default 300; `afk=0` waits for ever).
3. `qareel demo wait` waits up to 45 seconds and says what to do next. Run it again while it says the script is still waiting. Between runs, read anything the person wrote:
   - They say it is fine: `qareel demo approve` (add `by="Their Name"`).
   - They want changes: edit the plan JSON, run `qareel demo revise --file plan.json`, show the new script and wait again. A changed script always needs a new OK.
   - They do not reply: when the waiting time is up, `qareel demo wait` (or `qareel demo start`) approves the script by itself and `evidence.md` says it was approved automatically. This is what lets an unattended run carry on. Never approve for the person yourself, and never skip the wait.
4. Record as usual from step 3 of the workflow (`qareel demo start` refuses until the script is approved). `qareel demo finish` polishes the video and then speaks the narration over it; it also keeps the version without voice as `demo.no-voice.mp4`.
5. If `demo finish` says the voice-over was not added (no voice engine installed), install one (`qareel doctor` says how), then run `qareel demo narrate`. It re-voices the finished demo without recording again. `qareel demo narrate voice="Name"` picks another voice, and `engine=piper|say|espeak|command` another engine.

Voices: on a Mac qareel uses the best installed system voice (an Ava, Zoe or Evan "Premium" or "Enhanced" voice sounds best; add one in System Settings > Accessibility > Spoken Content > System Voice > Manage Voices). On Linux install Piper (`pip install piper-tts`) and put a voice (`.onnx` and `.onnx.json`) in `~/.qareel/voices`. To use any other speech tool, set `QAREEL_TTS_COMMAND` to a command that reads the text on standard input and writes audio to `{out}`. The text is spoken on this machine unless that command sends it elsewhere.

Narration is written before the app is exercised, so it can only describe what is about to be checked. If a shot's check fails or is not run, qareel leaves that shot's narration out of the video and says so in the evidence.

Example:
