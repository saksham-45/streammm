"""ffmpeg subprocess capture + frame framing.

Owns the ffmpeg subprocess; one reader thread frames stdout into full
encoded units (JPEG frames or MP4 fragments) and publishes them to the hub.
A second thread parses stderr for the stream resolution (MP4 mode).
"""

from __future__ import annotations

import logging
import platform
import re
import subprocess
import threading
import time

log = logging.getLogger("streamaid")

_MAX_FRAME_BUF = 20 * 1024 * 1024  # 20 MB buffer cap
_READ_SIZE = 65536

_STREAM_RE = re.compile(r"Stream #0:0[^\n]*? (\d{2,5})x(\d{2,5})")

_VIDEOTOOLBOX = None  # lazily probed


def _have_videotoolbox() -> bool:
    global _VIDEOTOOLBOX
    if _VIDEOTOOLBOX is None:
        try:
            out = subprocess.run(
                ["ffmpeg", "-hide_banner", "-encoders"],
                capture_output=True, text=True, timeout=15,
            )
            _VIDEOTOOLBOX = "h264_videotoolbox" in out.stdout
        except (FileNotFoundError, subprocess.TimeoutExpired):
            _VIDEOTOOLBOX = False
    return _VIDEOTOOLBOX


def jpeg_size(data: bytes):
    """Parse SOF0/SOF2 marker: bytes 5-6 height, 7-8 width, big-endian.

    Returns (0, 0) on parse failure.
    """
    n = len(data)
    if n < 4 or data[0:2] != b"\xff\xd8":
        return 0, 0
    i = 2
    while i + 9 <= n:
        if data[i] != 0xFF:
            i += 1
            continue
        m = data[i + 1]
        if m in (0xC0, 0xC2):  # SOF0 / SOF2
            h = (data[i + 5] << 8) | data[i + 6]
            w = (data[i + 7] << 8) | data[i + 8]
            return w, h
        if m in (0xD8, 0x01) or 0xD0 <= m <= 0xD7:  # standalone markers
            i += 2
            continue
        if i + 4 > n:
            return 0, 0
        seglen = (data[i + 2] << 8) | data[i + 3]
        if seglen < 2:
            return 0, 0
        i += 2 + seglen
    return 0, 0


def _scan_eoi(buf, start, in_scan):
    """Find EOI (FF D9) in ``buf`` from ``start``.

    Walks JPEG markers so embedded bytes cannot false-positive; handles FF 00
    stuffing inside entropy data. Returns (index, resume_pos, in_scan).
    ``in_scan`` is True once SOS has been passed and must be threaded through
    calls so a resume position inside entropy data is interpreted correctly.
    """
    n = len(buf)
    i = start
    while i + 1 < n:
        if buf[i] != 0xFF:
            i += 1
            continue
        m = buf[i + 1]
        if m == 0xD9:
            return i, i + 2, False
        if in_scan:
            if m == 0x00:  # stuffed byte
                i += 2
                continue
            if 0xD0 <= m <= 0xD7:  # restart marker
                i += 2
                continue
            return None, min(i + 2, n - 1), True  # corrupt; make progress
        if m in (0xD8, 0x01) or 0xD0 <= m <= 0xD7:
            i += 2
            continue
        if m == 0xDA:  # SOS: entropy data follows
            in_scan = True
            i += 2
            continue
        if i + 4 > n:
            return None, i, False
        seglen = (buf[i + 2] << 8) | buf[i + 3]
        if seglen < 2:
            return None, i, False
        i += 2 + seglen
    return None, i, in_scan


def _mp4_init_end(buf):
    """Offset just past the ftyp+moov init segment.

    Returns an int offset, None (need more data), or -1 (not a valid mp4 start).
    """
    n = len(buf)
    if n < 8 or buf[4:8] != b"ftyp":
        return -1
    pos = int.from_bytes(buf[0:4], "big")
    if pos < 8:
        return -1
    if pos > n:
        return None
    while True:
        if pos + 8 > n:
            return None
        size = int.from_bytes(buf[pos:pos + 4], "big")
        typ = bytes(buf[pos + 4:pos + 8])
        if size < 8:
            return -1
        if typ == b"moov":
            if n >= pos + size:
                return pos + size
            return None
        if typ in (b"free", b"skip", b"wide"):
            pos += size
            continue
        return -1


def _next_fragment(buf):
    """Length of the next complete moof(+mdat) fragment, or None.

    The frag_keyframe muxer writes one moof followed by one mdat per
    keyframe; a fragment is the moof plus its following mdat.
    """
    n = len(buf)
    if n < 8:
        return None
    size = int.from_bytes(buf[0:4], "big")
    typ = bytes(buf[4:8])
    if size < 8 or typ != b"moof" or n < size:
        return None
    after = size
    if after + 8 > n:
        return None
    msize = int.from_bytes(buf[after:after + 4], "big")
    mtyp = bytes(buf[after + 4:after + 8])
    if msize < 8:
        return None
    if mtyp == b"mdat":
        if n < size + msize:
            return None
        return size + msize
    return size  # lone moof


def enumerate_devices():
    """List capture devices. Darwin: live avfoundation enumeration.

    ``ffmpeg -f avfoundation -list_devices true -i ""`` exits non-zero by
    design; the device list is on stderr.
    """
    if platform.system() != "Darwin":
        out = [{"id": "desktop", "name": "Desktop"}]
        if platform.system() == "Linux":
            out.append({"id": ":0.0", "name": "X11 :0.0"})
        return out
    try:
        p = subprocess.run(
            ["ffmpeg", "-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
            capture_output=True, text=True, timeout=15,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return []
    devices = []
    for m in re.finditer(r"\[(\d+)\] (Capture screen \d+)", p.stderr):
        devices.append({"id": f"{m.group(1)}:", "name": m.group(2)})
    return devices


class Capture:
    """Owns the ffmpeg subprocess and frames its stdout for the hub."""

    def __init__(self):
        self._lock = threading.Lock()
        self._proc = None
        self._reader = None
        self._stderr_t = None
        self._stopping = False
        self._mode = "mjpeg"
        # state exposed to /api/status
        self.running = False
        self.status_error = ""
        self.width = 0
        self.height = 0
        self.input = ""
        self.frames_per_fragment = 1  # mp4 modes: GOP size (fragments are keyframes)

    # ---- lifecycle ----

    def start(self, cfg, hub) -> None:
        from .config import resolve_input  # local import avoids a cycle

        self._stopping = False
        self.input = resolve_input(cfg.capture.input)
        self._mode = cfg.encoder.mode
        self.frames_per_fragment = 10 if self._mode != "mjpeg" else 1
        argv = self._build_argv(cfg)
        try:
            proc = subprocess.Popen(
                argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                stdin=subprocess.DEVNULL,
            )
        except FileNotFoundError:
            self._set_error("ffmpeg not found on PATH")
            return
        except OSError as e:
            self._set_error(f"failed to start ffmpeg: {e}")
            return
        with self._lock:
            self._proc = proc
            self.status_error = ""
            self.running = True
        self._reader = threading.Thread(
            target=self._read_loop, args=(hub,), name="streamaid-capture", daemon=True
        )
        self._stderr_t = threading.Thread(
            target=self._stderr_loop, name="streamaid-ffmpeg-stderr", daemon=True
        )
        self._reader.start()
        self._stderr_t.start()

    def stop(self) -> None:
        self._stopping = True
        with self._lock:
            proc = self._proc
            self._proc = None
        if proc is None:
            return
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    pass
        for t in (self._reader, self._stderr_t):
            if t is not None and t.is_alive():
                t.join(timeout=5)
        self._reader = None
        self._stderr_t = None
        self.running = False

    def restart(self, cfg, hub) -> None:
        self.stop()
        self.start(cfg, hub)

    def _set_error(self, msg: str) -> None:
        with self._lock:
            self.status_error = msg
            self.running = False
        log.warning("capture: %s", msg)

    # ---- command construction ----

    def _build_argv(self, cfg):
        fps = cfg.capture.fps
        sysname = platform.system()
        if sysname == "Darwin":
            input_part = ["-f", "avfoundation", "-framerate", str(fps), "-i", self.input]
            use_vf = True
        elif sysname == "Windows":
            input_part = ["-f", "gdigrab", "-framerate", str(fps), "-i", "desktop"]
            use_vf = True
        else:  # Linux (X11 only; Wayland unsupported, error surfaces in status)
            input_part = ["-f", "x11grab", "-framerate", str(fps), "-i", self.input]
            use_vf = False

        if cfg.encoder.mode == "mjpeg":
            # loglevel error: JPEG framing needs no stderr metadata
            argv = ["ffmpeg", "-hide_banner", "-loglevel", "error", *input_part, "-an"]
            if use_vf:
                # avfoundation on this display ignores -framerate and can
                # deliver hundreds of fps (or 30 with unique PTS — device
                # dependent); fps={fps} is the reliable pre-encode throttle
                # (holds each frame at most one frame period — the sampling
                # floor of any fixed-rate stream).
                argv += ["-vf", f"setpts=PTS-STARTPTS,select='isnan(prev_pts)+gt(pts,prev_pts)',fps={fps},scale=trunc(iw*{cfg.capture.scale}/2)*2:trunc(ih*{cfg.capture.scale}/2)*2"]
            q = max(2, min(31, round(2 + (95 - cfg.capture.jpeg_quality) * 29 / 65)))
            argv += ["-c:v", "mjpeg", "-q:v", str(q), "-f", "mjpeg", "pipe:1"]
            return argv

        # H.264/HEVC mode. loglevel info so the Stream #0:0 line with the
        # output resolution appears on stderr (width/height contract for MP4).
        argv = ["ffmpeg", "-hide_banner", "-loglevel", "info", *input_part, "-an"]
        if use_vf:
            argv += ["-vf", f"setpts=PTS-STARTPTS,select='isnan(prev_pts)+gt(pts,prev_pts)',fps={fps},scale=trunc(iw*{cfg.capture.scale}/2)*2:trunc(ih*{cfg.capture.scale}/2)*2"]
        if cfg.encoder.mode == "hevc" and sysname == "Darwin":
            # HEVC via the Apple Silicon hardware encoder: ~2x the
            # compression of H.264 at the same quality. Playback needs a
            # HEVC-capable client (Safari/Chrome on Apple hardware).
            argv += ["-c:v", "hevc_videotoolbox", "-allow_sw", "1"]
        elif sysname == "Darwin" and _have_videotoolbox():
            argv += ["-c:v", "h264_videotoolbox", "-allow_sw", "1"]
        else:
            # hevc mode outside macOS falls back to H.264 software encode
            argv += ["-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency"]
        argv += [
            "-b:v", f"{cfg.encoder.bitrate_kbps}k",
            "-maxrate", f"{cfg.encoder.bitrate_kbps}k",
            "-bufsize", f"{2 * cfg.encoder.bitrate_kbps}k",
            # g=10: keyframe every ~1/3 s. Intra-only (g=1) at fixed bitrate
            # starves every frame (~33 KB @ 8 Mbps -> blurry text); P-frames
            # are near-free on screen content, so keyframes get ~10x the bit
            # budget. Join latency: client waits at most one GOP (~333 ms)
            # for the first keyframe; steady-state latency is unchanged.
            "-g", "10", "-keyint_min", "10", "-sc_threshold", "0",
            "-pix_fmt", "yuv420p",
            "-f", "mp4", "-movflags", "frag_keyframe+empty_moov+default_base_moof", "pipe:1",
        ]
        return argv

    # ---- reader loops ----

    def _read_loop(self, hub) -> None:
        proc = self._proc
        try:
            if self._mode == "mjpeg":
                self._read_mjpeg(hub, proc)
            else:
                self._read_mp4(hub, proc)
        except Exception as e:  # defensive: keep the failure visible, not silent
            with self._lock:
                if not self._stopping:
                    self.status_error = f"capture read error: {e}"
                    log.warning("capture read error: %s", e)
        finally:
            with self._lock:
                self.running = False
                if not self._stopping and proc.poll() is not None:
                    self.status_error = f"ffmpeg exited with code {proc.returncode}"
                    log.warning("ffmpeg exited with code %s", proc.returncode)

    def _read_mjpeg(self, hub, proc) -> None:
        buf = bytearray()
        scanning = True   # looking for SOI
        scan_from = 0     # search resume offset
        in_scan = False   # past SOS (entropy data)
        while True:
            chunk = proc.stdout.read(_READ_SIZE)
            if not chunk:
                break
            buf += chunk
            if scanning:
                idx = buf.find(b"\xff\xd8", scan_from)
                if idx == -1:
                    scan_from = max(0, len(buf) - 1)
                    continue
                del buf[:idx]
                scanning = False
                scan_from = 0
                in_scan = False
                continue
            idx, scan_from, in_scan = _scan_eoi(buf, scan_from, in_scan)
            if idx is None:
                if len(buf) > _MAX_FRAME_BUF:
                    # overflow: drop to next SOI
                    buf.clear()
                    scanning = True
                    scan_from = 0
                    in_scan = False
                continue
            frame = bytes(buf[: idx + 2])
            del buf[: idx + 2]
            scanning = True
            scan_from = 0
            in_scan = False
            w, h = jpeg_size(frame)
            self.width, self.height = w, h
            hub.publish("image/jpeg", frame, w, h)

    def _read_mp4(self, hub, proc) -> None:
        buf = bytearray()
        seen_init = False
        while True:
            chunk = proc.stdout.read(_READ_SIZE)
            if not chunk:
                break
            buf += chunk
            if not seen_init:
                end = _mp4_init_end(buf)
                if end == -1 or (end is None and len(buf) > _MAX_FRAME_BUF):
                    # junk/never-completing init: publish as a fragment and
                    # switch to fragment mode (client will error cleanly)
                    hub.publish("video/mp4-fragment", bytes(buf), 0, 0)
                    buf.clear()
                    seen_init = True
                    continue
                if end is None:
                    continue
                w, h = self._await_size()
                hub.publish("video/mp4-init", bytes(buf[:end]), w, h)
                del buf[:end]
                seen_init = True
            # publish complete moof(+mdat) fragments; keep partial tail buffered
            while True:
                frag = _next_fragment(buf)
                if frag is None:
                    break
                hub.publish("video/mp4-fragment", bytes(buf[:frag]), 0, 0)
                del buf[:frag]
            if len(buf) > _MAX_FRAME_BUF:
                buf.clear()

    def _await_size(self):
        """ffmpeg prints the Stream #0:0 line before the init segment reaches
        stdout; give the stderr thread a moment to parse it."""
        deadline = time.monotonic() + 1.0
        while self.width == 0 and time.monotonic() < deadline:
            time.sleep(0.01)
        return self.width, self.height

    def _stderr_loop(self) -> None:
        proc = self._proc
        if proc is None or proc.stderr is None:
            return
        try:
            for raw in proc.stderr:
                m = _STREAM_RE.search(raw.decode("utf-8", errors="replace"))
                if m:
                    self.width = int(m.group(1))
                    self.height = int(m.group(2))
        except Exception:
            pass  # stderr parsing is best-effort
