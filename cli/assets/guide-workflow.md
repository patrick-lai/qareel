# qareel: prove a web change works with a QA demo video

qareel drives a real WebKit browser (the engine inside Safari), records it, and polishes the recording into a 4K demo video with a title card, numbered caption pills and crayon click marks. It also enforces what makes that video trustworthy evidence: a written plan that can fail, captions that match the plan, every check reported with a time inside the video, and a recording bound to one exact git commit.

Below, `qareel` means however you started it: `npx qareel@latest` works anywhere with Node 18+, and `npm install -g qareel` puts `qareel` on PATH. Run every command from the repository you are testing. A background session starts on the first command, keeps the browser and its sign-ins between commands, and stops by itself after 15 idle minutes (`qareel stop` ends it now). Run `qareel doctor` once to check the machine: it needs ffmpeg and uv (or python3) for the polish step.

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

Example:

