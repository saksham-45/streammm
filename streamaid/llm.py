"""LLM client, prompts, and the background Analyzer thread."""

from __future__ import annotations

import base64
import json
import logging
import threading
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone

from .quality import frame_to_jpeg

log = logging.getLogger("streamaid")

DEFAULT_PROMPT = (
    "You are a screen-analysis assistant for gaming and streaming. Analyze the provided screen capture. "
    "1) Summarize what is on screen in 2-3 sentences. "
    "2) If any question is visible on screen (game UI, chat, quiz, or prompt text), answer it. "
    "3) For each question give an answer, a confidence score 0-100, and one-sentence reasoning. "
    'Respond ONLY with valid JSON: {"summary": string, "questions": [{"question": string, "answer": string, '
    '"confidence": number, "reasoning": string}]}. If no question is visible, set questions to [].'
)

ASK_PROMPT = (
    "Answer this question about the current screen capture.\n"
    "Question: {question}\n"
    'Respond ONLY with valid JSON: {{"answer": string, "confidence": number (0-100), "reasoning": string}}.'
)


class LLMError(Exception):
    pass


class LLMClient:
    """OpenAI-compatible chat/completions client (works with Ollama)."""

    def __init__(self, base_url: str, api_key: str, model: str):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.model = model

    def complete(self, prompt: str, jpeg: bytes, timeout: int = 120) -> str:
        url = self.base_url + "/chat/completions"
        body = {
            "model": self.model,
            "temperature": 0.2,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": prompt},
                    {"type": "image_url", "image_url": {
                        "url": "data:image/jpeg;base64," + base64.b64encode(jpeg).decode("ascii")}},
                ],
            }],
        }
        req = urllib.request.Request(
            url,
            data=json.dumps(body).encode("utf-8"),
            method="POST",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                data = json.loads(resp.read().decode("utf-8"))
        except Exception as e:
            raise LLMError(str(e)) from e
        try:
            content = data["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError):
            raise LLMError("invalid response from model endpoint") from None
        if not isinstance(content, str) or not content.strip():
            raise LLMError("empty response from model endpoint")
        return content


def parse_json_tolerant(text: str) -> dict:
    """Strip code fences, find the first balanced JSON object, parse it."""
    s = text.strip()
    if s.startswith("```"):
        nl = s.find("\n")
        if nl != -1:
            s = s[nl + 1:]
        if s.endswith("```"):
            s = s[:-3]
        s = s.strip()
    start = s.find("{")
    if start == -1:
        raise LLMError("invalid JSON from model")
    depth = 0
    in_str = False
    esc = False
    for i in range(start, len(s)):
        c = s[i]
        if in_str:
            if esc:
                esc = False
            elif c == "\\":
                esc = True
            elif c == '"':
                in_str = False
            continue
        if c == '"':
            in_str = True
        elif c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                try:
                    return json.loads(s[start:i + 1])
                except json.JSONDecodeError:
                    raise LLMError("invalid JSON from model") from None
    raise LLMError("invalid JSON from model")


@dataclass
class Question:
    question: str = ""
    answer: str = ""
    confidence: int = 0
    reasoning: str = ""

    @classmethod
    def from_dict(cls, d) -> "Question":
        if not isinstance(d, dict):
            return cls()
        try:
            conf = int(d.get("confidence", 0) or 0)
        except (TypeError, ValueError):
            conf = 0
        return cls(
            question=str(d.get("question", "")),
            answer=str(d.get("answer", "")),
            confidence=max(0, min(100, conf)),
            reasoning=str(d.get("reasoning", "")),
        )

    def to_dict(self) -> dict:
        return {
            "question": self.question,
            "answer": self.answer,
            "confidence": self.confidence,
            "reasoning": self.reasoning,
        }


@dataclass
class Analysis:
    ts: str  # ISO 8601 UTC
    summary: str = ""
    questions: list = field(default_factory=list)  # list[Question]

    @classmethod
    def from_dict(cls, d, ts: str) -> "Analysis":
        if not isinstance(d, dict):
            return cls(ts=ts)
        qs = d.get("questions")
        questions = [Question.from_dict(q) for q in qs] if isinstance(qs, list) else []
        return cls(ts=ts, summary=str(d.get("summary", "")), questions=questions)

    def to_dict(self) -> dict:
        return {
            "ts": self.ts,
            "summary": self.summary,
            "questions": [q.to_dict() for q in self.questions],
        }


class Analyzer:
    """Periodic screen analysis in a background thread.

    The loop sleeps on an Event for ``interval_sec``; ``reconfigure`` wakes it
    so enabled/disabled and interval changes apply without restart.
    """

    def __init__(self, llm_cfg, hub, on_analysis=None):
        self._hub = hub
        self._on_analysis = on_analysis
        self._lock = threading.Lock()
        self._event = threading.Event()
        self._thread = None
        self._active = False
        self._last = None            # Analysis | None
        self._last_run_at = ""
        self._last_error = ""
        self._enabled = llm_cfg.enabled
        self._interval = llm_cfg.interval_sec
        self._prompt = llm_cfg.prompt
        self._client = LLMClient(llm_cfg.base_url, llm_cfg.api_key, llm_cfg.model)

    def start(self) -> None:
        if self._thread is None:
            self._thread = threading.Thread(
                target=self._loop, name="streamaid-analyzer", daemon=True
            )
            self._thread.start()

    # ---- read-only state ----

    @property
    def last(self):
        with self._lock:
            return self._last

    @property
    def last_run_at(self) -> str:
        with self._lock:
            return self._last_run_at

    @property
    def last_error(self) -> str:
        with self._lock:
            return self._last_error

    @property
    def active(self) -> bool:
        with self._lock:
            return self._active

    # ---- loop ----

    def _loop(self) -> None:
        while True:
            with self._lock:
                enabled = self._enabled
                interval = self._interval
            self._event.clear()
            if not enabled:
                self._event.wait()
                continue
            self._event.wait(interval)
            with self._lock:
                enabled = self._enabled
            if not enabled:
                continue
            try:
                self.run_once()
            except Exception as e:  # defensive: keep the loop alive
                with self._lock:
                    self._last_error = str(e)
                log.warning("llm: %s", e)

    def run_once(self) -> None:
        """Run one analysis. Skips when a previous run is still in flight."""
        with self._lock:
            if self._active:
                return
            self._active = True
            client = self._client
            prompt = self._prompt or DEFAULT_PROMPT
        try:
            frame = self._hub.latest()
            if frame is None:
                return
            mime, data, w, h, ts = frame
            if mime == "image/jpeg":
                jpeg = data
            elif mime == "video/mp4-fragment":
                jpeg = frame_to_jpeg((self._hub.init_segment() or b"") + data, mime)
            else:
                return
            if jpeg is None:
                return
            try:
                parsed = self._complete_parsed(client, prompt, jpeg)
            except LLMError as e:
                with self._lock:
                    self._last_error = str(e)
                log.warning("llm: %s", e)
                return
            a = Analysis.from_dict(parsed, datetime.now(timezone.utc).isoformat())
            with self._lock:
                self._last = a
                self._last_run_at = a.ts
                self._last_error = ""
            if self._on_analysis is not None:
                self._on_analysis(a.to_dict())
        finally:
            with self._lock:
                self._active = False

    @staticmethod
    def _complete_parsed(client, prompt: str, jpeg: bytes) -> dict:
        text = client.complete(prompt, jpeg)
        try:
            return parse_json_tolerant(text)
        except LLMError:
            # one retry with an explicit JSON-only reminder (run_once only)
            text = client.complete(prompt + "\nRespond ONLY with valid JSON.", jpeg)
            return parse_json_tolerant(text)

    def ask(self, question: str) -> dict:
        """Synchronous ad-hoc question; runs in the request thread."""
        frame = self._hub.latest()
        if frame is None:
            raise LLMError("no screen frame available")
        mime, data, w, h, ts = frame
        if mime == "image/jpeg":
            jpeg = data
        elif mime == "video/mp4-fragment":
            jpeg = frame_to_jpeg((self._hub.init_segment() or b"") + data, mime)
        else:
            raise LLMError("no screen frame available")
        if jpeg is None:
            raise LLMError("frame decode failed")
        with self._lock:
            client = self._client
        text = client.complete(ASK_PROMPT.format(question=question), jpeg)
        parsed = parse_json_tolerant(text)
        try:
            conf = int(parsed.get("confidence", 0) or 0)
        except (TypeError, ValueError):
            conf = 0
        return {
            "answer": str(parsed.get("answer", "")),
            "confidence": max(0, min(100, conf)),
            "reasoning": str(parsed.get("reasoning", "")),
        }

    def run_now(self) -> dict:
        self.run_once()
        with self._lock:
            if self._last is None:
                raise LLMError("no analysis yet")
            return self._last.to_dict()

    def reconfigure(self, cfg) -> None:
        with self._lock:
            self._enabled = cfg.enabled
            self._interval = cfg.interval_sec
            self._prompt = cfg.prompt
            self._client = LLMClient(cfg.base_url, cfg.api_key, cfg.model)
        self._event.set()
