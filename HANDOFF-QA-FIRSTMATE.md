# Firstmate / Grok prompt — streammm

**This file on GitHub is the whole brief.** The bot has no access to anyone’s home directory. Clone the repo; do not look for `/Users/…`.

GitHub: https://github.com/saksham-45/streammm  
Branch: `main`  
This file: `HANDOFF-QA-FIRSTMATE.md`  
Product contract: `README.md` (same repo)

---

## Captain → Firstmate (paste this only)

```
Scout, Grok harness, not a ship.

Clone https://github.com/saksham-45/streammm (main).
The full brief is IN the repo: HANDOFF-QA-FIRSTMATE.md
Also read README.md.

You cannot see the captain’s local disk. GitHub is the source of truth.
Work in the Firstmate worktree of that clone.

Play HOST USER and REMOTE CLIENT. Write data/<task-id>/report.md.
Do not PR, do not launchctl KeepAlive, do not “improve until TeamViewer.”
```

---

## Prompt (scout executes this after clone)

You are a Firstmate scout (Grok). Live product QA, not a feature build.

```bash
git clone https://github.com/saksham-45/streammm.git
cd streammm
git checkout main
git pull
# or use the Firstmate worktree of this GitHub repo — same thing
```

Every path below is **relative to that clone**. `config.json` is gitignored; if it is missing, the origin **writes defaults** on first run. Do not require a local machine path, SSH to the captain, or files that are not in git.

### What this product is

streammm (binary **streamaid**) is **TeamViewer for one computer**, plus **AI that can use that computer**.

Two people:

1. **HOST** — owns the machine. Runs the origin. Reads a 6-digit PIN (or sets an unattended password). Turns features on in Settings. Can End the session and uncheck remote control (kill switch).
2. **CLIENT** — only a browser + that PIN. Sees the screen. If the host allowed it, they **use** the computer: mouse, keyboard, clipboard, files, and/or an AI task.

It is not Zoom and not a VOD player. If video works but the client cannot click, type, paste, and move files, the product is failing.

How it is built (all in this GitHub repo):

- `src/` — Rust origin. ffmpeg captures a display, H.264 over **WebSocket** `/stream.ws` (~0.5s live edge), not HTTP `/stream.mp4` through a tunnel.
- `web/` — host UI (compiled into the binary; rebuild after UI edits).
- `cloudflare/` — Worker + Durable Object. One upload, N watchers. `/watch` is the client socket; `/publish` is ingest (STREAM_TOKEN, never on the public watch URL).
- Control: client JSON → (Worker) → origin injector (real HID on macOS; tests use a fake injector).
- Remote control and AI are **off until the host enables them**.

### What it is supposed to do

**Host UI** (`http://127.0.0.1:8080` after you start origin in the clone):

- Live view of this machine’s screen.
- Header PIN (6 digits, ~5 min) to read to the client.
- Settings gear. Every capability is a toggle, **off by default**. **On → that feature’s controls appear. Off → they hide.** Dead toggles are P0.
- Allow remote computer use → client may drive the machine. Host gets REMOTE SESSION + End. Reveals Send-keys, Chat, Files (Inbox/Home/Desktop/Documents/Downloads, split pane, drop, zip Get, multi-select, copy/cut/paste, rename, mkdir, recursive delete). Optional (do not turn on in QA on a shared Mac): block local input, blank screens, lock on end.
- Allow AI computer use → task box; screenshot → model → same injector; file actions list/mkdir/rename/copy/move/delete. Cancel AI stops it.
- Capture audio, Talk, unattended password, Quality/Balanced/Speed, display map, Fullscreen, Record — as in README.md.

**Client:**

- No token in the URL. PIN (or unattended password) on the unlock field. Wrong PIN = **visible error**. Right PIN = session cookie + video.
- Drive the desktop if control is on. Chat must stay in Chat, not type into the host OS.
- Files jailed to those roots. AI uses the same file handlers if enabled.
- Worker watch page: if origin drops, show **host offline**, not live. Copy MAC may still work. A sleeping host cannot send its own WoL packet.

**APIs (fail closed, never 5xx on these):**

- `POST /api/otp/redeem` — invalid JSON / missing/empty/non-string pin → 400 `{error}`; wrong pin → 401; after fail limit → 429; valid 6-digit pin → 200 + `Set-Cookie: streamaid_session=`
- `POST /api/login` — same idea for host token when configured
- `POST /api/config`, `POST /api/computer-use` — bad JSON / missing fields → 400; no auth when token set → 401; computer-use with AI off → 403

### How to run (from the GitHub clone only)

Do **not** `launchctl` `com.streamaid.origin` (KeepAlive caused a permission-dialog storm).

```bash
# from clone root
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
rustup run stable cargo build --release --bin streamaid
# missing config.json is OK — origin creates defaults (empty token, 127.0.0.1:8080)
./target/release/streamaid -c ./config.json
```

If port 8080 is busy and you did not start it: `blocked:` and stop.  
Kill **your** origin pid when the scout ends. Leave the port free. Do not leave ffmpeg capturing.

Play **both hats** with two browser contexts (Playwright / webapp-testing), **separate cookies**.

**HOST:** open `http://127.0.0.1:8080`, confirm live view or honest capture error, read PIN, enable remote control + AI, confirm controls **appear**. Do not enable blank/lock/block-local.

**CLIENT:** new context, same URL (Worker only if `config.json` in the running tree has watch_url; defaults have none). Unlock with PIN; also prove wrong PIN errors. Video, click, type, Chat, Files under Inbox folder `qa-scout-*` only (create, tiny upload, Get, delete that folder). Trivial AI task if enabled. Then host End + uncheck remote control; client must not inject.

Do not loop System Settings or `POST /api/permissions/open`. Accessibility banner is only valid when remote control is on.

### Report (`data/<task-id>/report.md` in the Firstmate home, not this repo)

```
# streammm QA
clone: https://github.com/saksham-45/streammm @ <sha>
What the product is: <2 sentences>
origin pid / killed:
/api/status capture + permissions:

## Host
## Client
## API fail-closed
## P0 (does not do what it is supposed to)
## skipped destructive
```

`done:` with that path, or `blocked:` if origin would not start.
