import argparse
import json
import math
import os
from pathlib import Path
import random
import shutil
import subprocess
import sys
import tempfile

try:
    import numpy as np
    from PIL import Image, ImageDraw, ImageFilter, ImageFont
except ImportError:
    np = None
    Image = ImageDraw = ImageFilter = ImageFont = None

import reel_sound
import reel_voice


ASSETS = Path(__file__).resolve().parent / 'reel_assets'
CANVAS = (3840, 2160)
FPS = 30
ACCENT = (255, 86, 48)
INK = (23, 43, 77)
MUTED = (68, 84, 111)
FONT_FILES = {'regular': 'NotoSans-Regular.ttf', 'bold': 'NotoSans-Bold.ttf'}
MARK_KINDS = ('click', 'type', 'focus')
STYLES = ('circle', 'box', 'underline')
MAX_FRAMES = 60 * 60 * FPS
LEAD_GAP = 0.1
CURSOR_TRAVEL = 0.7
DRAW_START = 0.5
READ_PAUSE = 1.05
CLICK_FADE = 0.3
MARK_SPACING = 0.4


class ReelError(ValueError):
    pass


def need_imaging():
    if np is None or Image is None:
        raise ReelError('reel needs Pillow and numpy (pip install pillow numpy).')


def clamp(value, low=0.0, high=1.0):
    return max(low, min(high, value))


def smooth(value):
    value = clamp(value)
    return value * value * (3 - 2 * value)


def ease_out(value):
    value = clamp(value)
    return 1 - (1 - value) ** 3


def even(value):
    return max(2, int(round(value)) & ~1)


def load_timeline(path, events_path=None):
    data = json.loads(Path(path).read_text()) if path else {}
    if not isinstance(data, dict):
        raise ReelError('The timeline must be a JSON object.')
    source = data.get('source') or {}
    steps = []
    for index, step in enumerate(data.get('steps') or []):
        text = str(step.get('caption') or '').strip()
        start = float(step.get('t', 0))
        end = float(step.get('end', start + 4))
        if not text or end <= start:
            raise ReelError(f'Step {index + 1} needs a caption and end > t.')
        steps.append({'t': start, 'end': end, 'caption': text})
    if not steps and events_path:
        steps = steps_from_events(events_path)
    marks = []
    for index, mark in enumerate(data.get('marks') or []):
        kind = mark.get('kind', 'click')
        rect = mark.get('rect')
        if kind not in MARK_KINDS or not (isinstance(rect, list) and len(rect) == 4 and all(isinstance(item, (int, float)) for item in rect)):
            raise ReelError(f'Mark {index + 1} needs kind {MARK_KINDS} and rect [x, y, width, height].')
        style = mark.get('style') or ('underline' if kind == 'type' else 'circle')
        if style not in STYLES:
            raise ReelError(f'Mark {index + 1} style must be one of {STYLES}.')
        marks.append({'t': float(mark['t']), 'kind': kind, 'rect': [float(item) for item in rect], 'style': style,
                      'label': str(mark.get('label') or '').strip(), 'hold': float(mark.get('hold', 0.0 if kind == 'click' else 1.2))})
    event_source = None
    if events_path:
        event_marks, event_source = marks_from_events(events_path)
        if not marks:
            marks = event_marks
    marks.sort(key=lambda mark: mark['t'])
    return {'title': str(data.get('title') or ''), 'subtitle': str(data.get('subtitle') or ''), 'badge': str(data.get('badge') or 'QA demo'),
            'url': str(data.get('url') or ''), 'tab_title': str(data.get('tab_title') or data.get('title') or ''),
            'source': (int(source['width']), int(source['height'])) if source.get('width') and source.get('height') else event_source,
            'intro': float(data.get('intro_seconds', 2.6)), 'outro': float(data.get('outro_seconds', 1.4)),
            'steps': steps, 'marks': marks}


def marks_from_events(path):
    events = json.loads(Path(path).read_text())
    width, height = int(events.get('viewport_width') or 0), int(events.get('viewport_height') or 0)
    marks = [{'t': float(mark['time_ms']) / 1000, 'kind': 'click', 'style': 'circle', 'label': '', 'hold': 0.0,
              'rect': [float(mark['x']), float(mark['y']), float(mark['width']), float(mark['height'])]}
             for mark in events.get('marks') or []]
    return marks, ((width, height) if width and height else None)


def steps_from_events(path):
    events = json.loads(Path(path).read_text())
    cues = events.get('captions') or []
    duration = float(events.get('duration_ms', 0)) / 1000
    steps = []
    for index, cue in enumerate(cues):
        start = float(cue['time_ms']) / 1000
        end = float(cues[index + 1]['time_ms']) / 1000 if index + 1 < len(cues) else duration
        if end > start:
            steps.append({'t': start, 'end': end, 'caption': str(cue['text'])})
    return steps


def frame_count(seconds):
    return int(math.floor(seconds * FPS + 0.5))


def draw_seconds(style):
    return 0.45 if style == 'underline' else 0.55


def read_seconds(text):
    return clamp(0.9 + len(text) / 17, 2.0, 7.0)


def plan_timeline(marks, steps):
    leads = []
    last_lead = None
    for index, mark in enumerate(marks):
        if last_lead is None or mark['t'] - last_lead >= MARK_SPACING:
            leads.append(index)
            last_lead = mark['t']
    leading = set(leads)
    items = []
    for index, mark in enumerate(marks):
        items.append((max(0.0, mark['t'] - LEAD_GAP) if index in leading else mark['t'], 1, 'mark', index))
    for index, step in enumerate(steps):
        effective = step['t']
        for mark_index in leads:
            freeze = max(0.0, marks[mark_index]['t'] - LEAD_GAP)
            if freeze < step['t'] <= marks[mark_index]['t']:
                effective = freeze
        items.append((effective, 0, 'step', index))
    items.sort()
    holds = {}
    schedule = [None] * len(marks)
    starts = [None] * len(steps)
    shift = 0
    dwell_until = 0.0
    last_frame = 0

    def hold(frame, count):
        nonlocal shift, last_frame
        if count <= 0:
            return
        holds[frame] = holds.get(frame, 0) + count
        shift += count
        last_frame = max(last_frame, frame)

    for effective, _, kind, index in items:
        frame = max(last_frame, frame_count(effective))
        if kind == 'step':
            output = effective + shift / FPS
            if output < dwell_until:
                hold(max(last_frame, frame - 1), math.ceil((dwell_until - output) * FPS))
                output = effective + shift / FPS
            starts[index] = output
            dwell_until = output + read_seconds(steps[index]['caption'])
            continue
        mark = marks[index]
        drawing = draw_seconds(mark['style'])
        if index in leading:
            begin = frame / FPS + shift / FPS
            lead = DRAW_START + drawing + READ_PAUSE
            hold(frame, frame_count(lead - LEAD_GAP))
            click = mark['t'] + shift / FPS
            schedule[index] = {'begin': begin, 'arrive': begin + CURSOR_TRAVEL, 'draw': begin + DRAW_START, 'click': click}
        else:
            click = mark['t'] + shift / FPS
            schedule[index] = {'begin': click - drawing, 'arrive': click, 'draw': click - drawing, 'click': click}
        schedule[index]['fade'] = schedule[index]['click'] + mark['hold']
    ends = [starts[index + 1] if index + 1 < len(steps) else None for index in range(len(steps))]
    return {'holds': holds, 'marks': schedule, 'starts': starts, 'ends': ends, 'extra_frames': shift}


def raw_frame_for_output(holds, output):
    passed = 0
    for frame in sorted(holds):
        if output < frame + passed:
            break
        if output <= frame + passed + holds[frame]:
            return frame
        passed += holds[frame]
    return output - passed


def layout(canvas, source):
    width, height = canvas
    scale = width / CANVAS[0]
    box_w, box_h = 2880 * scale, 1620 * scale
    fit = min(box_w / source[0], box_h / source[1])
    content_w, content_h = even(source[0] * fit), even(source[1] * fit)
    chrome = even(150 * scale)
    window_w, window_h = content_w, content_h + chrome
    window_x = (width - window_w) // 2
    window_y = even(130 * scale)
    return {'scale': scale, 'canvas': canvas, 'content': (window_x, window_y + chrome, content_w, content_h), 'window': (window_x, window_y, window_w, window_h),
            'chrome': chrome, 'radius': even(44 * scale), 'caption_y': height - even(130 * scale), 'source': source, 'fit': fit}


def font_path(weight, font_dir=None):
    directory = Path(font_dir or os.environ.get('REEL_FONT_DIR') or ASSETS)
    path = directory / FONT_FILES[weight]
    if not path.is_file():
        raise ReelError(f'Font {path} is missing; set --font-dir or REEL_FONT_DIR.')
    return str(path)


class Fonts:
    def __init__(self, font_dir=None):
        self.font_dir = font_dir
        self.cache = {}

    def get(self, weight, size):
        key = (weight, int(size))
        if key not in self.cache:
            self.cache[key] = ImageFont.truetype(font_path(weight, self.font_dir), int(size))
        return self.cache[key]

    def width(self, text, weight, size):
        return self.get(weight, size).getlength(text)

    def fit(self, text, weight, size, limit):
        if self.width(text, weight, size) <= limit:
            return text
        while text and self.width(text + '…', weight, size) > limit:
            text = text[:-1]
        return text + '…'


def gradient_background(canvas):
    width, height = canvas
    ys = np.linspace(0, 1, height, dtype=np.float32)[:, None]
    xs = np.linspace(0, 1, width, dtype=np.float32)[None, :]
    top, bottom = np.array([234, 242, 255], np.float32), np.array([243, 234, 251], np.float32)
    image = top[None, None, :] * (1 - ys[:, :, None]) + bottom[None, None, :] * ys[:, :, None]
    image = np.broadcast_to(image, (height, width, 3)).copy()
    aspect = width / height
    blobs = (((0.10, 0.12), 0.55, (176, 208, 255), 0.95), ((0.92, 0.18), 0.50, (214, 200, 250), 0.95), ((0.14, 1.0), 0.55, (255, 205, 230), 0.9),
             ((0.86, 0.95), 0.50, (186, 238, 245), 0.85), ((0.5, 0.5), 0.45, (244, 246, 255), 0.55))
    for (cx, cy), radius, color, strength in blobs:
        dist = np.sqrt(((xs - cx) * aspect) ** 2 + (ys - cy) ** 2)
        weight = np.exp(-((dist / radius) ** 2) * 1.6) * strength
        image += (np.array(color, np.float32)[None, None, :] - image) * weight[:, :, None]
    rng = np.random.default_rng(7)
    image += rng.normal(0, 1.1, (height, width, 1)).astype(np.float32)
    return np.clip(image, 0, 255).astype(np.uint8)


def supersample(size, factor=2):
    return (size[0] * factor, size[1] * factor)


def rounded_mask(size, radius, corners=(True, True, True, True)):
    factor = 3
    big = Image.new('L', supersample(size, factor), 0)
    ImageDraw.Draw(big).rounded_rectangle((0, 0, big.width - 1, big.height - 1), radius=radius * factor, fill=255, corners=corners)
    return big.resize(size, Image.LANCZOS)


def draw_chrome(lay, fonts, tab_title, url):
    width, height = lay['window'][2], lay['window'][3]
    chrome, scale = lay['chrome'], lay['scale']
    factor = 2
    sheet = Image.new('RGBA', (width * factor, chrome * factor), (0, 0, 0, 0))
    draw = ImageDraw.Draw(sheet)
    unit = scale * factor
    draw.rectangle((0, 0, sheet.width, int(74 * unit)), fill=(232, 234, 238, 255))
    draw.rectangle((0, int(74 * unit), sheet.width, sheet.height), fill=(246, 247, 249, 255))
    draw.line((0, sheet.height - 2, sheet.width, sheet.height - 2), fill=(214, 218, 224, 255), width=max(2, int(2 * unit)))
    for index, color in enumerate(((255, 95, 87), (254, 188, 46), (40, 200, 64))):
        cx, cy, radius = (50 + index * 48) * unit, 37 * unit, 14 * unit
        draw.ellipse((cx - radius, cy - radius, cx + radius, cy + radius), fill=color + (255,), outline=tuple(int(c * 0.82) for c in color) + (255,), width=max(1, int(unit)))
    tab_x, tab_w = 250 * unit, 560 * unit
    draw.rounded_rectangle((tab_x, 14 * unit, tab_x + tab_w, 78 * unit), radius=18 * unit, fill=(246, 247, 249, 255), corners=(True, True, False, False))
    fav = (tab_x + 38 * unit, 46 * unit, 13 * unit)
    draw.ellipse((fav[0] - fav[2], fav[1] - fav[2], fav[0] + fav[2], fav[1] + fav[2]), fill=(12, 102, 228, 255))
    draw.polygon([(fav[0] - 5 * unit, fav[1] + 6 * unit), (fav[0], fav[1] - 7 * unit), (fav[0] + 5 * unit, fav[1] + 6 * unit)], fill=(255, 255, 255, 255))
    tab_font = fonts.get('regular', int(25 * unit))
    draw.text((tab_x + 72 * unit, 46 * unit), fonts.fit(tab_title, 'regular', 25 * unit, tab_w - 120 * unit), font=tab_font, fill=INK + (255,), anchor='lm')
    row_y = 116 * unit
    stroke = max(3, int(4 * unit))
    for index, direction in enumerate((-1, 1)):
        cx = (62 + index * 62) * unit
        arm = 10 * unit
        draw.line((cx + direction * -arm * 0.5, row_y - arm, cx + direction * arm * 0.5, row_y), fill=(120, 130, 146, 255), width=stroke, joint='curve')
        draw.line((cx + direction * arm * 0.5, row_y, cx + direction * -arm * 0.5, row_y + arm), fill=(120, 130, 146, 255), width=stroke, joint='curve')
    rx = 200 * unit
    draw.arc((rx - 12 * unit, row_y - 12 * unit, rx + 12 * unit, row_y + 12 * unit), 40, 330, fill=(120, 130, 146, 255), width=stroke)
    pill_x0, pill_x1 = 270 * unit, sheet.width - 150 * unit
    draw.rounded_rectangle((pill_x0, 90 * unit, pill_x1, 142 * unit), radius=26 * unit, fill=(233, 235, 239, 255))
    lock_x, lock_y = pill_x0 + 40 * unit, 116 * unit
    if url.startswith('https://'):
        draw.rounded_rectangle((lock_x - 8 * unit, lock_y - 3 * unit, lock_x + 8 * unit, lock_y + 10 * unit), radius=3 * unit, fill=(100, 110, 128, 255))
        draw.arc((lock_x - 6 * unit, lock_y - 13 * unit, lock_x + 6 * unit, lock_y + 3 * unit), 180, 360, fill=(100, 110, 128, 255), width=max(2, int(2.6 * unit)))
    url_font = fonts.get('regular', int(26 * unit))
    draw.text((pill_x0 + 78 * unit, 116 * unit), fonts.fit(url, 'regular', 26 * unit, pill_x1 - pill_x0 - 120 * unit), font=url_font, fill=(59, 64, 72, 255), anchor='lm')
    share_x = sheet.width - 90 * unit
    draw.rectangle((share_x - 12 * unit, row_y - 4 * unit, share_x + 12 * unit, row_y + 16 * unit), outline=(120, 130, 146, 255), width=stroke)
    draw.line((share_x, row_y + 6 * unit, share_x, row_y - 16 * unit), fill=(120, 130, 146, 255), width=stroke)
    return sheet.resize((width, chrome), Image.LANCZOS)


def build_window(lay, fonts, tab_title, url):
    x, y, width, height = lay['window']
    chrome = lay['chrome']
    layer = Image.new('RGBA', (width, height), (255, 255, 255, 255))
    layer.paste(draw_chrome(lay, fonts, tab_title, url), (0, 0))
    mask = rounded_mask((width, height), lay['radius'])
    layer.putalpha(mask)
    return layer


def shadowed(background, window, lay):
    x, y, width, height = lay['window']
    canvas = Image.fromarray(background)
    scale = lay['scale']
    for blur, alpha, offset in ((90 * scale, 0.30, 48 * scale), (22 * scale, 0.20, 12 * scale)):
        pad = int(blur * 3)
        shadow = Image.new('L', (width + pad * 2, height + pad * 2), 0)
        shadow.paste(rounded_mask((width, height), lay['radius']), (pad, pad))
        shadow = shadow.filter(ImageFilter.GaussianBlur(blur)).point(lambda value, a=alpha: int(value * a))
        tint = Image.new('RGB', shadow.size, (20, 30, 60))
        canvas.paste(tint, (x - pad, y - pad + int(offset)), shadow)
    canvas.paste(window, (x, y), window)
    return np.asarray(canvas).copy()


def text_sprite(text, fonts, weight, size, color, padding, fill, radius, accent=None, badge=None):
    font = fonts.get(weight, size)
    left = padding[0] + (size * 1.8 if badge else 0) + (size * 0.9 if accent else 0)
    text_w = int(font.getlength(text))
    width, height = int(left + text_w + padding[0]), int(size * 1.25 + padding[1] * 2)
    factor = 2
    sheet = Image.new('RGBA', (width * factor, height * factor), (0, 0, 0, 0))
    draw = ImageDraw.Draw(sheet)
    draw.rounded_rectangle((0, 0, sheet.width - 1, sheet.height - 1), radius=radius * factor, fill=fill)
    if badge:
        cx, cy, br = (padding[0] + size * 0.75) * factor, sheet.height / 2, size * 0.62 * factor
        draw.ellipse((cx - br, cy - br, cx + br, cy + br), fill=ACCENT + (255,))
        draw.text((cx, cy), badge, font=fonts.get('bold', int(size * 0.72 * factor)), fill=(255, 255, 255, 255), anchor='mm')
    if accent:
        cx, cy, br = (padding[0] + size * 0.3) * factor, sheet.height / 2, size * 0.2 * factor
        draw.ellipse((cx - br, cy - br, cx + br, cy + br), fill=accent + (255,))
    draw.text((left * factor, sheet.height / 2), text, font=fonts.get(weight, int(size * factor)), fill=color + (255,), anchor='lm')
    return sheet.resize((width, height), Image.LANCZOS)


def to_array(image):
    return np.asarray(image.convert('RGBA')).copy()


def blend(frame, sprite, x, y, opacity=1.0):
    height, width = frame.shape[:2]
    sh, sw = sprite.shape[:2]
    x, y = int(round(x)), int(round(y))
    x0, y0, x1, y1 = max(0, x), max(0, y), min(width, x + sw), min(height, y + sh)
    if x1 <= x0 or y1 <= y0 or opacity <= 0:
        return
    part = sprite[y0 - y:y1 - y, x0 - x:x1 - x]
    alpha = (part[:, :, 3:4].astype(np.float32) / 255.0) * min(1.0, opacity)
    region = frame[y0:y1, x0:x1].astype(np.float32)
    frame[y0:y1, x0:x1] = np.clip(region + (part[:, :, :3].astype(np.float32) - region) * alpha, 0, 255).astype(np.uint8)


def crayon_points(style, rect, scale, rng):
    x, y, width, height = rect
    pad = 22 * scale
    points = []
    if style == 'circle':
        cx, cy = x + width / 2, y + height / 2
        ry = height / 2 + pad
        rx = max(width / 2 + pad + 16 * scale, ry * 1.25)
        count = 150
        start = rng.uniform(-2.7, -1.9)
        sweep = math.tau * 1.12
        phases = [rng.uniform(0, math.tau) for _ in range(3)]
        for index in range(count + 1):
            progress = index / count
            angle = start + sweep * progress
            wobble = 1 + 0.035 * math.sin(2 * angle + phases[0]) + 0.02 * math.sin(3 * angle + phases[1]) + 0.012 * math.sin(5 * angle + phases[2]) + 0.05 * progress
            points.append((cx + rx * wobble * math.cos(angle), cy + ry * wobble * math.sin(angle)))
    elif style == 'box':
        left, top, right, bottom = x - pad, y - pad, x + width + pad, y + height + pad
        corners = [(left, top), (right, top), (right, bottom), (left, bottom), (left - 10 * scale, top + 8 * scale)]
        for a, b in zip(corners, corners[1:]):
            for index in range(34):
                t = index / 33
                bow = math.sin(t * math.pi) * rng.uniform(-5, 5) * scale
                points.append((a[0] + (b[0] - a[0]) * t + bow, a[1] + (b[1] - a[1]) * t - bow))
    else:
        baseline = y + height + 14 * scale
        for stroke in range(2):
            offset = stroke * 12 * scale
            sign = 1 if stroke == 0 else -1
            xs = [x - 8 * scale, x + width + 8 * scale][::sign]
            for index in range(60):
                t = index / 59
                px = xs[0] + (xs[1] - xs[0]) * t
                points.append((px, baseline + offset + math.sin(t * math.pi * 3 + stroke) * 3.5 * scale))
    return points


def crayon_sprite(style, rect, scale, seed):
    rng = random.Random(seed)
    points = crayon_points(style, rect, scale, rng)
    xs, ys = [p[0] for p in points], [p[1] for p in points]
    margin = int(40 * scale)
    left, top = int(min(xs)) - margin, int(min(ys)) - margin
    width, height = int(max(xs)) - left + margin, int(max(ys)) - top + margin
    factor = 2
    mask = Image.new('L', (width * factor, height * factor), 0)
    order = Image.new('L', (width * factor, height * factor), 0)
    draw_mask, draw_order = ImageDraw.Draw(mask), ImageDraw.Draw(order)
    total = len(points) - 1
    phase = rng.uniform(0, math.tau)
    for index in range(total):
        taper = min(1.0, index / 8, (total - index) / 10)
        pressure = (0.78 + 0.22 * math.sin(index * 0.21 + phase)) * (0.55 + 0.45 * taper)
        stroke = max(2, int(12 * scale * factor * pressure))
        a = ((points[index][0] - left) * factor, (points[index][1] - top) * factor)
        b = ((points[index + 1][0] - left) * factor, (points[index + 1][1] - top) * factor)
        value = int(255 * index / max(1, total))
        for jitter in (0.0, 1.0):
            dx, dy = jitter * 2.6 * scale * factor * math.cos(index * 0.4), jitter * 2.6 * scale * factor * math.sin(index * 0.33)
            line = ((a[0] + dx, a[1] + dy), (b[0] + dx, b[1] + dy))
            draw_mask.line(line, fill=255 if jitter == 0 else 150, width=stroke if jitter == 0 else max(2, stroke - 3))
            draw_order.line(line, fill=value, width=stroke)
        radius = stroke / 2
        draw_mask.ellipse((a[0] - radius, a[1] - radius, a[0] + radius, a[1] + radius), fill=255)
        draw_order.ellipse((a[0] - radius, a[1] - radius, a[0] + radius, a[1] + radius), fill=value)
    mask = mask.filter(ImageFilter.GaussianBlur(0.8 * factor / 2)).resize((width, height), Image.LANCZOS)
    order = order.resize((width, height), Image.NEAREST)
    gen = np.random.default_rng(seed)
    grain = gen.normal(0.0, 1.0, (height, width)).astype(np.float32)
    soft = np.asarray(Image.fromarray(np.clip(grain * 40 + 128, 0, 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(1.6)), dtype=np.float32) / 255.0
    texture = np.clip(0.8 + (soft - 0.5) * 2.4, 0.5, 1.0)
    texture *= np.where(gen.random((height, width)) < 0.05, 0.45, 1.0)
    alpha = np.asarray(mask, dtype=np.float32) / 255.0
    return {'origin': (left, top), 'alpha': alpha * texture, 'order': np.asarray(order, dtype=np.float32) / 255.0}


def stroke_frame(sprite, progress, opacity, color=ACCENT):
    reveal = np.clip((progress * 1.04 - sprite['order']) / 0.035, 0, 1)
    alpha = sprite['alpha'] * reveal * opacity
    height, width = alpha.shape
    out = np.empty((height, width, 4), np.uint8)
    out[:, :, 0], out[:, :, 1], out[:, :, 2] = color
    out[:, :, 3] = np.clip(alpha * 255, 0, 255).astype(np.uint8)
    return out


def cursor_sprite(scale):
    size = int(100 * scale)
    factor = 3
    sheet = Image.new('RGBA', (size * 2 * factor, size * 2 * factor), (0, 0, 0, 0))
    draw = ImageDraw.Draw(sheet)
    unit = size * factor / 30
    outline = [(0, 0), (0, 23), (6, 18), (10, 28), (14.5, 26), (10.5, 17), (18, 17)]
    offset = size * factor * 0.25
    shadow = Image.new('RGBA', sheet.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).polygon([(offset + 2.5 * unit + px * unit, offset + 3.5 * unit + py * unit) for px, py in outline], fill=(0, 0, 0, 120))
    shadow = shadow.filter(ImageFilter.GaussianBlur(unit * 1.2))
    sheet = Image.alpha_composite(shadow, sheet)
    draw = ImageDraw.Draw(sheet)
    points = [(offset + px * unit, offset + py * unit) for px, py in outline]
    draw.polygon(points, fill=(255, 255, 255, 255), outline=(15, 20, 30, 255), width=max(2, int(unit * 1.5)))
    return {'array': to_array(sheet.resize((size * 2, size * 2), Image.LANCZOS)), 'hot': (offset / factor, offset / factor)}


def ripple_sprite(radius, ring, alpha):
    size = int(radius * 2 + ring * 4)
    factor = 2
    sheet = Image.new('RGBA', (size * factor, size * factor), (0, 0, 0, 0))
    center = size * factor / 2
    ImageDraw.Draw(sheet).ellipse((center - radius * factor, center - radius * factor, center + radius * factor, center + radius * factor),
                                  outline=ACCENT + (int(255 * alpha),), width=max(2, int(ring * factor)))
    return to_array(sheet.resize((size, size), Image.LANCZOS)), size / 2


class Reel:
    def __init__(self, timeline, source, canvas, fonts, background=None):
        self.timeline = timeline
        self.canvas = canvas
        self.lay = layout(canvas, source)
        self.fonts = fonts
        scale = self.lay['scale']
        self.background = background if background is not None else gradient_background(canvas)
        window = build_window(self.lay, fonts, timeline['tab_title'], timeline['url'])
        self.base = shadowed(self.background, window, self.lay)
        self.corner = self.corner_mask()
        self.cursor = cursor_sprite(scale)
        self.plan = plan_timeline(timeline['marks'], timeline['steps'])
        self.strokes = [crayon_sprite(mark['style'], self.canvas_rect(mark['rect']), scale, 1000 + index) for index, mark in enumerate(timeline['marks'])]
        self.labels = [self.label_sprite(mark) for mark in timeline['marks']]
        self.captions = [self.caption_sprite(step, index) for index, step in enumerate(timeline['steps'])]
        self.title = self.title_card()
        self.waypoints = self.cursor_waypoints()
        self.label_spots = {}

    def corner_mask(self):
        x, y, width, height = self.lay['content']
        radius = self.lay['radius']
        mask = np.asarray(rounded_mask((width, height), radius, (False, False, True, True)), dtype=np.float32) / 255.0
        return mask

    def to_canvas(self, point):
        x, y, width, height = self.lay['content']
        fit = self.lay['fit']
        return (x + point[0] * fit, y + point[1] * fit)

    def canvas_rect(self, rect):
        x, y = self.to_canvas((rect[0], rect[1]))
        fit = self.lay['fit']
        return (x, y, rect[2] * fit, rect[3] * fit)

    def label_sprite(self, mark):
        if not mark['label']:
            return None
        scale = self.lay['scale']
        return to_array(text_sprite(mark['label'], self.fonts, 'bold', 40 * scale, (255, 255, 255), (30 * scale, 17 * scale), INK + (238,), int(30 * scale), accent=ACCENT))

    def caption_sprite(self, step, index):
        scale = self.lay['scale']
        total = len(self.timeline['steps'])
        text = self.fonts.fit(step['caption'], 'bold', 58 * scale, self.lay['window'][2] - 360 * scale)
        badge = f'{index + 1}' if total > 1 else None
        return to_array(text_sprite(text, self.fonts, 'bold', 58 * scale, INK, (44 * scale, 26 * scale), (255, 255, 255, 240), int(54 * scale), badge=badge))

    def title_card(self):
        width, height = self.canvas
        scale = self.lay['scale']
        sheet = Image.new('RGBA', (width, height), (0, 0, 0, 0))
        draw = ImageDraw.Draw(sheet)
        badge = text_sprite(self.timeline['badge'], self.fonts, 'bold', 46 * scale, (255, 255, 255), (34 * scale, 16 * scale), (12, 102, 228, 255), int(40 * scale))
        sheet.paste(badge, ((width - badge.width) // 2, int(height * 0.34)), badge)
        title = self.fonts.fit(self.timeline['title'], 'bold', 170 * scale, width - 400 * scale)
        draw.text((width / 2, height * 0.5), title, font=self.fonts.get('bold', int(170 * scale)), fill=INK + (255,), anchor='mm')
        subtitle = self.fonts.fit(self.timeline['subtitle'], 'regular', 74 * scale, width - 600 * scale)
        draw.text((width / 2, height * 0.5 + 170 * scale), subtitle, font=self.fonts.get('regular', int(74 * scale)), fill=MUTED + (255,), anchor='mm')
        return to_array(sheet)

    def cursor_waypoints(self):
        points = []
        for mark, when in zip(self.timeline['marks'], self.plan['marks']):
            if mark['kind'] == 'click':
                rect = self.canvas_rect(mark['rect'])
                points.append((when['arrive'], when['click'], (rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)))
        return points

    def cursor_position(self, t):
        if not self.waypoints:
            return None
        x, y, width, height = self.lay['content']
        previous = (x + width * 0.62, y + height * 0.82)
        previous_click = self.waypoints[0][0] - 2.0
        for index, (arrive, click, position) in enumerate(self.waypoints):
            begin = min(arrive - 0.25, max(previous_click, arrive - CURSOR_TRAVEL))
            if t < begin:
                return previous, clamp((t - (begin - 0.4)) / 0.4) if index == 0 else 1.0
            if t <= arrive:
                progress = smooth((t - begin) / (arrive - begin))
                dx, dy = position[0] - previous[0], position[1] - previous[1]
                bend = 0.12 * progress * (1 - progress) * 4
                return (previous[0] + dx * progress - dy * bend * 0.5, previous[1] + dy * progress + dx * bend * 0.5), 1.0
            if t <= click:
                return position, 1.0
            previous_click, previous = click, position
        return previous, 1.0

    def draw_overlays(self, frame, t, content):
        scale = self.lay['scale']
        for index, mark in enumerate(self.timeline['marks']):
            when = self.plan['marks'][index]
            draw_time = draw_seconds(mark['style'])
            start = when['draw']
            if t < start or t > when['fade'] + CLICK_FADE:
                continue
            progress = ease_out((t - start) / draw_time)
            opacity = 1.0 - smooth((t - when['fade']) / CLICK_FADE)
            sprite = self.strokes[index]
            blend(frame, stroke_frame(sprite, progress, 1.0), sprite['origin'][0], sprite['origin'][1], opacity)
            label = self.labels[index]
            if label is not None and t > start + 0.18:
                rect = self.canvas_rect(mark['rect'])
                appear = ease_out((t - start - 0.18) / 0.28)
                lx, ly = self.label_position(index, rect, label, content)
                blend(frame, label, lx, ly + (1 - appear) * 14 * scale, opacity * appear)
        if self.waypoints:
            placed = self.cursor_position(t)
            if placed:
                (px, py), alpha = placed
                for _, tc, position in self.waypoints:
                    age = t - tc
                    if 0 <= age < 0.55:
                        radius = (10 + 70 * ease_out(age / 0.55)) * scale
                        ring, ring_origin = ripple_sprite(radius, 7 * scale, 0.85 * (1 - age / 0.55))
                        blend(frame, ring, position[0] - ring_origin, position[1] - ring_origin)
                hot = self.cursor['hot']
                blend(frame, self.cursor['array'], px - hot[0], py - hot[1], alpha)
        for index in range(len(self.timeline['steps'])):
            begin, finish = self.plan['starts'][index], self.plan['ends'][index]
            if begin - 0.001 <= t and (finish is None or t <= finish + 0.3):
                fade_in = ease_out((t - begin) / 0.32)
                fade_out = 1.0 if finish is None else 1.0 - smooth((t - finish) / 0.3)
                sprite = self.captions[index]
                x = (self.canvas[0] - sprite.shape[1]) / 2
                y = self.lay['caption_y'] - sprite.shape[0] / 2 + (1 - fade_in) * 26 * scale
                blend(frame, sprite, x, y, fade_in * fade_out)

    def label_position(self, index, rect, label, content):
        if index in self.label_spots:
            return self.label_spots[index]
        scale = self.lay['scale']
        cx, cy, cw, ch = self.lay['content']
        height, width = label.shape[:2]
        margin = 26 * scale
        near, far = 52 * scale, 130 * scale
        middle = rect[1] + rect[3] / 2 - height / 2
        spread = rect[0] + rect[2] / 2 - width / 2
        candidates = [(rect[0] + rect[2] + near, middle), (rect[0] - near - width, middle), (spread, rect[1] + rect[3] + near * 0.8),
                      (spread, rect[1] - near * 0.8 - height), (rect[0] + rect[2] + far, middle), (rect[0] - far - width, middle),
                      (spread, rect[1] + rect[3] + far), (spread, rect[1] - far - height)]
        best, best_cost = None, None
        for order, (x, y) in enumerate(candidates):
            if x < cx + margin or y < cy + margin or x + width > cx + cw - margin or y + height > cy + ch - margin:
                continue
            region = content[int(y - cy):int(y - cy + height), int(x - cx):int(x - cx + width)]
            ink = float((region.mean(axis=2) < 238).mean()) if region.size else 1.0
            cost = ink + order * 0.01
            if best_cost is None or cost < best_cost:
                best, best_cost = (x, y), cost
        if best is None:
            best = (min(max(cx + margin, spread), cx + cw - width - margin), min(max(cy + margin, rect[1] + rect[3] + near), cy + ch - height - margin))
        self.label_spots[index] = best
        return best

    def compose(self, content, t):
        frame = self.base.copy()
        x, y, width, height = self.lay['content']
        radius = self.lay['radius']
        frame[y:y + height - radius, x:x + width] = content[:height - radius]
        corner_rows = slice(y + height - radius, y + height)
        base_part = self.base[corner_rows, x:x + width].astype(np.float32)
        mask = self.corner[height - radius:, :, None]
        frame[corner_rows, x:x + width] = np.clip(base_part + (content[height - radius:].astype(np.float32) - base_part) * mask, 0, 255).astype(np.uint8)
        self.draw_overlays(frame, t, content)
        return frame

    def intro_frame(self, content, k, intro_frames):
        t = k / FPS
        intro = intro_frames / FPS
        reveal = smooth((t - (intro - 0.8)) / 0.8)
        card = self.background.copy()
        title_alpha = 1.0 - smooth((t - (intro - 1.15)) / 0.4)
        enter = ease_out(t / 0.7)
        shift = (1 - enter) * 40 * self.lay['scale'] - smooth((t - (intro - 1.0)) / 0.9) * 60 * self.lay['scale']
        blend(card, self.title, 0, shift, title_alpha * enter)
        if reveal <= 0:
            return card
        window = self.compose(content, -1.0)
        zoom = 1.07 - 0.07 * reveal
        if zoom > 1.0005:
            image = Image.fromarray(window)
            size = (int(self.canvas[0] * zoom), int(self.canvas[1] * zoom))
            image = image.resize(size, Image.BILINEAR)
            left, top = (size[0] - self.canvas[0]) // 2, (size[1] - self.canvas[1]) // 2
            window = np.asarray(image.crop((left, top, left + self.canvas[0], top + self.canvas[1]))).copy()
        mixed = card.astype(np.float32) + (window.astype(np.float32) - card.astype(np.float32)) * reveal
        return np.clip(mixed, 0, 255).astype(np.uint8)


def run(arguments, **keywords):
    return subprocess.run(arguments, check=True, capture_output=True, text=True, **keywords).stdout


def probe(video, ffprobe):
    data = json.loads(run([ffprobe, '-v', 'error', '-select_streams', 'v:0', '-show_entries', 'stream=width,height,avg_frame_rate:format=duration', '-of', 'json', str(video)]))
    stream = data['streams'][0]
    return int(stream['width']), int(stream['height']), float(data['format']['duration'])


def has_audio(video, ffprobe):
    return bool(run([ffprobe, '-v', 'error', '-select_streams', 'a', '-show_entries', 'stream=index', '-of', 'csv=p=0', str(video)]).strip())


def decoder(video, size, ffmpeg, start=None):
    command = [ffmpeg, '-v', 'error', '-nostdin']
    if start:
        command += ['-ss', f'{start:.3f}']
    command += ['-i', str(video), '-an', '-vf', f'fps={FPS}:round=near,scale={size[0]}:{size[1]}:flags=lanczos+accurate_rnd+full_chroma_int,unsharp=5:5:0.7:3:3:0.0',
                '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-']
    return subprocess.Popen(command, stdout=subprocess.PIPE)


def read_frames(process, size):
    length = size[0] * size[1] * 3
    while True:
        data = process.stdout.read(length)
        if len(data) < length:
            return
        yield np.frombuffer(data, np.uint8).reshape(size[1], size[0], 3)


def encoder(out, canvas, ffmpeg, crf, sound=None, native=None, delay=0.0, preset='medium'):
    command = [ffmpeg, '-v', 'error', '-y', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-s', f'{canvas[0]}x{canvas[1]}', '-r', str(FPS), '-i', '-']
    for source in (sound, native):
        if source:
            command += ['-i', str(source)]
    command += ['-map', '0:v']
    if sound and native:
        command += ['-filter_complex', f'[2:a]adelay={int(delay * 1000)}:all=1[native];[1:a][native]amix=inputs=2:duration=first:normalize=0[mix]', '-map', '[mix]']
    elif sound:
        command += ['-map', '1:a']
    elif native:
        command += ['-map', '1:a', '-af', f'adelay={int(delay * 1000)}:all=1']
    if sound or native:
        command += ['-c:a', 'aac', '-b:a', '160k', '-shortest']
    command += ['-vf', 'scale=out_color_matrix=bt709:out_range=tv,format=yuv420p', '-c:v', 'libx264', '-preset', preset, '-crf', str(crf),
                '-profile:v', 'high', '-r', str(FPS), '-colorspace', 'bt709', '-color_primaries', 'bt709', '-color_trc', 'bt709',
                '-movflags', '+faststart', str(out)]
    return subprocess.Popen(command, stdin=subprocess.PIPE)


def parse_size(text):
    try:
        width, height = (int(part) for part in text.lower().split('x'))
    except ValueError as error:
        raise ReelError('--size must look like 3840x2160.') from error
    if width % 2 or height % 2 or width < 640 or height < 360 or width * 9 != height * 16:
        raise ReelError('--size must be an even 16:9 size of at least 640x360.')
    return width, height


def build(args):
    need_imaging()
    ffmpeg, ffprobe = args.ffmpeg, args.ffprobe
    for tool in (ffmpeg, ffprobe):
        if not shutil.which(tool):
            raise ReelError(f'{tool} is not on PATH.')
    timeline = load_timeline(args.timeline, args.events)
    width, height, duration = probe(args.video, ffprobe)
    source = timeline['source'] or (width, height)
    canvas = parse_size(args.size)
    reel = Reel(timeline, source, canvas, Fonts(args.font_dir))
    return reel, (width, height, duration), canvas


def pick_music(choice):
    if choice == 'off':
        return None
    if choice == 'random':
        return random.SystemRandom().randrange(reel_sound.TRACKS)
    try:
        track = int(choice)
    except ValueError as error:
        raise ReelError('--music must be random, off or a track number.') from error
    if not 0 <= track < reel_sound.TRACKS:
        raise ReelError(f'--music track must be from 0 to {reel_sound.TRACKS - 1}.')
    return track


def click_times(timeline, plan):
    return [timeline['intro'] + when['click'] for mark, when in zip(timeline['marks'], plan['marks']) if mark['kind'] in ('click', 'type')]


def compose(args):
    reel, (width, height, duration), canvas = build(args)
    timeline = reel.timeline
    intro_frames = int(round(timeline['intro'] * FPS))
    outro_frames = int(round(timeline['outro'] * FPS))
    holds = reel.plan['holds']
    if (intro_frames + outro_frames + reel.plan['extra_frames']) / FPS + duration > MAX_FRAMES / FPS:
        raise ReelError('The recording is too long to compose (limit one hour).')
    audio = args.video if not holds and has_audio(args.video, args.ffprobe) else None
    track = pick_music(args.music)
    clicks = click_times(timeline, reel.plan) if track is not None else []
    seconds = (intro_frames + outro_frames + reel.plan['extra_frames'] + int(round(duration * FPS))) / FPS
    with tempfile.TemporaryDirectory() as scratch:
        sound = None
        if track is not None:
            sound = Path(scratch) / 'sound.wav'
            reel_sound.write_wav(sound, reel_sound.render(seconds, clicks, track))
        return encode(args, reel, canvas, timeline, (intro_frames, outro_frames), holds, audio, sound, track)


def encode(args, reel, canvas, timeline, edges, holds, audio, sound, track):
    intro_frames, outro_frames = edges
    content_size = reel.lay['content'][2:]
    source = decoder(args.video, content_size, args.ffmpeg)
    out = encoder(args.out, canvas, args.ffmpeg, args.crf, sound, audio, timeline['intro'], args.preset)
    frames = read_frames(source, content_size)
    first = next(frames, None)
    if first is None:
        raise ReelError('The recording has no video frames.')
    last = first
    count = 0
    try:
        for k in range(intro_frames):
            out.stdin.write(reel.intro_frame(first, k, intro_frames).tobytes())
            count += 1
        shown = 0
        for raw, content in enumerate([first, *frames]):
            for _ in range(1 + holds.get(raw, 0)):
                out.stdin.write(reel.compose(content, shown / FPS).tobytes())
                shown += 1
                count += 1
            last = content
        for k in range(outro_frames):
            out.stdin.write(reel.compose(last, (shown + k) / FPS).tobytes())
            count += 1
    finally:
        out.stdin.close()
        source.stdout.close()
        source.wait()
        if out.wait():
            raise ReelError('ffmpeg could not encode the video.')
    return {'out': str(args.out), 'frames': count, 'seconds': round(count / FPS, 2), 'size': f'{canvas[0]}x{canvas[1]}', 'fps': FPS,
            'intro_seconds': timeline['intro'], 'lead_gap': LEAD_GAP, 'holds': [[frame, count] for frame, count in sorted(holds.items())], 'audio_kept': audio is not None, 'music': track}


def frame(args):
    reel, (width, height, duration), canvas = build(args)
    content_size = reel.lay['content'][2:]
    intro = reel.timeline['intro']
    shown_t = max(0.0, args.time)
    raw_t = raw_frame_for_output(reel.plan['holds'], frame_count(shown_t)) / FPS
    process = decoder(args.video, content_size, args.ffmpeg, raw_t)
    content = next(read_frames(process, content_size), None)
    process.kill()
    if content is None:
        raise ReelError('No video frame at that time.')
    image = reel.intro_frame(content, int(args.card * FPS), int(round(intro * FPS))) if args.card is not None else reel.compose(content, shown_t)
    Image.fromarray(image).save(args.out)
    return {'out': str(args.out), 'time': shown_t, 'source_time': raw_t}


def main():
    parser = argparse.ArgumentParser(description='Compose a polished QA demo video from a clean browser recording.')
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('compose', 'frame'):
        item = commands.add_parser(name)
        item.add_argument('--video', required=True)
        item.add_argument('--timeline')
        item.add_argument('--events')
        item.add_argument('--out', required=True)
        item.add_argument('--size', default='3840x2160')
        item.add_argument('--font-dir')
        item.add_argument('--ffmpeg', default='ffmpeg')
        item.add_argument('--ffprobe', default='ffprobe')
        if name == 'compose':
            item.add_argument('--crf', type=int, default=17)
            item.add_argument('--preset', default='medium')
            item.add_argument('--music', default='random')
        else:
            item.add_argument('--time', type=float, default=1.0)
            item.add_argument('--card', type=float)
    item = commands.add_parser('voiceover')
    item.add_argument('--video', required=True)
    item.add_argument('--cues', required=True)
    item.add_argument('--out', required=True)
    item.add_argument('--engine', default=None)
    item.add_argument('--ffmpeg', default='ffmpeg')
    item.add_argument('--ffprobe', default='ffprobe')
    args = parser.parse_args()
    try:
        print(json.dumps({'compose': compose, 'frame': frame, 'voiceover': reel_voice.voiceover}[args.command](args)))
    except (ReelError, reel_voice.VoiceError, OSError, subprocess.SubprocessError, KeyError, ValueError) as error:
        print(f'reel: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
