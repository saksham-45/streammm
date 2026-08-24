"""Configuration schema, load/save/clamp.

JSON file at ``-c`` path (default ``./config.json``). Missing file -> defaults
written immediately. Unknown keys silently ignored.
"""

from __future__ import annotations

import json
import os
import platform
from dataclasses import dataclass, field, fields as dc_fields
from pathlib import Path
from typing import Any

from .capture import enumerate_devices


def _clamp(value: Any, lo: int | float, hi: int | float, default: int | float) -> Any:
    try:
        v = type(default)(value)
    except (TypeError, ValueError):
        return default
    return max(lo, min(hi, v))


@dataclass
class CaptureConfig:
    driver: str = "ffmpeg"  # only supported value
    input: str = ""  # empty -> per-OS default, resolved at startup
    fps: int = 30  # clamp 1..60
    scale: float = 1.0  # clamp 0.25..1.0
    jpeg_quality: int = 90  # clamp 30..95

    def clamp(self) -> None:
        self.fps = int(_clamp(self.fps, 1, 60, 30))
        self.scale = float(_clamp(self.scale, 0.25, 1.0, 1.0))
        self.jpeg_quality = int(_clamp(self.jpeg_quality, 30, 95, 80))


@dataclass
class EncoderConfig:
    mode: str = "mjpeg"  # "mjpeg" | "ffmpeg" (H.264) | "hevc"
    bitrate_kbps: int = 20000  # clamp 500..50000

    def clamp(self) -> None:
        if self.mode not in ("mjpeg", "ffmpeg", "hevc"):
            self.mode = "mjpeg"
        self.bitrate_kbps = int(_clamp(self.bitrate_kbps, 500, 50000, 20000))


@dataclass
class LLMConfig:
    enabled: bool = False
    base_url: str = "http://127.0.0.1:11434/v1"
    api_key: str = "ollama"
    model: str = "llama3.2-vision"
    interval_sec: int = 5  # clamp 2..3600
    prompt: str = ""  # empty -> DEFAULT_PROMPT from llm.py

    def clamp(self) -> None:
        self.enabled = bool(self.enabled)
        self.interval_sec = int(_clamp(self.interval_sec, 2, 3600, 5))


@dataclass
class Config:
    host: str = "0.0.0.0"
    port: int = 8080
    token: str = ""
    capture: CaptureConfig = field(default_factory=CaptureConfig)
    encoder: EncoderConfig = field(default_factory=EncoderConfig)
    llm: LLMConfig = field(default_factory=LLMConfig)

    def clamp(self) -> None:
        try:
            self.port = int(self.port)
        except (TypeError, ValueError):
            self.port = 8080
        self.capture.clamp()
        self.encoder.clamp()
        self.llm.clamp()

    def to_dict(self) -> dict:
        return {
            "host": self.host,
            "port": self.port,
            "token": self.token,
            "capture": {
                "driver": self.capture.driver,
                "input": self.capture.input,
                "fps": self.capture.fps,
                "scale": self.capture.scale,
                "jpeg_quality": self.capture.jpeg_quality,
            },
            "encoder": {
                "mode": self.encoder.mode,
                "bitrate_kbps": self.encoder.bitrate_kbps,
            },
            "llm": {
                "enabled": self.llm.enabled,
                "base_url": self.llm.base_url,
                "api_key": self.llm.api_key,
                "model": self.llm.model,
                "interval_sec": self.llm.interval_sec,
                "prompt": self.llm.prompt,
            },
        }

    @classmethod
    def from_dict(cls, d: dict) -> "Config":
        cfg = cls()
        for f in dc_fields(cls):
            if f.name in d and f.name not in ("capture", "encoder", "llm"):
                setattr(cfg, f.name, d[f.name])
        cap = d.get("capture") or {}
        for f in dc_fields(CaptureConfig):
            if f.name in cap:
                setattr(cfg.capture, f.name, cap[f.name])
        enc = d.get("encoder") or {}
        for f in dc_fields(EncoderConfig):
            if f.name in enc:
                setattr(cfg.encoder, f.name, enc[f.name])
        llm = d.get("llm") or {}
        for f in dc_fields(LLMConfig):
            if f.name in llm:
                setattr(cfg.llm, f.name, llm[f.name])
        cfg.clamp()
        return cfg


def default_input() -> str:
    """Per-OS default capture input, resolved at startup."""
    sysname = platform.system()
    if sysname == "Darwin":
        devices = enumerate_devices()
        if devices:
            return devices[0]["id"]
        return "3:"
    if sysname == "Windows":
        return "desktop"
    return os.environ.get("DISPLAY", ":0.0")


def resolve_input(raw: str) -> str:
    return raw if raw else default_input()


def load(path: str | Path) -> Config:
    p = Path(path)
    if not p.exists():
        cfg = Config()
        save(cfg, p)
        return cfg
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        raise ValueError(f"config {path}: {e}") from e
    if not isinstance(data, dict):
        raise ValueError(f"config {path}: expected JSON object")
    return Config.from_dict(data)


def save(cfg: Config, path: str | Path) -> None:
    Path(path).write_text(
        json.dumps(cfg.to_dict(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
