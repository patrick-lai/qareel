import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import wave

import numpy as np

RATE = 44100
LINE_GAP = 0.35
END_MARGIN = 0.6
TAIL = 0.5
TRIM_LEVEL = 0.012
TRIM_KEEP = 0.05
FADE = 0.012
TARGET_RMS = 0.11
PEAK_LIMIT = 0.9
MAX_LINES = 80
MAX_SECONDS = 600.0
SAY_RATE = 168
SAY_PREFERRED = ('Ava (Premium)', 'Zoe (Premium)', 'Evan (Premium)', 'Ava (Enhanced)', 'Zoe (Enhanced)', 'Evan (Enhanced)', 'Allison (Enhanced)', 'Samantha (Enhanced)', 'Samantha')
ENGINES = ('auto', 'command', 'piper', 'say', 'espeak')
SPEECH_TIMEOUT = 120


class VoiceError(ValueError):
    pass


def run(arguments, **keywords):
    return subprocess.run(arguments, check=True, capture_output=True, text=True, **keywords).stdout


def speakable(text):
    text = re.sub(r'https?://\S+', 'the link', text)
    text = re.sub(r'`([^`]*)`', r'\1', text)
    text = text.replace('&', ' and ').replace('→', ' to ').replace('->', ' to ').replace('…', '...')
    text = re.sub(r'[*_#>|]+', ' ', text)
    text = re.sub(r'\s+', ' ', text).strip()
    return text


def voices_dir(env):
    home = env.get('QAREEL_HOME') or str(Path(env.get('HOME', '~')).expanduser() / '.qareel')
    return Path(home) / 'voices'


def piper_model(env, voice=None):
    candidates = []
    if voice:
        named = Path(voice).expanduser()
        candidates += [named, voices_dir(env) / voice, voices_dir(env) / f'{voice}.onnx']
    if env.get('QAREEL_PIPER_MODEL'):
        candidates.append(Path(env['QAREEL_PIPER_MODEL']).expanduser())
    folder = voices_dir(env)
    if folder.is_dir():
        candidates += sorted(folder.glob('*.onnx'))
    return next((path for path in candidates if path.is_file() and path.suffix == '.onnx'), None)


def pick_say_voice(listing, wanted=None):
    names = []
    for line in listing.splitlines():
        match = re.match(r'^(.+?)\s{2,}([A-Za-z]{2,3}[_-][A-Za-z0-9]+)\s+#', line)
        if match:
            names.append(match.group(1).strip())
    if wanted:
        for name in names:
            if name.lower() == wanted.lower():
                return name
        for name in names:
            if name.lower().startswith(wanted.lower()):
                return name
        raise VoiceError(f'The voice "{wanted}" is not installed. Install it in System Settings > Accessibility > Spoken Content > System Voice > Manage Voices, or pick another.')
    for name in SAY_PREFERRED:
        if name in names:
            return name
    return None


class Engine:
    def __init__(self, name, label, speak):
        self.name = name
        self.label = label
        self.speak = speak


def command_engine(template, voice):
    parts = shlex.split(template)
    if not parts or not any('{out}' in part for part in parts):
        raise VoiceError('QAREEL_TTS_COMMAND must contain {out}, the file the command writes its audio to, and read the text from standard input.')

    def speak(text, out):
        arguments = [part.replace('{out}', str(out)).replace('{voice}', voice or '') for part in parts]
        try:
            subprocess.run(arguments, input=text, text=True, check=True, capture_output=True, timeout=SPEECH_TIMEOUT)
        except subprocess.CalledProcessError as error:
            raise VoiceError(f'The voice command failed: {(error.stderr or "").strip()[-300:]}') from error
        except subprocess.TimeoutExpired as error:
            raise VoiceError('The voice command did not finish in time.') from error
    return Engine('command', voice or 'custom command', speak)


def piper_engine(executable, model):
    def speak(text, out):
        try:
            subprocess.run([executable, '--model', str(model), '--output_file', str(out)], input=text, text=True, check=True, capture_output=True, timeout=SPEECH_TIMEOUT)
        except subprocess.CalledProcessError as error:
            raise VoiceError(f'Piper failed: {(error.stderr or "").strip()[-300:]}') from error
        except subprocess.TimeoutExpired as error:
            raise VoiceError('Piper did not finish in time.') from error
    return Engine('piper', model.stem, speak)


def say_engine(executable, voice):
    def speak(text, out):
        arguments = [executable, '-r', str(SAY_RATE), '-o', str(out)]
        if voice:
            arguments += ['-v', voice]
        try:
            subprocess.run(arguments + ['--', text], check=True, capture_output=True, text=True, timeout=SPEECH_TIMEOUT)
        except subprocess.CalledProcessError as error:
            raise VoiceError(f'say failed: {(error.stderr or "").strip()[-300:]}') from error
        except subprocess.TimeoutExpired as error:
            raise VoiceError('say did not finish in time.') from error
    return Engine('say', voice or 'system voice', speak)


def espeak_engine(executable, voice):
    def speak(text, out):
        try:
            subprocess.run([executable, '-v', voice or 'en-us', '-s', '150', '-p', '45', '-w', str(out), '--', text], check=True, capture_output=True, text=True, timeout=SPEECH_TIMEOUT)
        except subprocess.CalledProcessError as error:
            raise VoiceError(f'espeak failed: {(error.stderr or "").strip()[-300:]}') from error
        except subprocess.TimeoutExpired as error:
            raise VoiceError('espeak did not finish in time.') from error
    return Engine('espeak', voice or 'en-us', speak)


def choose_engine(requested='auto', voice=None, env=None, which=shutil.which, listing=None):
    env = os.environ if env is None else env
    requested = (requested or 'auto').lower()
    if requested not in ENGINES:
        raise VoiceError(f'The voice engine must be one of: {", ".join(ENGINES)}.')
    order = ['command', 'piper', 'say', 'espeak'] if requested == 'auto' else [requested]
    for name in order:
        if name == 'command' and env.get('QAREEL_TTS_COMMAND'):
            return command_engine(env['QAREEL_TTS_COMMAND'], voice)
        if name == 'piper':
            executable, model = which('piper'), piper_model(env, voice)
            if executable and model:
                return piper_engine(executable, model)
        if name == 'say' and which('say'):
            executable = which('say')
            available = listing if listing is not None else run([executable, '-v', '?'])
            return say_engine(executable, pick_say_voice(available, voice or env.get('QAREEL_VOICE')))
        if name == 'espeak' and (which('espeak-ng') or which('espeak')):
            return espeak_engine(which('espeak-ng') or which('espeak'), voice)
    raise VoiceError(no_engine_hint(requested))


def no_engine_hint(requested='auto'):
    if requested not in ('auto', None):
        return f'The {requested} voice engine is not available on this machine. Run `qareel doctor` for what is installed.'
    if shutil.which('say'):
        return 'No voice is available.'
    return ('No voice-over engine was found. For a natural voice install Piper (pip install piper-tts), download a voice such as en_US-lessac-medium '
            '(the .onnx and .onnx.json files from https://huggingface.co/rhasspy/piper-voices) into ~/.qareel/voices, then run `qareel demo narrate`. '
            'To use any other speech tool set QAREEL_TTS_COMMAND, for example "mytts --out {out}", which reads the text on standard input.')


def convert(ffmpeg, source, target):
    run([ffmpeg, '-v', 'error', '-y', '-i', str(source), '-ac', '1', '-ar', str(RATE), '-c:a', 'pcm_s16le', str(target)])


def read_wav(path):
    with wave.open(str(path), 'rb') as handle:
        if handle.getsampwidth() != 2 or handle.getnchannels() != 1:
            raise VoiceError('The converted speech is not 16-bit mono.')
        raw = handle.readframes(handle.getnframes())
    return np.frombuffer(raw, dtype='<i2').astype(np.float32) / 32768.0


def write_wav(path, samples):
    with wave.open(str(path), 'wb') as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(RATE)
        handle.writeframes((np.clip(samples, -1.0, 1.0) * 32767).astype('<i2').tobytes())


def trim(samples):
    loud = np.flatnonzero(np.abs(samples) > TRIM_LEVEL)
    if len(loud) == 0:
        return samples[:0]
    keep = int(TRIM_KEEP * RATE)
    return samples[max(0, loud[0] - keep):loud[-1] + keep + 1]


def normalize(samples):
    if len(samples) == 0:
        return samples
    size = int(0.02 * RATE)
    frames = samples[:len(samples) // size * size].reshape(-1, size) if len(samples) >= size else samples.reshape(1, -1)
    levels = np.sqrt(np.mean(frames ** 2, axis=1))
    active = levels[levels > TRIM_LEVEL / 2]
    rms = float(np.sqrt(np.mean(active ** 2))) if len(active) else 0.0
    peak = float(np.abs(samples).max())
    if rms <= 0.0 or peak <= 0.0:
        return samples
    return samples * min(TARGET_RMS / rms, PEAK_LIMIT / peak)


def fade(samples):
    size = min(int(FADE * RATE), len(samples) // 2)
    if size == 0:
        return samples
    ramp = np.linspace(0.0, 1.0, size, dtype=np.float32)
    shaped = samples.copy()
    shaped[:size] *= ramp
    shaped[-size:] *= ramp[::-1]
    return shaped


def prepare(samples):
    return fade(normalize(trim(samples)))


def schedule(cues, durations, total):
    placed = []
    cursor = 0.0
    ordered = sorted(range(len(cues)), key=lambda index: (bool(cues[index].get('end')), cues[index].get('at') or 0.0))
    for index in ordered:
        cue, length = cues[index], durations[index]
        if cue.get('end'):
            wanted = total - END_MARGIN - length
        else:
            wanted = float(cue['at'])
        start = max(wanted, cursor, 0.0)
        end = start + length
        placed.append({'id': cue['id'], 'start': round(start, 3), 'end': round(end, 3), 'late': round(max(0.0, start - wanted), 3), 'index': index})
        cursor = end + LINE_GAP
    last = max((item['end'] for item in placed), default=0.0)
    return placed, max(0.0, last + TAIL - total)


def lay_out(placed, clips, seconds):
    track = np.zeros(int(np.ceil(seconds * RATE)) + 1, dtype=np.float32)
    for item in placed:
        clip = clips[item['index']]
        start = int(round(item['start'] * RATE))
        part = clip[:max(0, len(track) - start)]
        track[start:start + len(part)] += part
    return track


def probe(video, ffprobe):
    data = json.loads(run([ffprobe, '-v', 'error', '-show_entries', 'stream=codec_type,avg_frame_rate:format=duration', '-of', 'json', str(video)]))
    streams = data.get('streams', [])
    return float(data['format']['duration']), any(stream.get('codec_type') == 'audio' for stream in streams)


def mix_command(ffmpeg, video, voice, out, seconds, music, extend):
    chain = 'highpass=f=80,acompressor=threshold=0.125:ratio=2.5:attack=8:release=120:makeup=1.4,alimiter=limit=0.95'
    command = [ffmpeg, '-v', 'error', '-y', '-i', str(video), '-i', str(voice)]
    if music:
        graph = f'[1:a]{chain},asplit=2[speech][key];[0:a][key]sidechaincompress=threshold=0.02:ratio=6:attack=40:release=700:makeup=1[bed];[bed][speech]amix=inputs=2:duration=longest:normalize=0[mixed]'
    else:
        graph = f'[1:a]{chain}[mixed]'
    command += ['-filter_complex', graph, '-map', '0:v', '-map', '[mixed]']
    if extend > 0:
        command += ['-vf', f'tpad=stop_mode=clone:stop_duration={extend:.3f},format=yuv420p', '-c:v', 'libx264', '-preset', 'medium', '-crf', '17', '-profile:v', 'high',
                    '-colorspace', 'bt709', '-color_primaries', 'bt709', '-color_trc', 'bt709']
    else:
        command += ['-c:v', 'copy']
    command += ['-c:a', 'aac', '-b:a', '192k', '-t', f'{seconds:.3f}', '-movflags', '+faststart', str(out)]
    return command


def check_cues(cues):
    if not cues or len(cues) > MAX_LINES:
        raise VoiceError(f'Provide 1 to {MAX_LINES} voice-over lines.')
    for cue in cues:
        if not isinstance(cue.get('id'), str) or not isinstance(cue.get('text'), str) or not speakable(cue['text']):
            raise VoiceError('Every voice-over line needs an id and some text.')
        if not cue.get('end') and not isinstance(cue.get('at'), (int, float)):
            raise VoiceError(f'The voice-over line {cue["id"]} has no time.')


def voiceover(args):
    for tool in (args.ffmpeg, args.ffprobe):
        if not shutil.which(tool):
            raise VoiceError(f'{tool} is not on PATH.')
    plan = json.loads(Path(args.cues).read_text())
    cues = plan['cues']
    check_cues(cues)
    engine = choose_engine(args.engine or plan.get('engine') or 'auto', plan.get('voice'))
    duration, music = probe(args.video, args.ffprobe)
    clips = []
    with tempfile.TemporaryDirectory() as scratch:
        for number, cue in enumerate(cues):
            raw = Path(scratch) / f'line{number}.raw'
            wav = Path(scratch) / f'line{number}.wav'
            engine.speak(speakable(cue['text']), raw)
            if not raw.is_file() or raw.stat().st_size == 0:
                raise VoiceError(f'The {engine.name} voice produced no audio for the line {cue["id"]}.')
            try:
                convert(args.ffmpeg, raw, wav)
            except subprocess.CalledProcessError as error:
                raise VoiceError(f'The {engine.name} voice produced audio ffmpeg cannot read.') from error
            clip = prepare(read_wav(wav))
            if len(clip) == 0:
                raise VoiceError(f'The {engine.name} voice produced only silence for the line {cue["id"]}.')
            clips.append(clip)
        durations = [len(clip) / RATE for clip in clips]
        placed, extend = schedule(cues, durations, duration)
        if sum(durations) > MAX_SECONDS:
            raise VoiceError('The voice-over is too long (limit ten minutes of speech).')
        seconds = duration + extend
        track = Path(scratch) / 'voice.wav'
        write_wav(track, lay_out(placed, clips, seconds))
        partial = Path(args.out).with_suffix('.voice.mp4')
        try:
            subprocess.run(mix_command(args.ffmpeg, args.video, track, partial, seconds, music, extend), check=True, capture_output=True, text=True)
        except subprocess.CalledProcessError as error:
            partial.unlink(missing_ok=True)
            raise VoiceError(f'ffmpeg could not mix the voice-over: {(error.stderr or "").strip()[-300:]}') from error
        os.replace(partial, args.out)
    return {'out': str(args.out), 'engine': engine.name, 'voice': engine.label, 'lines': len(cues), 'voice_seconds': round(sum(durations), 2),
            'extended_seconds': round(extend, 2), 'placed': [{key: item[key] for key in ('id', 'start', 'end', 'late')} for item in placed]}
