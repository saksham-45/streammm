"""HTTP routes, SSE, auth, and live config apply."""

from __future__ import annotations

import collections
import hmac
import json
import logging
import queue
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from . import __version__
from .capture import Capture, enumerate_devices
from .config import Config, save
from .hub import FrameHub
from .llm import Analyzer, LLMError
from .quality import QualityError, QualityMonitor

log = logging.getLogger("streamaid")

WEB_DIR = Path(__file__).parent / "web"

PUBLIC_PATHS = {"/", "/app.js", "/style.css"}


class Broadcaster:
    """Condition + per-subscriber deque (cap 100, drop oldest) for SSE."""

    def __init__(self):
        self._cond = threading.Condition()
        self._subs = {}
        self._next = 0

    def subscribe(self):
        with self._cond:
            sid = self._next
            self._next += 1
            dq = collections.deque(maxlen=100)
            self._subs[sid] = dq
            return sid, dq

    def unsubscribe(self, sid: int) -> None:
        with self._cond:
            self._subs.pop(sid, None)

    def publish(self, name: str, data) -> None:
        with self._cond:
            if not self._subs:
                return
            ev = "event: {}\ndata: {}\n\n".format(
                name, json.dumps(data, separators=(",", ":"))
            )
            for dq in self._subs.values():
                dq.append(ev)
            self._cond.notify_all()

    def wait(self, dq, timeout: float):
        """Block until an event is available; returns the event or None."""
        with self._cond:
            if dq:
                return dq.popleft()
            self._cond.wait(timeout)
            return dq.popleft() if dq else None


class StreamServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, addr, cfg: Config, cfg_path: str, hub: FrameHub, capture: Capture):
        super().__init__(addr, Handler)
        self.cfg = cfg
        self.cfg_path = cfg_path
        self.hub = hub
        self.capture = capture
        self.broadcaster = Broadcaster()
        self.history = collections.deque(maxlen=50)
        self.started_at = time.time()
        self.config_lock = threading.Lock()
        self.analyzer = None  # set via set_analyzer
        self.quality = QualityMonitor(hub, capture)
        self.quality.start()
        threading.Thread(target=self._status_loop, name="streamaid-status", daemon=True).start()

    def set_analyzer(self, analyzer: Analyzer) -> None:
        self.analyzer = analyzer

    def publish_analysis(self, analysis_dict: dict) -> None:
        self.history.appendleft(analysis_dict)
        self.broadcaster.publish("analysis", analysis_dict)

    def _status_loop(self) -> None:
        while True:
            time.sleep(2)
            self.broadcaster.publish("status", self.status_dict())

    def status_dict(self) -> dict:
        cfg = self.cfg
        cap = self.capture
        an = self.analyzer
        return {
            "version": __version__,
            "uptime_s": time.time() - self.started_at,
            "capture": {
                "input": cap.input or cfg.capture.input,
                "width": cap.width,
                "height": cap.height,
                "fps_target": cfg.capture.fps,
                "fps_actual": self.hub.fps() * cap.frames_per_fragment,
                "running": cap.running,
                "error": cap.status_error,
            },
            "stream": {
                "mode": cfg.encoder.mode,
                "clients": self.hub.clients(),
                "bitrate_kbps": cfg.encoder.bitrate_kbps,
                "jpeg_quality": cfg.capture.jpeg_quality,
                "scale": cfg.capture.scale,
            },
            "llm": {
                "enabled": cfg.llm.enabled,
                "model": cfg.llm.model,
                "interval_sec": cfg.llm.interval_sec,
                "last_run_at": an.last_run_at if an else "",
                "last_error": an.last_error if an else "",
                "active": an.active if an else False,
            },
            "quality": self.quality.status(),
        }

    def apply_config(self, new_cfg: Config) -> dict:
        """Clamp/save/live-apply. Returns {applied, restart_required, note}."""
        with self.config_lock:
            old = self.cfg
            restart_required = (
                old.host != new_cfg.host or old.port != new_cfg.port or old.token != new_cfg.token
            )
            cap_changed = old.capture != new_cfg.capture
            enc_changed = old.encoder != new_cfg.encoder
            llm_changed = old.llm != new_cfg.llm
            self.cfg = new_cfg
            save(new_cfg, self.cfg_path)
        if cap_changed or enc_changed:
            # clear FIRST: the restart's fresh init/fragments must be published
            # after the wipe, or new subscribers would get fragments with no init
            self.hub.clear()
            self.capture.restart(new_cfg, self.hub)
        if llm_changed and self.analyzer is not None:
            self.analyzer.reconfigure(new_cfg.llm)
        note = "host/port/token changes take effect on restart" if restart_required else ""
        return {"applied": True, "restart_required": restart_required, "note": note}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "streamaid/" + __version__

    def log_message(self, fmt, *args):
        pass  # request spam is noise; failures surface via /api/status

    # ---- plumbing ----

    def _json(self, code: int, obj) -> None:
        data = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _stream_headers(self, code: int, ctype: str) -> None:
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()

    def _read_body(self) -> bytes:
        try:
            n = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            n = 0
        if n <= 0:
            return b""
        return self.rfile.read(n)

    def _authorized(self) -> bool:
        srv = self.server
        token = srv.cfg.token
        if not token:
            return True
        auth = self.headers.get("Authorization", "")
        if auth.startswith("Bearer "):
            given = auth[7:]
        else:
            qs = parse_qs(urlparse(self.path).query)
            given = qs.get("token", [""])[0]
            if not given:
                for part in self.headers.get("Cookie", "").split(";"):
                    k, _, v = part.strip().partition("=")
                    if k == "streamaid_token":
                        given = v
                        break
        return hmac.compare_digest(given, token)

    def _dispatch(self, method: str) -> None:
        path = urlparse(self.path).path
        if (method, path) not in ROUTES:
            self._json(404, {"error": "not found"})
            return
        if path not in PUBLIC_PATHS and not self._authorized():
            self._json(401, {"error": "unauthorized"})
            return
        try:
            ROUTES[(method, path)](self)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        self._dispatch("GET")

    def do_POST(self):
        self._dispatch("POST")

    # ---- streaming endpoints (close-delimited, no Content-Length) ----

    @staticmethod
    def _write_mjpeg_part(wfile, data: bytes) -> None:
        wfile.write(
            b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: %d\r\n\r\n%s\r\n"
            % (len(data), data)
        )
        wfile.flush()

    def _stream_mjpeg(self) -> None:
        srv = self.server
        if srv.cfg.encoder.mode != "mjpeg":
            self._json(409, {"error": "encoder mode is not mjpeg; use /stream.mp4"})
            return
        self._stream_headers(200, "multipart/x-mixed-replace; boundary=frame")
        latest = srv.hub.latest()
        if latest is not None and latest[0] == "image/jpeg":
            self._write_mjpeg_part(self.wfile, latest[1])
        sid, q = srv.hub.subscribe()
        try:
            while True:
                try:
                    item = q.get(timeout=1.0)
                except queue.Empty:
                    if not srv.capture.running:
                        break
                    continue
                if item[0] != "image/jpeg":
                    continue
                self._write_mjpeg_part(self.wfile, item[1])
        finally:
            srv.hub.unsubscribe(sid)

    def _stream_mp4(self) -> None:
        srv = self.server
        if srv.cfg.encoder.mode not in ("ffmpeg", "hevc"):
            self._json(409, {"error": "encoder mode is mjpeg; use /stream.mjpeg"})
            return
        self._stream_headers(200, "video/mp4")
        init = srv.hub.init_segment()
        gen = srv.hub.generation()
        if init:
            self.wfile.write(init)
            self.wfile.flush()
        sid, q = srv.hub.subscribe(maxsize=32)
        try:
            while True:
                try:
                    item = q.get(timeout=1.0)
                except queue.Empty:
                    if not srv.capture.running or srv.hub.generation() != gen:
                        break
                    continue
                if srv.hub.generation() != gen:
                    break  # stream restarted: force the client to reconnect
                self.wfile.write(item[1])
                self.wfile.flush()
        finally:
            srv.hub.unsubscribe(sid)

    def _api_events(self) -> None:
        srv = self.server
        self._stream_headers(200, "text/event-stream")
        sid, dq = srv.broadcaster.subscribe()
        try:
            while True:
                ev = srv.broadcaster.wait(dq, 2.0)
                if ev is None:
                    continue
                self.wfile.write(ev.encode("utf-8"))
                self.wfile.flush()
        finally:
            srv.broadcaster.unsubscribe(sid)

    # ---- API endpoints ----

    def _api_status(self) -> None:
        self._json(200, self.server.status_dict())

    def _api_config_get(self) -> None:
        self._json(200, self.server.cfg.to_dict())

    def _api_config_post(self) -> None:
        srv = self.server
        body = self._read_body()
        try:
            data = json.loads(body.decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            self._json(400, {"error": "invalid JSON body"})
            return
        if not isinstance(data, dict):
            self._json(400, {"error": "expected JSON object"})
            return
        # deep-merge partial bodies over the current config, then clamp/save
        cur = srv.cfg.to_dict()
        for k in ("capture", "encoder", "llm"):
            if isinstance(data.get(k), dict) and isinstance(cur.get(k), dict):
                cur[k].update(data[k])
            elif k in data:
                cur[k] = data[k]
        for k in ("host", "port", "token"):
            if k in data:
                cur[k] = data[k]
        new_cfg = Config.from_dict(cur)
        result = srv.apply_config(new_cfg)
        srv.broadcaster.publish("config-applied", result)
        self._json(200, result)

    def _api_analysis(self) -> None:
        self._json(200, list(self.server.history))

    def _api_ask(self) -> None:
        srv = self.server
        if not srv.cfg.llm.enabled or srv.analyzer is None:
            self._json(502, {"error": "LLM disabled"})
            return
        body = self._read_body()
        try:
            data = json.loads(body.decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            self._json(400, {"error": "invalid JSON body"})
            return
        question = (data or {}).get("question")
        if not isinstance(question, str) or not question.strip():
            self._json(400, {"error": "missing question"})
            return
        try:
            result = srv.analyzer.ask(question)
        except LLMError as e:
            self._json(502, {"error": str(e)})
            return
        self._json(200, result)

    def _api_analyze_now(self) -> None:
        srv = self.server
        if not srv.cfg.llm.enabled or srv.analyzer is None:
            self._json(502, {"error": "LLM disabled"})
            return
        try:
            result = srv.analyzer.run_now()
        except LLMError as e:
            self._json(502, {"error": str(e)})
            return
        self._json(200, result)

    def _api_devices(self) -> None:
        self._json(200, enumerate_devices())

    def _api_quality_check(self) -> None:
        try:
            result = self.server.quality.check_once()
        except QualityError as e:
            self._json(502, {"error": str(e)})
            return
        self._json(200, result)

    def _api_quality(self) -> None:
        q = self.server.quality
        self._json(200, {"last": q.status(), "history": q.history()})

    def _web_file(self, name: str) -> None:
        try:
            data = (WEB_DIR / name).read_bytes()
        except OSError:
            self._json(404, {"error": "not found"})
            return
        ctype = {"app.js": "application/javascript", "style.css": "text/css"}.get(name, "text/html")
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


ROUTES = {
    ("GET", "/"): lambda h: h._web_file("index.html"),
    ("GET", "/app.js"): lambda h: h._web_file("app.js"),
    ("GET", "/style.css"): lambda h: h._web_file("style.css"),
    ("GET", "/stream.mjpeg"): lambda h: h._stream_mjpeg(),
    ("GET", "/stream.mp4"): lambda h: h._stream_mp4(),
    ("GET", "/api/status"): lambda h: h._api_status(),
    ("GET", "/api/config"): lambda h: h._api_config_get(),
    ("POST", "/api/config"): lambda h: h._api_config_post(),
    ("GET", "/api/analysis"): lambda h: h._api_analysis(),
    ("POST", "/api/ask"): lambda h: h._api_ask(),
    ("POST", "/api/analyze-now"): lambda h: h._api_analyze_now(),
    ("GET", "/api/events"): lambda h: h._api_events(),
    ("GET", "/api/capture-devices"): lambda h: h._api_devices(),
    ("POST", "/api/quality-check"): lambda h: h._api_quality_check(),
    ("GET", "/api/quality"): lambda h: h._api_quality(),
}
