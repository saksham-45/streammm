"""Periodic stream quality monitoring: sharpness/readability metrics.

Every 3 minutes the monitor decodes the latest streamed frame, measures
sharpness (Laplacian variance) and readability (edge density) against a
calibrated reference (the sharpest frame from the startup warmup), and
confirms human readability with OCR (tesseract, when installed). A score
below 90 logs a warning. Every result is kept in a history ring.
"""

from __future__ import annotations

import collections
import logging
import shutil
import subprocess
import threading
import time
from datetime import datetime, timezone

log = logging.getLogger("streamaid")

CHECK_INTERVAL = 180.0  # every 3 minutes
WARMUP_DELAYS = (10.0, 20.0, 30.0)  # calibration checks at startup
EDGE_THRESHOLD = 40.0  # gradient magnitude that counts as a readable edge
HISTORY_LEN = 48  # ~2.5 hours of 3-minute checks

_W = 960
_H = 540
_FW = 1920
_FH = 1080

_HAS_TESSERACT = None


class QualityError(Exception):
    pass


def _iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _has_tesseract() -> bool:
    global _HAS_TESSERACT
    if _HAS_TESSERACT is None:
        _HAS_TESSERACT = shutil.which("tesseract") is not None
    return _HAS_TESSERACT


def decode_gray(data: bytes, mime: str, w: int = _W, h: int = _H):
    """Decode a JPEG frame, or an MP4 init+fragment pair, to grayscale.

    Returns the raw Y-plane bytes (w x h), or None on decode failure.
    """
    if mime not in ("image/jpeg", "video/mp4-fragment"):
        return None
    try:
        proc = subprocess.run(
            ["ffmpeg", "-loglevel", "error", "-i", "pipe:0",
             "-vf", f"scale={w}:{h}", "-frames:v", "1", "-pix_fmt", "gray",
             "-f", "rawvideo", "pipe:1"],
            input=data, capture_output=True, timeout=30,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if proc.returncode != 0 or len(proc.stdout) != w * h:
        return None
    return proc.stdout


def _downscale2(y):
    """2x2 box-downscale a 2w x 2h gray plane to w x h."""
    w, h = _FW // 2, _FH // 2
    out = bytearray(w * h)
    for j in range(h):
        r0 = j * 2 * _FW
        r1 = r0 + _FW
        for i in range(w):
            i2 = i * 2
            out[j * w + i] = (y[r0 + i2] + y[r0 + i2 + 1] + y[r1 + i2] + y[r1 + i2 + 1]) // 4
    return bytes(out)


def frame_metrics(y):
    """Sharpness/readability metrics over a _W x _H gray plane.

    - laplacian_var: variance of the 3x3 Laplacian (blur detector)
    - edge_density: fraction of pixels with strong horizontal+vertical
      gradients (text/UI readability proxy)
    - contrast: luminance standard deviation
    """
    w, h = _W, _H
    total = 0
    for v in y:
        total += v
    mean = total / (w * h)
    var = 0.0
    lap_sq = 0.0
    lap_n = 0
    edges = 0
    for j in range(1, h - 1):
        row = j * w
        prev = row - w
        nxt = row + w
        for i in range(1, w - 1):
            idx = row + i
            v = y[idx]
            lap = y[prev + i] + y[nxt + i] + y[idx - 1] + y[idx + 1] - 4 * v
            lap_sq += lap * lap
            lap_n += 1
            if abs(y[idx + 1] - y[idx - 1]) + abs(y[nxt + i] - y[prev + i]) > EDGE_THRESHOLD:
                edges += 1
            var += (v - mean) * (v - mean)
    return {
        "laplacian_var": lap_sq / lap_n if lap_n else 0.0,
        "edge_density": edges / ((w - 2) * (h - 2)),
        "contrast": (var / (w * h)) ** 0.5,
    }


def frame_to_jpeg(data: bytes, mime: str):
    """Return a JPEG of the frame: passthrough for JPEG, ffmpeg-decode for
    MP4 fragments (init must be prepended). None on failure."""
    if mime == "image/jpeg":
        return data
    if mime != "video/mp4-fragment":
        return None
    try:
        proc = subprocess.run(
            ["ffmpeg", "-loglevel", "error", "-i", "pipe:0", "-frames:v", "1",
             "-f", "mjpeg", "-q:v", "3", "pipe:1"],
            input=data, capture_output=True, timeout=30,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if proc.returncode != 0 or not proc.stdout.startswith(b"\xff\xd8"):
        return None
    return proc.stdout


def ocr_confidence(y):
    """OCR the full-res gray plane with tesseract.

    Returns (median_word_confidence, word_count); word_count 0 on failure
    or when no text is visible (fine for non-text screens). Median is
    robust to the low-confidence tail of small/antialiased UI text.
    """
    if not _has_tesseract():
        return None, 0
    pgm = b"P5\n%d %d\n255\n" % (_FW, _FH) + y
    try:
        proc = subprocess.run(
            ["tesseract", "stdin", "stdout", "-l", "eng", "--psm", "11", "tsv"],
            input=pgm, capture_output=True, timeout=30,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None, 0
    confs = []
    words = 0
    for line in proc.stdout.decode("utf-8", errors="replace").splitlines()[1:]:
        cols = line.split("\t")
        if len(cols) < 12:
            continue
        try:
            conf = float(cols[10])
        except ValueError:
            continue
        if cols[11].strip() and conf >= 0:
            confs.append(conf)
            words += 1
    if not confs:
        return None, words
    confs.sort()
    return confs[len(confs) // 2], words


class QualityMonitor:
    """Background thread; warms up references at startup, then checks every
    3 minutes. Exposes the last result and a history ring."""

    def __init__(self, hub, capture):
        self._hub = hub
        self._capture = capture
        self._lock = threading.Lock()
        self._thread = None
        self._ref_lap = None
        self._ref_edge = None
        self._warmup = 0
        self._fails = 0
        self._history = collections.deque(maxlen=HISTORY_LEN)
        self._last = {
            "last_check_at": "",
            "score": 0,
            "sharpness": 0,
            "readability": 0,
            "ocr_confidence": None,
            "ocr_words": 0,
            "ok": False,
            "error": "",
        }

    def start(self) -> None:
        if self._thread is None:
            self._thread = threading.Thread(
                target=self._loop, name="streamaid-quality", daemon=True
            )
            self._thread.start()

    def status(self) -> dict:
        with self._lock:
            return dict(self._last)

    def history(self) -> list:
        with self._lock:
            return [dict(e) for e in self._history]

    def _loop(self) -> None:
        delay = 10.0
        while True:
            time.sleep(delay)
            try:
                self.check_once()
            except Exception as e:  # keep the loop alive
                with self._lock:
                    self._last = {
                        "last_check_at": _iso(),
                        "score": 0,
                        "sharpness": 0,
                        "readability": 0,
                        "ocr_confidence": None,
                        "ocr_words": 0,
                        "ok": False,
                        "error": str(e),
                    }
                    self._history.appendleft(dict(self._last))
                log.warning("quality check failed: %s", e)
            with self._lock:
                warmup = self._warmup
            if warmup >= len(WARMUP_DELAYS):
                delay = CHECK_INTERVAL
            elif warmup > 0:
                delay = WARMUP_DELAYS[warmup]
            else:
                delay = WARMUP_DELAYS[0]

    def check_once(self) -> dict:
        latest = self._hub.latest()
        if latest is None:
            raise QualityError("no frame available")
        mime, data, w, h, ts = latest
        if mime == "image/jpeg":
            payload = data
        elif mime == "video/mp4-fragment":
            payload = (self._hub.init_segment() or b"") + data
        else:
            raise QualityError(f"unexpected frame type: {mime}")
        full = decode_gray(payload, mime, _FW, _FH)
        if full is None:
            raise QualityError("frame decode failed")
        m = frame_metrics(_downscale2(full))
        ocr_conf, ocr_words = ocr_confidence(full)
        with self._lock:
            if self._warmup < len(WARMUP_DELAYS):
                # calibration: keep the sharpest frame seen so far
                self._ref_lap = m["laplacian_var"] if self._ref_lap is None \
                    else max(self._ref_lap, m["laplacian_var"])
                self._ref_edge = m["edge_density"] if self._ref_edge is None \
                    else max(self._ref_edge, m["edge_density"])
                self._warmup += 1
                sharp = readable = score = 100
                ok = True
            else:
                # grow-only reference: new sharper content re-baselines up;
                # never decays, so genuine degradation still scores low
                self._ref_lap = max(self._ref_lap, m["laplacian_var"]) if self._ref_lap else m["laplacian_var"]
                self._ref_edge = max(self._ref_edge, m["edge_density"]) if self._ref_edge else m["edge_density"]
                sharp = min(100, round(m["laplacian_var"] / self._ref_lap * 100))
                edge_ratio = min(100, round(m["edge_density"] / self._ref_edge * 100))
                readable = edge_ratio
                if ocr_conf is not None and ocr_words >= 3:
                    # literal human-readability confirmation via OCR; keep the
                    # edge metric as the floor when OCR is unavailable
                    readable = min(edge_ratio, round(ocr_conf))
                score = min(sharp, readable)
                ok = score >= 90
                self._fails = self._fails + 1 if not ok else 0
            self._last = {
                "last_check_at": _iso(),
                "score": score,
                "sharpness": sharp,
                "readability": readable,
                "ocr_confidence": round(ocr_conf) if ocr_conf is not None else None,
                "ocr_words": ocr_words,
                "ok": ok,
                "error": "",
            }
            self._history.appendleft(dict(self._last))
            # warn only on consecutive failures: single-check dips are usually
            # content changes (e.g. motion blur), not stream degradation
            if self._warmup >= len(WARMUP_DELAYS) and self._fails >= 2:
                log.warning("stream quality low: sharpness=%d%% readability=%d%%", sharp, readable)
        return dict(self._last)
