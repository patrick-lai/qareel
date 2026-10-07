import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import numpy as np

import reel_sound

RATE = 8000


class MusicTests(unittest.TestCase):
    def test_every_track_is_a_distinct_gentle_seamless_half_minute_loop(self):
        loops = [reel_sound.render_loop(index, RATE) for index in range(reel_sound.TRACKS)]
        for loop in loops:
            self.assertAlmostEqual(len(loop) / RATE, 32.0, delta=3.0)
            self.assertLess(float(np.abs(loop).max()), 0.3)
            self.assertLess(float(np.sqrt((loop ** 2).mean())), 0.06)
            self.assertLessEqual(float(np.abs(loop[0] - loop[-1]).max()), 3 * float(np.abs(np.diff(loop, axis=0)).max()))
        self.assertEqual(len({loop.tobytes() for loop in loops}), reel_sound.TRACKS)
        self.assertTrue(np.array_equal(loops[5], reel_sound.render_loop(5, RATE)))

    def test_clicks_sit_on_the_music_at_their_time_and_the_bed_fades_in_and_out(self):
        quiet = reel_sound.render(10.0, [], 1, RATE)
        clicked = reel_sound.render(10.0, [4.0], 1, RATE)
        changed = np.flatnonzero(np.abs(clicked - quiet).max(axis=1) > 0)
        self.assertAlmostEqual(changed[0] / RATE, 4.0, delta=0.01)
        self.assertLess(changed[-1] / RATE, 4.2)
        self.assertGreater(float(np.abs(clicked - quiet).max()), 0.1)
        self.assertLess(float(np.abs(quiet[:RATE // 10]).max()), float(np.abs(quiet[5 * RATE:6 * RATE]).max()) / 4)
        self.assertLess(float(np.abs(quiet[-RATE // 10:]).max()), float(np.abs(quiet[5 * RATE:6 * RATE]).max()) / 4)


if __name__ == '__main__':
    unittest.main()
