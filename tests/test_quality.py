"""Quality metric contract tests: sharp vs blurred discrimination."""

import unittest

from streamaid.quality import EDGE_THRESHOLD, _H, _W, frame_metrics


def _sharp_plane():
    """Text-like pattern: fine checkerboard grid, high-frequency content."""
    y = bytearray(_W * _H)
    for j in range(_H):
        for i in range(_W):
            # 4px stripes plus fine 1px checkerboard in a region
            v = 40 if (j // 4) % 2 == 0 else 220
            if 200 < i < 500 and 100 < j < 300:
                v = 255 if (i + j) % 2 == 0 else 0
            y[j * _W + i] = v
    return bytes(y)


def _flat_plane():
    return bytes([128]) * (_W * _H)


class QualityMetricsTests(unittest.TestCase):
    def test_sharp_plane_scores_high(self):
        m = frame_metrics(_sharp_plane())
        self.assertGreater(m["laplacian_var"], 100.0)
        self.assertGreater(m["edge_density"], 0.05)
        self.assertGreater(m["contrast"], 50.0)

    def test_flat_plane_scores_zero(self):
        m = frame_metrics(_flat_plane())
        self.assertAlmostEqual(m["laplacian_var"], 0.0, places=6)
        self.assertEqual(m["edge_density"], 0.0)
        self.assertAlmostEqual(m["contrast"], 0.0, places=6)

    def test_sharp_dominates_blurred(self):
        """A fine pattern must yield far higher sharpness than a coarse one."""
        fine = frame_metrics(_sharp_plane())
        coarse = bytearray(_W * _H)
        for j in range(_H):
            for i in range(_W):
                coarse[j * _W + i] = 40 if (j // 64) % 2 == 0 else 220
        coarse_m = frame_metrics(bytes(coarse))
        self.assertGreater(fine["laplacian_var"], coarse_m["laplacian_var"] * 8)
        self.assertGreater(fine["edge_density"], coarse_m["edge_density"] * 4)

    def test_threshold_sane(self):
        self.assertGreater(EDGE_THRESHOLD, 0)
        self.assertLess(EDGE_THRESHOLD, 128)


if __name__ == "__main__":
    unittest.main()
