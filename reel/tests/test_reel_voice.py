import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import numpy as np

import reel_voice

RATE = reel_voice.RATE

STUB = textwrap.dedent('''
    import math, struct, sys, wave
    words = len(sys.stdin.read().split())
    rate = 22050
    lead = [0] * int(0.3 * rate)
    body = [int(9000 * math.sin(2 * math.pi * 220 * n / rate)) for n in range(int(words * 0.2 * rate))]
    with wave.open(sys.argv[1], 'wb') as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes(struct.pack('<%dh' % (len(lead) * 2 + len(body)), *(lead + body + lead)))
''')


def have_ffmpeg():
    if not (shutil.which('ffmpeg') and shutil.which('ffprobe')):
        return False
    return 'libx264' in subprocess.run(['ffmpeg', '-hide_banner', '-encoders'], capture_output=True, text=True).stdout


def tone(seconds, level=0.3, frequency=300.0):
    t = np.arange(int(seconds * RATE)) / RATE
    return (level * np.sin(2 * np.pi * frequency * t)).astype(np.float32)


class TextTests(unittest.TestCase):
    def test_speech_text_drops_markup_links_and_symbols(self):
        spoken = reel_voice.speakable('Open `Settings` -> **Profile** at https://example.com/a?b=1 & save.')
        self.assertEqual(spoken, 'Open Settings to Profile at the link and save.')


class EngineTests(unittest.TestCase):
    LISTING = 'Alex                en_US    # Most people recognize me\nAva (Enhanced)      en_US    # Hello\nSamantha            en_US    # Hello\nZoe (Premium)       en_US    # Hello\n'

    def test_the_best_installed_system_voice_is_preferred_and_a_missing_one_is_explained(self):
        self.assertEqual(reel_voice.pick_say_voice(self.LISTING), 'Zoe (Premium)')
        self.assertEqual(reel_voice.pick_say_voice(self.LISTING, 'ava'), 'Ava (Enhanced)')
        self.assertIsNone(reel_voice.pick_say_voice('Alex   en_US    # x\n'))
        with self.assertRaises(reel_voice.VoiceError):
            reel_voice.pick_say_voice(self.LISTING, 'Nobody')

    def test_engines_are_chosen_in_order_and_an_empty_machine_gets_a_fix(self):
        with tempfile.TemporaryDirectory() as home:
            env = {'QAREEL_HOME': home}
            models = Path(home) / 'voices'
            models.mkdir()
            (models / 'en_US-lessac-medium.onnx').write_bytes(b'x')
            tools = {'piper': '/bin/piper', 'say': '/bin/say', 'espeak-ng': '/bin/espeak-ng'}
            self.assertEqual(reel_voice.choose_engine('auto', env=env, which=tools.get, listing=self.LISTING).name, 'piper')
            self.assertEqual(reel_voice.choose_engine('auto', env={'QAREEL_HOME': home + '/none'}, which=tools.get, listing=self.LISTING).name, 'say')
            self.assertEqual(reel_voice.choose_engine('auto', env={'QAREEL_HOME': home + '/none'}, which={'espeak-ng': '/bin/e'}.get).name, 'espeak')
            custom = {'QAREEL_HOME': home, 'QAREEL_TTS_COMMAND': 'speak --out {out}'}
            self.assertEqual(reel_voice.choose_engine('auto', env=custom, which=tools.get).name, 'command')
            with self.assertRaises(reel_voice.VoiceError) as raised:
                reel_voice.choose_engine('auto', env={'QAREEL_HOME': home + '/none'}, which={}.get)
            self.assertIn('QAREEL_TTS_COMMAND', str(raised.exception))
            with self.assertRaises(reel_voice.VoiceError):
                reel_voice.choose_engine('festival', env=env, which={}.get)

    def test_a_custom_command_must_say_where_it_writes(self):
        with self.assertRaises(reel_voice.VoiceError):
            reel_voice.command_engine('speak --loud', None)


class CleanupTests(unittest.TestCase):
    def test_every_line_is_trimmed_and_brought_to_the_same_level_without_clipping(self):
        quiet = np.concatenate([np.zeros(RATE // 2, np.float32), tone(1.0, 0.02), np.zeros(RATE // 2, np.float32)])
        loud = np.concatenate([tone(1.0, 0.9), np.zeros(RATE, np.float32)])
        a, b = reel_voice.prepare(quiet), reel_voice.prepare(loud)
        self.assertLess(len(a) / RATE, 1.3)
        self.assertLess(len(b) / RATE, 1.2)
        for clip in (a, b):
            self.assertLessEqual(float(np.abs(clip).max()), reel_voice.PEAK_LIMIT + 1e-6)
            self.assertAlmostEqual(float(np.sqrt(np.mean(clip[RATE // 10:-RATE // 10] ** 2))), reel_voice.TARGET_RMS, delta=0.02)
            self.assertEqual(float(clip[0]), 0.0)
            self.assertEqual(float(clip[-1]), 0.0)
        self.assertEqual(len(reel_voice.prepare(np.zeros(RATE, np.float32))), 0)


class ScheduleTests(unittest.TestCase):
    def test_a_line_waits_for_the_one_before_it_and_the_outro_ends_before_the_video_does(self):
        cues = [{'id': 'a', 'at': 1.0}, {'id': 'b', 'at': 2.0}, {'id': 'c', 'end': True}]
        placed, extend = reel_voice.schedule(cues, [3.0, 2.0, 2.0], 20.0)
        by_id = {item['id']: item for item in placed}
        self.assertEqual((by_id['a']['start'], by_id['a']['end']), (1.0, 4.0))
        self.assertAlmostEqual(by_id['b']['start'], 4.0 + reel_voice.LINE_GAP)
        self.assertAlmostEqual(by_id['b']['late'], 2.0 + reel_voice.LINE_GAP)
        self.assertAlmostEqual(by_id['c']['end'], 20.0 - reel_voice.END_MARGIN)
        self.assertEqual(extend, 0.0)

    def test_speech_that_runs_past_the_video_asks_for_a_longer_video(self):
        placed, extend = reel_voice.schedule([{'id': 'a', 'at': 9.0}], [4.0], 10.0)
        self.assertAlmostEqual(extend, 3.0 + reel_voice.TAIL)
        self.assertEqual(placed[0]['start'], 9.0)

    def test_lines_are_placed_in_time_order_whatever_order_they_arrive_in(self):
        placed, _ = reel_voice.schedule([{'id': 'late', 'at': 8.0}, {'id': 'early', 'at': 1.0}], [1.0, 1.0], 20.0)
        self.assertEqual([item['id'] for item in placed], ['early', 'late'])


@unittest.skipUnless(have_ffmpeg(), 'ffmpeg with libx264 is not installed')
class MixTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.dir = Path(self.scratch.name)
        (self.dir / 'stub.py').write_text(STUB)
        self.env = mock.patch.dict(os.environ, {'QAREEL_TTS_COMMAND': f'{sys.executable} {self.dir / "stub.py"} {{out}}', 'QAREEL_HOME': str(self.dir / 'home')})
        self.env.start()

    def tearDown(self):
        self.env.stop()
        self.scratch.cleanup()

    def video(self, seconds=8, music=True):
        path = self.dir / 'in.mp4'
        command = ['ffmpeg', '-v', 'error', '-y', '-f', 'lavfi', '-i', f'testsrc=size=320x180:rate=30:duration={seconds}']
        if music:
            command += ['-f', 'lavfi', '-i', f'sine=frequency=440:duration={seconds}', '-af', 'volume=0.3', '-c:a', 'aac']
        subprocess.run(command + ['-c:v', 'libx264', '-pix_fmt', 'yuv420p', str(path)], check=True)
        return path

    def run_voiceover(self, cues, video):
        cue_file = self.dir / 'cues.json'
        cue_file.write_text(json.dumps({'cues': cues}))
        out = self.dir / 'out.mp4'
        result = reel_voice.voiceover(argparse.Namespace(video=str(video), cues=str(cue_file), out=str(out), engine=None, ffmpeg='ffmpeg', ffprobe='ffprobe'))
        return out, result

    def samples(self, path):
        raw = subprocess.run(['ffmpeg', '-v', 'error', '-i', str(path), '-vn', '-ac', '1', '-ar', str(RATE), '-f', 's16le', '-'], capture_output=True, check=True).stdout
        return np.frombuffer(raw, dtype='<i2').astype(np.float32) / 32768.0

    def level(self, samples, frequency, start, end):
        window = samples[int(start * RATE):int(end * RATE)]
        spectrum = np.abs(np.fft.rfft(window * np.hanning(len(window)))) / len(window)
        freqs = np.fft.rfftfreq(len(window), 1.0 / RATE)
        return float(spectrum[(freqs > frequency - 15) & (freqs < frequency + 15)].max())

    def streams(self, path):
        data = json.loads(subprocess.run(['ffprobe', '-v', 'error', '-show_entries', 'stream=codec_name,codec_type:format=duration', '-of', 'json', str(path)], capture_output=True, text=True, check=True).stdout)
        return {stream['codec_type']: stream['codec_name'] for stream in data['streams']}, float(data['format']['duration'])

    def test_the_voice_is_added_the_music_dips_under_it_and_the_picture_is_not_re_encoded(self):
        source = self.video()
        out, result = self.run_voiceover([{'id': 'shot-1', 'text': 'Now we save the new display name and look at the header', 'at': 2.0}], source)
        codecs, duration = self.streams(out)
        self.assertEqual(codecs, {'video': 'h264', 'audio': 'aac'})
        self.assertAlmostEqual(duration, 8.0, delta=0.15)
        self.assertEqual(result['engine'], 'command')
        self.assertEqual(result['extended_seconds'], 0.0)
        audio = self.samples(out)
        start = result['placed'][0]['start']
        end = result['placed'][0]['end']
        self.assertAlmostEqual(start, 2.0)
        speech = self.level(audio, 220, start + 0.2, end - 0.2)
        silence = self.level(audio, 220, 6.0, 7.5)
        self.assertGreater(speech, silence * 20)
        music_under_voice = self.level(audio, 440, start + 0.5, end - 0.2)
        music_alone = self.level(audio, 440, 6.0, 7.5)
        self.assertLess(music_under_voice, music_alone * 0.7)
        self.assertGreater(music_under_voice, 0.0)

    def test_a_video_with_no_music_still_gets_its_voice(self):
        out, _ = self.run_voiceover([{'id': 'a', 'text': 'Hello there', 'at': 1.0}], self.video(music=False))
        codecs, _ = self.streams(out)
        self.assertEqual(codecs['audio'], 'aac')
        self.assertGreater(self.level(self.samples(out), 220, 1.2, 1.5), 0.001)

    def test_a_line_that_outlasts_the_video_extends_it_on_the_last_frame(self):
        text = ' '.join(['word'] * 20)
        out, result = self.run_voiceover([{'id': 'a', 'text': text, 'at': 6.5}], self.video())
        codecs, duration = self.streams(out)
        self.assertGreater(result['extended_seconds'], 2.0)
        self.assertAlmostEqual(duration, 8.0 + result['extended_seconds'], delta=0.2)
        self.assertEqual(codecs['video'], 'h264')

    def test_the_outro_line_finishes_before_the_end_card_does(self):
        _, result = self.run_voiceover([{'id': 'a', 'text': 'First line here', 'at': 1.0}, {'id': 'outro', 'text': 'That is the whole change', 'end': True}], self.video())
        outro = result['placed'][-1]
        self.assertEqual(outro['id'], 'outro')
        self.assertAlmostEqual(outro['end'], 8.0 - reel_voice.END_MARGIN, delta=0.02)

    def test_a_voice_that_writes_nothing_fails_with_the_line_named(self):
        (self.dir / 'stub.py').write_text('import sys\nsys.stdin.read()\n')
        with self.assertRaises(reel_voice.VoiceError) as raised:
            self.run_voiceover([{'id': 'shot-3', 'text': 'Anything', 'at': 1.0}], self.video())
        self.assertIn('shot-3', str(raised.exception))
        self.assertFalse((self.dir / 'out.mp4').exists())
        self.assertEqual([path.name for path in self.dir.glob('*.voice.mp4')], [])


if __name__ == '__main__':
    unittest.main()
