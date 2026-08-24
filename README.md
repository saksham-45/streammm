# streamaid

LAN/cloud screen streaming with a browser UI: captures the host display at up to 1080p/30 fps and streams it to any browser, with configurable quality, latency tuning, an optional LLM screen-analysis loop, and an automated 3-minute image-quality monitor.

Python stdlib only; `ffmpeg` (and optionally `tesseract`) are the only external binaries.

## Quick start

```bash
cd streamaid
python3 -m streamaid            # config file: ./config.json (created with defaults)
# open http://<host-ip>:8080
```

Requirements: ffmpeg on PATH. macOS: grant Screen Recording permission to the terminal.

## Encoders

- `mjpeg` — universal, works everywhere (iOS uses a canvas renderer)
- `ffmpeg` — H.264, VideoToolbox hardware on macOS, `libx264` elsewhere
- `hevc` — Apple Silicon hardware HEVC (~2x compression of H.264); needs an HEVC-capable browser (Safari, Chrome on Apple hardware)

All encoder/capture/LLM settings apply live (no restart). `host`, `port`, `token` take effect on restart.

## Quality monitor

Every 3 minutes: decodes the live frame, scores sharpness (Laplacian variance vs a calibrated reference), readability (edge density + tesseract OCR median confidence when installed), and logs a WARNING + shows a red pill in the UI when the score drops below 90 on consecutive checks.

```
GET  /api/quality        → last result + history
POST /api/quality-check  → run one now
```

## API

```
GET  /                UI
GET  /stream.mjpeg    MJPEG stream (mjpeg mode)
GET  /stream.mp4      fragmented MP4 (ffmpeg/hevc modes)
GET  /api/status      capture/stream/llm/quality status
GET  /api/config      full config        POST /api/config   apply+save
GET  /api/analysis    LLM analysis history (last 50)
POST /api/ask         {"question": "..."}
POST /api/analyze-now
GET  /api/events      SSE (status/analysis/config-applied)
GET  /api/capture-devices
```

Token auth (opt-in via config `token`): `Authorization: Bearer`, `?token=`, or `streamaid_token` cookie.

## LLM analysis

OpenAI-compatible endpoint (works with Ollama). Enable in the UI, point `base_url`/`model` at any vision model that follows the JSON contract (verified with `llava-phi3` via Ollama).

## Platform notes

- macOS: avfoundation capture, device auto-detected at startup; VideoToolbox hardware encoding.
- Windows: gdigrab. Linux: x11grab (X11 only).
- Some virtual displays ignore `-framerate`; the capture pipeline throttles pre-encode to the configured fps regardless.

## Tests

```bash
python3 -m unittest discover -s tests
```
