"""Config contract tests: defaults, clamps, round-trip, error paths."""

import json
import os
import tempfile
import unittest

from streamaid.config import Config, load, save


class ConfigTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.dir.name, "config.json")

    def tearDown(self):
        self.dir.cleanup()

    def test_defaults_written_when_missing(self):
        cfg = load(self.path)
        self.assertEqual(cfg.host, "0.0.0.0")
        self.assertEqual(cfg.port, 8080)
        self.assertEqual(cfg.token, "")
        self.assertEqual(cfg.capture.driver, "ffmpeg")
        self.assertEqual(cfg.capture.input, "")
        self.assertEqual(cfg.capture.fps, 30)
        self.assertEqual(cfg.capture.scale, 1.0)
        self.assertEqual(cfg.capture.jpeg_quality, 90)
        self.assertEqual(cfg.encoder.mode, "mjpeg")
        self.assertEqual(cfg.encoder.bitrate_kbps, 20000)
        self.assertEqual(cfg.llm.enabled, False)
        self.assertEqual(cfg.llm.base_url, "http://127.0.0.1:11434/v1")
        self.assertEqual(cfg.llm.api_key, "ollama")
        self.assertEqual(cfg.llm.model, "llama3.2-vision")
        self.assertEqual(cfg.llm.interval_sec, 5)
        self.assertEqual(cfg.llm.prompt, "")
        self.assertTrue(os.path.exists(self.path))

    def test_clamp_low_bounds(self):
        cfg = Config.from_dict({
            "capture": {"fps": 0, "scale": 0.1, "jpeg_quality": 10},
            "encoder": {"bitrate_kbps": 100},
            "llm": {"interval_sec": 1},
        })
        self.assertEqual(cfg.capture.fps, 1)
        self.assertEqual(cfg.capture.scale, 0.25)
        self.assertEqual(cfg.capture.jpeg_quality, 30)
        self.assertEqual(cfg.encoder.bitrate_kbps, 500)
        self.assertEqual(cfg.llm.interval_sec, 2)

    def test_clamp_high_bounds(self):
        cfg = Config.from_dict({
            "capture": {"fps": 999, "scale": 3.0, "jpeg_quality": 100},
            "encoder": {"bitrate_kbps": 99999},
            "llm": {"interval_sec": 99999},
        })
        self.assertEqual(cfg.capture.fps, 60)
        self.assertEqual(cfg.capture.scale, 1.0)
        self.assertEqual(cfg.capture.jpeg_quality, 95)
        self.assertEqual(cfg.encoder.bitrate_kbps, 50000)
        self.assertEqual(cfg.llm.interval_sec, 3600)

    def test_encoder_mode_invalid_resets_to_mjpeg(self):
        cfg = Config.from_dict({"encoder": {"mode": "webp"}})
        self.assertEqual(cfg.encoder.mode, "mjpeg")
        cfg = Config.from_dict({"encoder": {"mode": "hevc"}})
        self.assertEqual(cfg.encoder.mode, "hevc")
        cfg = Config.from_dict({"encoder": {"mode": "ffmpeg"}})
        self.assertEqual(cfg.encoder.mode, "ffmpeg")

    def test_to_from_roundtrip(self):
        cfg = Config()
        cfg.host = "127.0.0.1"
        cfg.port = 9000
        cfg.token = "abc"
        cfg.capture.input = "4:"
        cfg.capture.fps = 15
        cfg.capture.scale = 0.5
        cfg.capture.jpeg_quality = 60
        cfg.encoder.mode = "ffmpeg"
        cfg.encoder.bitrate_kbps = 4000
        cfg.llm.enabled = True
        cfg.llm.model = "moondream"
        cfg.llm.interval_sec = 10
        cfg.llm.prompt = "custom"
        cfg2 = Config.from_dict(cfg.to_dict())
        self.assertEqual(cfg.to_dict(), cfg2.to_dict())

    def test_save_roundtrip(self):
        cfg = Config.from_dict({"capture": {"fps": 20}, "token": "t"})
        save(cfg, self.path)
        cfg2 = load(self.path)
        self.assertEqual(cfg2.capture.fps, 20)
        self.assertEqual(cfg2.token, "t")

    def test_invalid_json_raises_value_error(self):
        with open(self.path, "w") as f:
            f.write("{not json")
        with self.assertRaises(ValueError):
            load(self.path)

    def test_non_object_json_raises(self):
        with open(self.path, "w") as f:
            json.dump([1, 2], f)
        with self.assertRaises(ValueError):
            load(self.path)

    def test_unknown_keys_ignored(self):
        cfg = Config.from_dict({
            "bogus": 1,
            "capture": {"nope": 2, "fps": 24},
            "encoder": {"weird": True, "mode": "ffmpeg"},
            "llm": {"x": "y", "model": "m"},
        })
        self.assertEqual(cfg.capture.fps, 24)
        self.assertEqual(cfg.encoder.mode, "ffmpeg")
        self.assertEqual(cfg.llm.model, "m")
        self.assertEqual(cfg.port, 8080)
        self.assertEqual(cfg.token, "")


if __name__ == "__main__":
    unittest.main()
