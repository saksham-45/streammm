# streammm / streamaid

Low-latency **1080p30 screen streaming**: capture on your machine, play in any browser, fan out through **Cloudflare Workers + Durable Objects**.

The origin is a **Rust** binary (`cargo run`). It encodes with ffmpeg (VideoToolbox on macOS, libx264 elsewhere) and delivers **WebSocket** fMP4 — not HTTP progressive download. That is the difference between localhost looking fine and Cloudflare looking like a stalled VOD.

## Why WebSocket (not `/stream.mp4` through a tunnel)

`cloudflared` buffers proxied HTTP unless `Content-Type: text/event-stream`. A live `video/mp4` body is held until the connection dies. WebSocket (`/stream.ws`, and the Worker `/publish` → `/watch` path) is not buffered that way.

The Worker also means **one upload** from home (the publisher) and **N viewers** at the edge.

## Quick start (this computer)

```bash
# ffmpeg on PATH. macOS: Screen Recording permission for the capturing app.
cargo run --release
# open http://127.0.0.1:8080
```

On this Mac the origin is a **LaunchAgent** (`com.streamaid.origin`). It stays up across terminal/chat sessions and login, and only stops when you unload it:

```bash
# start (and keep running)
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.streamaid.origin.plist

# stop — this is the off switch
launchctl bootout gui/$(id -u)/com.streamaid.origin
```

Killing the process is not enough — `KeepAlive` will restart it. Logs: `logs/origin.out.log`. If capture is black after a reboot, allow **Streamaid** under System Settings → Privacy & Security → Screen Recording.

`./config.json` is created with defaults if missing (gitignored).

| Default | Value |
|---------|--------|
| Encoder | H.264 (`ffmpeg`), High profile, no B-frames |
| Bitrate | 10 Mbps CBR, 0.5 s VBV |
| GOP | 15 frames (~0.5 s join / live edge) |
| Size | cap 1920×1080, **never upscale**, lanczos downscale |
| fps | 30 |

## Global watch link

1. Keep the origin running on the capture machine.
2. Deploy the Worker (below).
3. Set origin `cloudflare.publish_url` / `watch_url` (or the Settings fields in the UI).
4. Share:

```
https://<your-worker>.workers.dev/?token=<STREAM_TOKEN>
```

Anyone with that URL can watch. Treat the token like a stream key. Do not commit it.

The Worker homepage is a player with a **screen analysis + Ask** sidebar. `/watch` is the WebSocket; `/publish` is ingest from the origin; `/health` is public.

Analysis does not call DeepSeek until you attach a key:

```bash
cd cloudflare
npx wrangler secret put DEEPSEEK_API_KEY
```

Until then the APIs return `503` with setup instructions. The origin already publishes a JPEG snapshot (`type 4`) about every 8 seconds while capturing. Viewers never receive those snapshots — they stay in the Durable Object for the model.

## Cloudflare deploy

```bash
cd cloudflare
npm install
npx wrangler login          # once
npx wrangler secret put STREAM_TOKEN
npx wrangler deploy
```

Then on the origin (UI Settings, or `POST /api/config`):

```json
{
  "cloudflare": {
    "publish_url": "wss://<your-worker>.workers.dev/publish?token=<STREAM_TOKEN>",
    "watch_url": "wss://<your-worker>.workers.dev/watch?token=<STREAM_TOKEN>"
  }
}
```

Restart the origin after changing publish URL if it was already connected.

Optional: `cloudflare/cloudflared.yml` if you want the **UI** on a tunnel hostname. Do not send HTTP `/stream.mp4` through the tunnel.

## Streams (origin)

| Path | Role |
|------|------|
| `GET /stream.ws` | **Primary.** Binary: `1` = fMP4 init, `2` = fragment, `3` = JPEG, `4` = LLM snapshot (origin → Worker only) |
| `GET /stream.mp4` | LAN fallback, `Transfer-Encoding: chunked` (buffered on Tunnel) |
| `GET /stream.mjpeg` | MJPEG compatibility |

Player live-edge is ~0.45 s with `playbackRate` catch-up. It appends by type byte, not by sniffing `ftyp` in a TCP chunk.

## API (origin)

```
GET  /                UI
GET  /api/status      capture/stream JSON
GET  /api/config      full config        POST /api/config   apply+save
GET  /api/events      SSE
GET  /api/capture-devices
```

Optional token: `Authorization: Bearer`, `?token=`, or `streamaid_token` cookie.

## Screen analysis (Cloudflare Worker)

The Durable Object (`StreamRoom`) stores the latest JPEG snapshot, runs vision on it, and serves Q&A on the same watch page.

| Path | Role |
|------|------|
| `GET /api/analysis` | Latest summary, on-screen Q&A, history, `{configured, has_snapshot, model}` |
| `GET /api/llm-status` | Whether `DEEPSEEK_API_KEY` is set and a snapshot has arrived |
| `POST /api/analyze-now` | Force a vision pass now |
| `POST /api/ask` | `{ "question": "…" }` → `{ answer, confidence, reasoning }` |

Model defaults (override with wrangler `vars`):

- `DEEPSEEK_BASE_URL` = `https://api.deepseek.com`
- `DEEPSEEK_MODEL` = `deepseek-v4-flash-vision-exp`

Same-origin player polls `/api/analysis` and also accepts a WebSocket JSON message `{ "type": "analysis", "data": … }` when a pass finishes.

## Tests

```bash
cargo test                 # unit + HTTP API + player contract + stress
cargo test --test stress   # 30 fps hub budget, 16 WS clients, real 1080p30 ffmpeg encode
cd cloudflare && npm test  # Durable Object: 401, fan-out, late join, publisher replace, snap isolation, 503 without DeepSeek key
```

## Layout

```
src/            Rust origin (encode, hub, HTTP/WS, publisher)
web/            local UI
cloudflare/     Worker + StreamRoom Durable Object + public player
tests/          cargo tests (HTTP API, player contract, 1080p30 stress)
```
