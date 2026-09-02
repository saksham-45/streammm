# Firstmate × Grok — streammm live QA handoff

Paste this whole file to a **Firstmate primary** (Grok harness) as the captain task. It is a **scout**, not a ship: play the product as a host user and as a remote client, write a report, do not KeepAlive-install the origin, do not merge, do not “improve until TeamViewer.”

```
Captain task: Scout-mode live QA of streammm. Play HOST USER and REMOTE CLIENT.
Repo: /Users/saksham/streammm (github.com/saksham-45/streammm, main).
Do not treat this as a feature-build. Report at data/<task-id>/report.md.
```

---

## Who you are

You are a Firstmate **scout crewmate** (Grok). Firstmate owns spawn/status. You own the browsers and the origin process **for this task only**.

Two hats in **one** scout (two Playwright browser contexts, not two machines):

| Hat | Who | URL | Job |
|-----|-----|-----|-----|
| **HOST** | person sitting at the Mac | `http://127.0.0.1:8080` | origin UI: stream, Settings, PIN, kill switches |
| **CLIENT** | remote watcher | same origin **or** Worker watch URL if publish is set | PIN/unattended unlock, watch, control, files, AI, chat, Talk |

Do not spawn a second Firstmate ship to “fix” findings unless the captain later asks. File bugs in the report.

---

## Hard rules (violations fail the scout)

1. **Do not load or enable `com.streamaid.origin` LaunchAgent.** Do not `launchctl bootstrap/load/kickstart` it. Last time KeepAlive respawned capture and macOS Screen Recording dialogs looped while the captain was not even using the app. The agent is **disabled** on purpose (`RunAtLoad`/`KeepAlive` false).
2. **Start origin as a one-shot process** you will kill at the end:
   ```bash
   cd /Users/saksham/streammm
   export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
   export RUST_LOG=info
   ./target/release/streamaid -c ./config.json
   ```
   If the binary is stale vs `git rev-parse HEAD`, `rustup run stable cargo build --release --bin streamaid` first. Bind is `127.0.0.1:8080`.
3. **If 8080 is already taken**, do not steal it. Report blocked and stop. Do not kill a process you did not start unless it is clearly your previous scout leftover.
4. **Do not open System Settings privacy panes in a loop.** `POST /api/permissions/open` is click-once, never on a timer. If Screen Recording is denied, note it and continue UI-only tests. Accessibility is only required when remote control is on.
5. **Remote control and AI start OFF.** Enable them in Settings like a user. Enabling a toggle must **reveal** its controls (files, chat, send-keys, unattended password, etc.). If it does not, that is a P0.
6. **Do not push, commit, or leave KeepAlive on.** Scout = report. Stop the origin when done (`kill` the pid you started). Confirm `127.0.0.1:8080` is free.
7. Load **webapp-testing** (Playwright). Two contexts: Host (chromium) and Client (chromium, separate storage so cookies/PIN session do not leak). Headed is better if the environment allows; otherwise headless + screenshots.
8. Firstmate: dispatch via `fm-spawn.sh` / brief, not harness built-in subagents. This repo is a **project clone**, not firstmate itself.

---

## Product snapshot (do not rediscover from scratch)

- **Checkpoint tag:** `checkpoint-2026-09-02-streammm`
- **Likely HEAD:** `69bab38` or later on `main` — *Let AI drive Files panel list/mkdir/rename/copy/move/delete*
- Origin: Rust `streamaid`. UI: `web/index.html` + `web/app.js` (compiled into the binary via `include_str!` — **rebuild after UI edits**).
- Watch path: WebSocket `/stream.ws` (not HTTP `/stream.mp4` through a tunnel).
- PIN: 6-digit in host header (`#pin-pill`), ~5 min. Redeem `POST /api/otp/redeem` `{ "pin": "…" }` → `streamaid_session`.
- Host token login: `POST /api/login` `{ "token": "…" }` only if `config.token` is set. Current `config.json` token is **empty** → no login overlay.
- Worker: `cloudflare/` (`streamaid-edge`). `config.json` currently has **empty** `publish_url` / `watch_url` — client tests on **localhost** unless you find a live watch URL in Settings. Historical worker: `streamaid-edge.sakshamiscool3434.workers.dev` — only use if Settings still has it or captain confirms.
- Kill switch: uncheck **Allow remote computer use**.

Documented non-goals (do not file as product bugs):

- Sleeping Mac cannot send its own Wake-on-LAN packet (browser/Worker cannot UDP-broadcast; lid-close often ignores WoL).
- System audio needs BlackHole/Loopback; mic picker is the in-app path.
- Inactive-monitor thumbs are ~15 fps stills, extra avfoundation HUD possible.

---

## Setup checklist

1. `cd /Users/saksham/streammm && git status && git log -1 --oneline`
2. Build release if needed; start origin in background; `curl -sS http://127.0.0.1:8080/api/status` → 200 JSON with `capture`, `stream`, `otp`, `control`, `permissions`.
3. Playwright: context A = HOST, context B = CLIENT. Both `http://127.0.0.1:8080` unless a watch URL exists.
4. Screenshots into the scout worktree `qa/` (or `/tmp/streammm-qa/` if worktree is awkward). Name them `host-…png` / `client-…png`.

---

## Script — HOST USER

Do these as a human at the capture machine. After each step: screenshot + one line pass/fail.

1. **Land.** Page loads, live video or a clear capture error, status pill, PIN pill shows 6 digits. No permission banner if Screen Recording already works (status `permissions.screen` true **or** capture width>0). Accessibility banner must **not** show while remote control is off.
2. **Settings gear.** Drawer opens; Close / backdrop / Escape dismiss it.
3. **Toggles reveal controls (P0).** One at a time, enable then confirm the matching UI appears; disable and confirm it hides:
   - Allow remote computer use → Send-keys, Chat, Files, session-related settings
   - Allow AI computer use → Have AI use this computer / Cancel AI
   - LLM analysis → LLM fields
   - Capture audio → device picker + Unmute (hide picker in MJPEG)
   - Allow unattended access → password field
   - Encoder MJPEG → JPEG quality shown; bitrate/GOP hidden
4. **PIN.** Note the 6-digit PIN from `#pin-pill` for the client. Optional: regenerate via Settings/`POST /api/otp` (host token empty → mint should work).
5. **With remote control ON:** Files panel (Inbox/Home/Desktop/Documents/Downloads), Split second pane, New folder, Rename, select-all / Shift-click, Copy/Cut/Paste here. Do not delete the captain’s real Desktop files — use Inbox or a folder you create named `qa-scout-*` and delete that tree at the end.
6. **Session banner.** If you can get a client driving, host shows REMOTE SESSION + End. End kicks the client.
7. **Do not** enable Blank screen, Block local input, or Lock on end on the captain’s real Mac unless they are at the keyboard and asked. Note “skipped — destructive” instead.

---

## Script — REMOTE CLIENT

Separate browser context (no host cookies).

1. **Unlock.** Open the same origin. If login overlay: enter the host PIN (not a guess). Wrong PIN → visible error, not a silent no-op. Right PIN → overlay gone, video plays. Unattended password only if host set one (likely not).
2. **Watch.** Live video moves (or MJPEG). Full (F11) enters fullscreen; Escape exits without injecting keys into the host OS.
3. **Quality.** Quality / Balanced / Speed control exists; changing it does not 5xx.
4. **Control (only if host enabled remote control).** Click/drag on the video. Send-keys bar visible. Type in Chat — must **not** appear as keystrokes in a host text field; host Chat should show the message (or report if chat did not fan).
5. **Files.** Same Files UI: list a root, do **not** delete user documents. Create `qa-scout-*` under Inbox, drop/upload a tiny text file, Get it back, Delete it.
6. **AI (only if host enabled AI).** Submit a tiny task like “do nothing / done”. Expect JSON ok or a clear disabled/403 — not a blank hang. Cancel AI on host if a loop starts.
7. **Talk / Record.** If visible, confirm the control exists; do not blast speaker audio. Local Record button should start/stop without crashing the player.
8. **Host offline (optional).** Do not kill origin unless you started it. If you briefly stop origin, client pill should not stay “live”; WoL/Copy MAC may appear. Restart origin if you stopped it.

---

## API smoke (no UI)

Drive the live origin. Do not hardcode PINs.

```bash
# status
curl -sS http://127.0.0.1:8080/api/status | jq '.capture,.control,.permissions,.otp'

# PIN mint + redeem (empty host token)
PIN=$(curl -sS -X POST http://127.0.0.1:8080/api/otp | jq -r .pin)
curl -sS -D - -X POST http://127.0.0.1:8080/api/otp/redeem \
  -H 'content-type: application/json' -d "{\"pin\":\"$PIN\"}" | head

# fail-closed
curl -sS -D - -X POST http://127.0.0.1:8080/api/otp/redeem \
  -H 'content-type: application/json' -d '{'   # expect 400 JSON error
curl -sS -D - -X POST http://127.0.0.1:8080/api/login \
  -H 'content-type: application/json' -d '{"token":""}'  # expect 400
```

Computer-use without AI enabled → 403 JSON `error`. Missing session with token configured → 401. Current config token is empty.

---

## Pass / fail bar

**Pass** if: origin serves UI + status; PIN redeem works; settings reveals; client can unlock and see video; files happy-path in Inbox without wrecking Desktop; no permission dialog storm; origin is dead and 8080 free when you leave.

**Fail (P0)** if: LaunchAgent left enabled; Screen Recording dialogs looping; Accessibility banner with remote control off; enabling a setting does not show controls; PIN unlock silent-fail; 5xx on redeem/login/config; you left ffmpeg/streamaid running.

---

## Report shape (`data/<task-id>/report.md`)

```
# streammm live QA
HEAD: <sha> <subject>
origin: started-by-scout yes/no  pid  killed-at-end yes/no
permissions.screen/accessibility/input: <from /api/status>
capture: running/size/error

## Host
- land:
- settings reveal:
- PIN:
- files (Inbox only):
- skipped destructive:

## Client
- unlock:
- video:
- control/chat:
- files:
- AI:

## API
- redeem 200:
- bad JSON 400:

## P0/P1 list
## Screenshots
```

Status to Firstmate: `done:` with the report path, or `blocked:` if 8080 busy / no ffmpeg / TCC actually denied and you cannot capture.

---

## Firstmate spawn sketch (primary does this)

```text
Scout, not ship. Repo streammm.
Brief: HANDOFF-QA-FIRSTMATE.md in the repo root (this file).
Harness: grok. Mode: scout.
Kill origin on teardown if this task started it.
```
