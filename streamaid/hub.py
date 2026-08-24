"""FrameHub: latest frame + subscribers + fps."""

from __future__ import annotations

import collections
import queue
import threading
import time

JPEG_MIME = "image/jpeg"
INIT_MIME = "video/mp4-init"
FRAGMENT_MIME = "video/mp4-fragment"


class FrameHub:
    """Holds the latest encoded frame and fans frames out to subscribers.

    Frames are full encoded units: one JPEG, or one MP4 fragment. The first
    MP4 frame (the init segment) is stored separately via ``init_segment()``.
    Subscriber queues use a drop-oldest policy so a slow client never blocks
    the capture writer.
    """

    def __init__(self):
        self._cond = threading.Condition()
        self._latest = None  # (mime, data, w, h, ts) | None
        self._init = None    # bytes | None
        self._subs = {}      # id -> queue.Queue
        self._next_id = 0
        self._gen = 0
        self._ts = collections.deque()  # publish timestamps (last 2 s)

    def publish(self, mime: str, data: bytes, w: int, h: int) -> None:
        with self._cond:
            if mime == INIT_MIME:
                self._init = data
            else:
                now = time.monotonic()
                self._latest = (mime, data, w, h, now)
                self._ts.append(now)
                while self._ts and now - self._ts[0] > 2.0:
                    self._ts.popleft()
            dead = []
            for sid, q in self._subs.items():
                try:
                    q.put_nowait((mime, data, w, h))
                except queue.Full:
                    # drop-oldest so a slow client never blocks capture
                    try:
                        q.get_nowait()
                    except queue.Empty:
                        pass
                    try:
                        q.put_nowait((mime, data, w, h))
                    except queue.Full:
                        dead.append(sid)
            for sid in dead:
                del self._subs[sid]
            self._cond.notify_all()

    def subscribe(self, maxsize: int = 2):
        with self._cond:
            q = queue.Queue(maxsize=maxsize)
            sid = self._next_id
            self._next_id += 1
            self._subs[sid] = q
            return sid, q

    def unsubscribe(self, sid: int) -> None:
        with self._cond:
            self._subs.pop(sid, None)

    def latest(self):
        with self._cond:
            return self._latest

    def init_segment(self):
        with self._cond:
            return self._init

    def fps(self) -> float:
        with self._cond:
            now = time.monotonic()
            while self._ts and now - self._ts[0] > 2.0:
                self._ts.popleft()
            return len(self._ts) / 2.0

    def clients(self) -> int:
        with self._cond:
            return len(self._subs)

    def clear(self) -> None:
        with self._cond:
            self._latest = None
            self._init = None
            self._ts.clear()
            self._gen += 1
            for q in self._subs.values():
                while True:
                    try:
                        q.get_nowait()
                    except queue.Empty:
                        break
            self._cond.notify_all()

    def generation(self) -> int:
        with self._cond:
            return self._gen
