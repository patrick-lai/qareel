import math
import wave

import numpy as np

SAMPLE_RATE = 44100
TRACKS = 20
CHORD_SECONDS = 4.0
CHORDS = 8
MUSIC_RMS = 0.04
CLICK_PEAK = 0.22
FADE_IN = 1.5
FADE_OUT = 2.5

SCALES = (
    (0, 2, 4, 7, 9),
    (0, 2, 4, 6, 7, 9, 11),
    (0, 2, 4, 5, 7, 9, 10),
    (0, 2, 3, 5, 7, 9, 10),
    (0, 3, 5, 7, 10),
)
PROGRESSIONS = (
    (0, 3, 4, 3, 0, 3, 5, 4),
    (0, 5, 3, 4, 0, 5, 3, 1),
    (0, 2, 3, 2, 0, 2, 4, 3),
    (0, 4, 5, 3, 0, 4, 1, 3),
    (0, 1, 3, 4, 0, 1, 5, 4),
    (0, 3, 1, 4, 0, 3, 2, 4),
)
BELLS = (
    {'ratios': ((1.0, 1.0), (2.76, 0.28), (5.4, 0.08)), 'decay': 1.6},
    {'ratios': ((1.0, 1.0), (2.0, 0.3), (3.0, 0.1)), 'decay': 0.9},
    {'ratios': ((1.0, 1.0), (2.0, 0.12)), 'decay': 1.3},
)


def frequency(note):
    return 440.0 * 2.0 ** ((note - 69) / 12.0)


def add_circular(buffer, start, wave_form):
    size = len(buffer)
    start %= size
    first = min(len(wave_form), size - start)
    buffer[start:start + first] += wave_form[:first]
    rest = len(wave_form) - first
    while rest > 0:
        part = min(rest, size)
        buffer[:part] += wave_form[first:first + part]
        first += part
        rest -= part


def smooth(x):
    x = np.clip(x, 0.0, 1.0)
    return x * x * (3.0 - 2.0 * x)


def lowpass(signal, cutoff, rate):
    spectrum = np.fft.rfft(signal)
    freqs = np.fft.rfftfreq(len(signal), 1.0 / rate)
    return np.fft.irfft(spectrum / (1.0 + (freqs / cutoff) ** 2), len(signal))


def chord_notes(scale, root, degree, seventh):
    steps = [degree, degree + 2, degree + 4] + ([degree + 6] if seventh else [])
    notes = []
    for step in steps:
        octave, index = divmod(step, len(scale))
        note = root + scale[index] + 12 * octave
        while note > 69:
            note -= 12
        notes.append(note)
    return notes


def pad_voice(note, length, rate, detune):
    t = np.arange(length) / rate
    f = frequency(note) * (1.0 + detune)
    return np.sin(2 * math.pi * f * t) + 0.35 * np.sin(4 * math.pi * f * t) + 0.12 * np.sin(6 * math.pi * f * t)


def bell_voice(note, bell, rate):
    length = int(rate * bell['decay'] * 5)
    t = np.arange(length) / rate
    voice = sum(weight * np.sin(2 * math.pi * frequency(note) * ratio * t) for ratio, weight in bell['ratios'])
    envelope = np.exp(-t / bell['decay']) * smooth(t / 0.006)
    return voice * envelope


def render_loop(index, rate=SAMPLE_RATE):
    rng = np.random.RandomState(7000 + index)
    scale = SCALES[index % len(SCALES)]
    progression = PROGRESSIONS[rng.randint(len(PROGRESSIONS))]
    root = int(rng.choice((48, 50, 51, 53, 55, 57)))
    bell = BELLS[rng.randint(len(BELLS))]
    cutoff = float(rng.uniform(1400, 2600))
    density = float(rng.uniform(0.25, 0.7))
    bass = bool(rng.randint(2))
    seventh = bool(rng.randint(2))
    chord_len = int(CHORD_SECONDS * rate)
    size = chord_len * CHORDS
    low, high, bells = np.zeros(size), np.zeros(size), np.zeros(size)
    attack = smooth(np.arange(int(1.4 * rate)) / (1.4 * rate))
    tail = int(1.8 * rate)
    for chord, degree in enumerate(progression):
        start = chord * chord_len
        length = chord_len + tail
        t = np.arange(length) / rate
        envelope = np.ones(length)
        envelope[:len(attack)] = attack
        envelope[chord_len:] = 1.0 - smooth((t[chord_len:] - CHORD_SECONDS) / 1.8)
        notes = chord_notes(scale, root, degree, seventh)
        for note in notes:
            add_circular(low, start, pad_voice(note, length, rate, -0.0016) * envelope)
            add_circular(high, start, pad_voice(note, length, rate, 0.0016) * envelope)
        if bass:
            add_circular(low, start, pad_voice(notes[0] - 12, length, rate, 0.0) * envelope * 0.9)
            add_circular(high, start, pad_voice(notes[0] - 12, length, rate, 0.0) * envelope * 0.9)
        for beat in range(8):
            if rng.rand() < density:
                step = int(rng.randint(len(scale) * 2))
                octave, position = divmod(step, len(scale))
                note = root + 24 + scale[position] + 12 * (octave - 1)
                add_circular(bells, start + int(beat * 0.5 * rate), bell_voice(note, bell, rate) * 0.5)
    echo_left = int(rng.choice((0.33, 0.375, 0.42)) * rate)
    echo_right = int(rng.choice((0.5, 0.56, 0.62)) * rate)
    wet_left = bells + 0.35 * np.roll(bells, echo_left) + 0.18 * np.roll(bells, 2 * echo_left)
    wet_right = bells + 0.35 * np.roll(bells, echo_right) + 0.18 * np.roll(bells, 2 * echo_right)
    left = lowpass(0.65 * low + 0.35 * high, cutoff, rate) + 0.6 * wet_left
    right = lowpass(0.35 * low + 0.65 * high, cutoff, rate) + 0.6 * wet_right
    stereo = np.stack([left, right], axis=1)
    stereo -= stereo.mean(axis=0)
    return (stereo * (MUSIC_RMS / math.sqrt(float(np.mean(stereo ** 2))))).astype(np.float32)


def click_sound(seed, rate=SAMPLE_RATE):
    rng = np.random.RandomState(seed)
    length = int(0.09 * rate)
    t = np.arange(length) / rate
    pitch = float(rng.uniform(1500, 1900))
    tone = np.sin(2 * math.pi * pitch * t) * np.exp(-t / 0.011)
    thump = np.sin(2 * math.pi * 190 * t) * np.exp(-t / 0.02) * 0.6
    grit = np.diff(rng.uniform(-1, 1, length + 1)) * np.exp(-t / 0.003) * 0.2
    sound = (tone + thump + grit) * smooth(t / 0.0006)
    return (sound / np.max(np.abs(sound)) * CLICK_PEAK).astype(np.float32)


def render(seconds, clicks, track, rate=SAMPLE_RATE):
    total = int(math.ceil(seconds * rate))
    loop = render_loop(track, rate)
    repeats = -(-total // len(loop))
    mix = np.tile(loop, (repeats, 1))[:total].copy()
    t = np.arange(total) / rate
    mix *= (smooth(t / FADE_IN) * smooth((seconds - t) / FADE_OUT))[:, None]
    for number, when in enumerate(clicks):
        start = int(round(when * rate))
        sound = click_sound(100 + number % 5, rate)
        if 0 <= start < total:
            part = sound[:total - start]
            mix[start:start + len(part)] += part[:, None]
    return np.clip(mix, -1.0, 1.0)


def write_wav(path, samples, rate=SAMPLE_RATE):
    with wave.open(str(path), 'wb') as out:
        out.setnchannels(samples.shape[1])
        out.setsampwidth(2)
        out.setframerate(rate)
        out.writeframes((samples * 32767).astype('<i2').tobytes())
