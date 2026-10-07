import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import reel
import reel_sound


def have_imaging():
    return reel.np is not None and reel.Image is not None


def have_ffmpeg():
    if not (shutil.which('ffmpeg') and shutil.which('ffprobe')):
        return False
    return 'libx264' in subprocess.run(['ffmpeg', '-hide_banner', '-encoders'], capture_output=True, text=True).stdout


class SoundPlanTests(unittest.TestCase):
    def test_only_marks_with_a_cursor_click_and_they_land_after_the_title_card(self):
        timeline = {'intro': 2.0, 'marks': [{'kind': 'click'}, {'kind': 'focus'}, {'kind': 'type'}]}
        plan = {'marks': [{'click': 1.0}, {'click': 2.0}, {'click': 3.5}]}
        self.assertEqual(reel.click_times(timeline, plan), [3.0, 5.5])

    def test_music_choice_is_random_off_or_a_valid_track(self):
        self.assertIsNone(reel.pick_music('off'))
        self.assertEqual(reel.pick_music('4'), 4)
        self.assertIn(reel.pick_music('random'), range(reel_sound.TRACKS))
        for bad in ('loud', '-1', str(reel_sound.TRACKS)):
            with self.assertRaises(reel.ReelError, msg=bad):
                reel.pick_music(bad)


class TimelineTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)

    def write(self, name, value):
        path = self.directory / name
        path.write_text(json.dumps(value))
        return path

    def test_marks_are_validated_sorted_and_defaulted(self):
        path = self.write('timeline.json', {'title': 'T', 'steps': [{'t': 1, 'end': 3, 'caption': 'One'}], 'source': {'width': 1280, 'height': 720},
                                            'marks': [{'t': 5, 'kind': 'type', 'rect': [1, 2, 3, 4]}, {'t': 2, 'kind': 'click', 'rect': [10, 20, 30, 40], 'label': ' Go '}]})
        timeline = reel.load_timeline(path)
        self.assertEqual([mark['t'] for mark in timeline['marks']], [2.0, 5.0])
        self.assertEqual([mark['style'] for mark in timeline['marks']], ['circle', 'underline'])
        self.assertEqual(timeline['marks'][0]['label'], 'Go')
        self.assertEqual(timeline['source'], (1280, 720))
        self.assertEqual(timeline['badge'], 'QA demo')

    def test_bad_marks_and_steps_are_refused(self):
        for bad in ({'marks': [{'t': 1, 'kind': 'swipe', 'rect': [1, 2, 3, 4]}]}, {'marks': [{'t': 1, 'kind': 'click', 'rect': [1, 2, 3]}]},
                    {'marks': [{'t': 1, 'kind': 'click', 'rect': [1, 2, 3, 4], 'style': 'star'}]}, {'steps': [{'t': 4, 'end': 2, 'caption': 'x'}]}, {'steps': [{'t': 1, 'end': 2}]}):
            with self.assertRaises(reel.ReelError, msg=bad):
                reel.load_timeline(self.write('bad.json', bad))

    def test_captions_become_steps_when_no_steps_are_given(self):
        events = self.write('events.json', {'duration_ms': 9000, 'captions': [{'time_ms': 1000, 'text': 'First'}, {'time_ms': 4000, 'text': 'Second'}]})
        steps = reel.load_timeline(self.write('timeline.json', {}), events)['steps']
        self.assertEqual([(step['t'], step['end'], step['caption']) for step in steps], [(1.0, 4.0, 'First'), (4.0, 9.0, 'Second')])

    def test_recorder_click_marks_and_viewport_come_from_events(self):
        events = self.directory / 'events.json'
        events.write_text(json.dumps({'duration_ms': 9000, 'captions': [], 'viewport_width': 1280, 'viewport_height': 800,
                                      'marks': [{'time_ms': 4200, 'x': 10, 'y': 20, 'width': 30, 'height': 40}]}))
        timeline = reel.load_timeline(None, str(events))
        self.assertEqual(timeline['source'], (1280, 800))
        self.assertEqual([(m['t'], m['kind'], m['rect']) for m in timeline['marks']], [(4.2, 'click', [10.0, 20.0, 30.0, 40.0])])

    def test_a_click_is_announced_before_it_happens_and_the_annotation_clears_on_the_click(self):
        marks = [{'t': 5.0, 'kind': 'click', 'style': 'circle', 'label': '', 'hold': 0.0, 'rect': [0, 0, 1, 1]}]
        plan = reel.plan_timeline(marks, [])
        self.assertEqual(plan['holds'], {int(4.9 * reel.FPS): 60})
        when = plan['marks'][0]
        self.assertAlmostEqual(when['click'] - when['begin'], 2.1, delta=0.04)
        self.assertLess(when['begin'], when['arrive'])
        self.assertLess(when['arrive'], when['click'])
        self.assertLessEqual(when['draw'] + reel.draw_seconds('circle') + 1.0, when['click'])
        self.assertAlmostEqual(when['click'], 5.0 + 2.0, delta=0.04)
        self.assertEqual(when['fade'], when['click'])

    def test_rapid_repeat_marks_do_not_stack_pauses(self):
        marks = [{'t': 5.0 + step * 0.15, 'kind': 'click', 'style': 'circle', 'label': '', 'hold': 0.0, 'rect': [0, 0, 1, 1]} for step in range(3)]
        plan = reel.plan_timeline(marks, [])
        self.assertEqual(sum(plan['holds'].values()), 60)

    def test_captions_stay_until_the_next_one_and_the_last_runs_to_the_end(self):
        steps = [{'t': 1.0, 'end': 2.0, 'caption': 'First step caption'}, {'t': 9.0, 'end': 10.0, 'caption': 'Second'}]
        plan = reel.plan_timeline([], steps)
        self.assertEqual(plan['starts'], [1.0, 9.0])
        self.assertEqual(plan['ends'], [9.0, None])
        self.assertEqual(plan['holds'], {})

    def test_a_caption_replaced_too_soon_is_held_long_enough_to_read(self):
        text = 'Open the personal agents page and search for an agent'
        steps = [{'t': 1.0, 'end': 1.5, 'caption': text}, {'t': 1.5, 'end': 4.0, 'caption': 'Next'}]
        plan = reel.plan_timeline([], steps)
        self.assertGreaterEqual(plan['starts'][1] - plan['starts'][0], reel.read_seconds(text) - 1 / reel.FPS)
        self.assertEqual(sum(plan['holds'].values()), plan['extra_frames'])

    def test_a_caption_sent_just_before_a_click_shows_while_the_annotation_plays(self):
        marks = [{'t': 5.0, 'kind': 'click', 'style': 'circle', 'label': '', 'hold': 0.0, 'rect': [0, 0, 1, 1]}]
        plan = reel.plan_timeline(marks, [{'t': 4.95, 'end': 6.0, 'caption': 'Click save'}])
        self.assertLess(plan['starts'][0], plan['marks'][0]['draw'])

    def test_output_frames_map_back_to_the_frozen_source_frame(self):
        holds = {10: 5}
        self.assertEqual([reel.raw_frame_for_output(holds, frame) for frame in (0, 9, 10, 12, 15, 16, 20)], [0, 9, 10, 10, 10, 11, 15])

    def test_size_must_be_even_sixteen_by_nine(self):
        self.assertEqual(reel.parse_size('3840x2160'), (3840, 2160))
        self.assertEqual(reel.parse_size('1280x720'), (1280, 720))
        for bad in ('3840x2000', '639x359', '1281x721', 'wide'):
            with self.assertRaises(reel.ReelError):
                reel.parse_size(bad)

    def test_layout_fits_the_window_and_leaves_room_for_captions(self):
        for source in ((1280, 720), (1100, 760), (1920, 1200)):
            lay = reel.layout((3840, 2160), source)
            x, y, width, height = lay['content']
            window = lay['window']
            self.assertEqual((width % 2, height % 2), (0, 0))
            self.assertEqual(window[3], height + lay['chrome'])
            self.assertLessEqual(window[0] + window[2], 3840)
            self.assertLess(window[1] + window[3], lay['caption_y'] - 40)
            self.assertAlmostEqual(width / height, source[0] / source[1], delta=0.02)
        small = reel.layout((1280, 720), (1280, 720))
        self.assertAlmostEqual(small['scale'], 1 / 3)

    def test_easing_is_bounded_and_monotonic(self):
        values = [reel.smooth(step / 10) for step in range(-2, 13)]
        self.assertEqual((values[0], values[-1]), (0.0, 1.0))
        self.assertEqual(values, sorted(values))
        self.assertEqual(reel.ease_out(2), 1.0)


@unittest.skipUnless(have_imaging(), 'Pillow and numpy are required')
class RenderTests(unittest.TestCase):
    def test_crayon_is_deterministic_and_draws_on_progressively(self):
        first = reel.crayon_sprite('circle', (400, 300, 200, 80), 1.0, 11)
        second = reel.crayon_sprite('circle', (400, 300, 200, 80), 1.0, 11)
        self.assertTrue((first['alpha'] == second['alpha']).all())
        covered = [float(reel.stroke_frame(first, progress, 1.0)[:, :, 3].astype(float).sum()) for progress in (0.0, 0.3, 0.6, 1.0)]
        self.assertEqual(covered[0], 0.0)
        self.assertEqual(covered, sorted(covered))
        self.assertGreater(covered[3], covered[1] * 1.5)
        other = reel.crayon_sprite('circle', (400, 300, 200, 80), 1.0, 12)
        self.assertFalse((first['alpha'].shape == other['alpha'].shape) and (first['alpha'] == other['alpha']).all())
        for style in reel.STYLES:
            sprite = reel.crayon_sprite(style, (100, 100, 120, 40), 1.0, 3)
            self.assertGreater(sprite['alpha'].max(), 0.5)

    def test_background_is_deterministic_smooth_and_light(self):
        first = reel.gradient_background((640, 360))
        self.assertTrue((first == reel.gradient_background((640, 360))).all())
        self.assertEqual(first.shape, (360, 640, 3))
        self.assertGreater(float(first.mean()), 200)

    def test_blend_clips_to_the_frame(self):
        frame = reel.np.zeros((20, 20, 3), reel.np.uint8)
        sprite = reel.np.full((10, 10, 4), 255, reel.np.uint8)
        reel.blend(frame, sprite, 15, 15, 1.0)
        reel.blend(frame, sprite, -5, -5, 0.5)
        reel.blend(frame, sprite, 100, 100, 1.0)
        self.assertEqual(int(frame[19, 19, 0]), 255)
        self.assertEqual(int(frame[0, 0, 0]), 127)
        self.assertEqual(int(frame[10, 10, 0]), 0)


@unittest.skipUnless(have_imaging() and have_ffmpeg(), 'Pillow, numpy and ffmpeg with libx264 are required')
class ComposeTests(unittest.TestCase):
    def test_a_short_recording_becomes_a_constant_thirty_fps_sixteen_by_nine_video(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'raw.mp4'
            subprocess.run(['ffmpeg', '-v', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=320x180:rate=24:duration=2', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', str(source)], check=True)
            timeline = root / 'timeline.json'
            timeline.write_text(json.dumps({'title': 'Check', 'subtitle': 'Compose', 'intro_seconds': 0.5, 'outro_seconds': 0.3, 'source': {'width': 320, 'height': 180},
                                            'steps': [{'t': 0.2, 'end': 1.5, 'caption': 'A caption'}],
                                            'marks': [{'t': 0.4, 'kind': 'click', 'rect': [100, 60, 60, 30], 'label': 'Click'}]}))
            out = root / 'out.mp4'
            args = argparse.Namespace(video=str(source), timeline=str(timeline), events=None, out=str(out), size='640x360', font_dir=None, ffmpeg='ffmpeg', ffprobe='ffprobe', crf=28, preset='ultrafast', music='2')
            result = reel.compose(args)
            probe = json.loads(subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_entries', 'stream=width,height,r_frame_rate,codec_name:format=duration', '-of', 'json', str(out)],
                                              capture_output=True, text=True, check=True).stdout)
            stream = probe['streams'][0]
            self.assertEqual((stream['width'], stream['height'], stream['r_frame_rate'], stream['codec_name']), (640, 360, '30/1', 'h264'))
            self.assertAlmostEqual(float(probe['format']['duration']), 0.5 + 2 + 2 + 0.3, delta=0.15)
            self.assertEqual(result['frames'], 15 + 60 + 60 + 9)
            self.assertEqual(result['music'], 2)
            audio = json.loads(subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'a', '-show_entries', 'stream=codec_name,duration', '-of', 'json', str(out)], capture_output=True, text=True, check=True).stdout)['streams']
            self.assertEqual([stream['codec_name'] for stream in audio], ['aac'])
            self.assertAlmostEqual(float(audio[0]['duration']), float(probe['format']['duration']), delta=0.15)
            self.assertEqual(result['holds'], [[9, 60]])
            png = root / 'frame.png'
            reel.frame(argparse.Namespace(video=str(source), timeline=str(timeline), events=None, out=str(png), size='640x360', font_dir=None, ffmpeg='ffmpeg', ffprobe='ffprobe', time=1.2, card=None))
            with reel.Image.open(png) as image:
                self.assertEqual(image.size, (640, 360))


if __name__ == '__main__':
    unittest.main()
