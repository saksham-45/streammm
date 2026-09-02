# Prompt for Firstmate / Grok — what streammm is, and how to test it

Copy everything inside the `PROMPT` fence into a **Firstmate primary** as the captain message. Spawn a **Grok scout** (not a ship). The scout plays the product as two people: the Mac owner and the remote client.

````text
PROMPT
You are a Firstmate scout (Grok harness). This is live product QA, not a feature build.
Do not open a PR. Do not KeepAlive-install a LaunchAgent. Do not “improve until TeamViewer.”
Write `data/<task-id>/report.md` and stop.

Repo: /Users/saksham/streammm
GitHub: https://github.com/saksham-45/streammm
Branch: main
Read this file in the repo after clone/worktree: HANDOFF-QA-FIRSTMATE.md
Also read README.md — it is the product contract.

════════════════════════════════════
WHAT THIS PRODUCT IS
════════════════════════════════════

streammm (binary name streamaid) is a **TeamViewer-class remote desktop for one Mac**, plus **AI that can use that Mac**.

There are exactly two humans in the story:

1. HOST — sits at the Mac. Runs the origin. Shares a 6-digit PIN (or an unattended password). Can kill the session.
2. CLIENT — anyone with a browser. Types the PIN. Sees the Mac’s screen. If the host allowed it, they drive the Mac (mouse, keyboard, clipboard, files) and/or tell an AI to drive it.

It is NOT a Zoom clone, NOT a VOD player, NOT “just a stream.” The live picture exists so the client can **use the computer**. If the client cannot type, click, paste, and move files the way they could in TeamViewer, the product is failing even if video looks fine.

Architecture in one paragraph:
The HOST Mac runs a Rust origin. ffmpeg captures a display and encodes H.264 (WebSocket fMP4, ~0.5s live edge). Optional Cloudflare Worker fans one upload to many watchers. The CLIENT browser talks to `/stream.ws` (or Worker `/watch`). Control JSON goes client → (Worker) → origin injector (real HID on macOS). PIN redeem mints a session cookie. Remote control and AI are **off until the host turns them on**.

════════════════════════════════════
WHAT IT IS SUPPOSED TO DO
════════════════════════════════════

HOST (http://127.0.0.1:8080)
- See their own screen live in the page.
- See a 6-digit PIN in the header to read to the client (~5 minutes; regenerable).
- Open Settings (gear). Every feature is a toggle that is OFF by default. **Turning a toggle ON must reveal that feature’s controls. Turning it OFF must hide them.** That is a hard product rule. Dead toggles are bugs.
- Allow remote computer use → client may drive the Mac. Host sees a REMOTE SESSION banner with End (kicks the client). Optional: block local input (⌘⇧Esc unlocks), blank the physical screens, keep Mac awake, lock Mac when session ends, record the session to recordings/, chat with the client, send-keys bar, file browser.
- Allow AI computer use → client (or host) can submit a task; origin loops screenshot → model → same injector (click/type/key/paste/files). Cancel AI stops it.
- Capture audio → mic/loopback into the stream; Unmute on players. Allow watcher to talk → client Talk button plays on host speakers.
- Allow unattended access → password field; after PIN expires the client uses that password on the same unlock box.
- Quality / Balanced / Speed retunes the live encode. Display map switches which screen is captured. Fullscreen and local Record exist on the live view.
- Kill switch: uncheck Allow remote computer use. Host always wins.

CLIENT (same origin in a second browser profile, or the Worker watch page if publish_url is set)
- No STREAM_TOKEN in the URL. They type the PIN (or unattended password). Wrong PIN = visible error, not a silent no-op. Right PIN = session cookie, video plays.
- Watch the live screen. Fullscreen. Quality picker. Local Record.
- If host enabled control: click / right-click / drag / scroll / type on the video, paste text/images/files, use Send-keys for shortcuts the browser would steal (⌘Tab, ⌘W, Alt+Tab, Ctrl+Alt+Del, …). Chat must go to the chat panel, NOT as keystrokes into whatever app is focused on the host.
- Files: browse Inbox / Home / Desktop / Documents / Downloads, two panes, drop, zip Get, multi-select, copy/cut/paste, rename, mkdir, recursive delete — like a small remote Finder, jailed to those roots.
- If host enabled AI: submit a task; AI should click/type and also list/mkdir/rename/copy/move/delete via the same file handlers.
- If the host origin drops, the Worker watch page should say host offline (not “live”) and still show Copy MAC. A sleeping Mac cannot send its own wake packet.

APIs fail closed: bad JSON / missing PIN / missing token → 400 JSON `{error}`; wrong PIN → 401; rate limit → 429; valid redeem → 200 + `Set-Cookie: streamaid_session=`. No 5xx on those paths.

════════════════════════════════════
WHAT YOU WILL DO (TWO HATS)
════════════════════════════════════

Play both people yourself with two Playwright (or real browser) contexts. Separate cookies. Headed if you can.

HAT A — HOST USER
You are Saksham at the Mac. You want to share this computer safely.
1. Start origin as a one-shot (rules below). Open http://127.0.0.1:8080.
2. Confirm you see live video (or an honest capture error). Read the PIN.
3. Open Settings. Enable **Allow remote computer use**. Confirm Send-keys, Chat, and Files actually appear. Disable and confirm they hide.
4. Enable **Allow AI computer use**. Confirm the AI box appears.
5. Leave those ON for the client hat. Do not enable Blank screen, Block local input, or Lock on end on the real Mac.

HAT B — REMOTE CLIENT
You are a friend on another laptop. You only have a browser and a PIN someone read to you.
1. New browser context. Same URL (localhost is fine for this scout; Worker only if Settings has a watch URL).
2. Unlock with the PIN from the host header. Then try a wrong PIN on a fresh context and demand a visible error.
3. Confirm video. Click the remote desktop. Type. Open Files, create `qa-scout-*` under Inbox only, upload a tiny text file, Get it, Delete that qa folder. Do not touch real Desktop documents.
4. If AI is on, submit a trivial task (“move the mouse slightly” or the model’s done). Host Cancel AI if it runs away.
5. Chat: send “hello from client” — it must show in host chat, not get typed into TextEdit on the Mac.

Then take HOST hat again: End session if the banner is up. Uncheck remote control (kill switch). Confirm client can no longer inject.

════════════════════════════════════
HARD RULES
════════════════════════════════════

- Do NOT launchctl load/bootstrap/kickstart com.streamaid.origin. KeepAlive caused a Screen Recording permission storm last time. The agent is disabled.
- Start origin yourself:
    cd /Users/saksham/streammm
    export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
    rustup run stable cargo build --release --bin streamaid   # if binary older than HEAD
    ./target/release/streamaid -c ./config.json
  Bind 127.0.0.1:8080. If 8080 is already taken by something you did not start, blocked: and stop.
- Kill the pid you started when the scout ends. 8080 must be free. Do not leave ffmpeg capturing.
- Do not loop System Settings / POST /api/permissions/open. Accessibility nag is only valid when remote control is on.
- config.json token is empty → no host-token login wall. PIN is the client door.
- Scout only: no git push, no LaunchAgent, no “fix the product” unless the captain follows up.

════════════════════════════════════
REPORT
════════════════════════════════════

data/<task-id>/report.md

# streammm QA
HEAD: <sha>
What I thought the product was: <2 sentences>
Origin pid / killed: 
permissions from /api/status:
capture width x height / error:

## Host user
- live view:
- PIN visible:
- settings reveal (control / AI / each toggle you touched):
- kill switch:

## Remote client
- wrong PIN error:
- right PIN → video:
- mouse/keyboard:
- chat did not leak into host OS:
- files Inbox qa-scout-* :

## P0 bugs (product does not do what it is supposed to)
## P1
## skipped (destructive: blank/lock/block-local)

Status: done: with that path, or blocked: if you could not start origin.
````
