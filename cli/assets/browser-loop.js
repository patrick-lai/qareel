((request) => {
  const nonce = request.input || null;
  delete request.input;
  const input = __INPUT__;
  const [offsetX, offsetY] = Array.isArray(request.offset) ? request.offset : [0, 0];
  const nativeTouch = request.touch === 'native';
  const handlers = window.webkit && window.webkit.messageHandlers;
  const bridge = nonce && handlers && handlers.commissionInput ? handlers.commissionInput : null;
  const root = (window.__commissionLoops ||= { loops: {} });
  const safe = (value, depth = 0) => {
    if (value === undefined) return null;
    if (value === null || typeof value === 'number' || typeof value === 'boolean') return Number.isFinite(value) || typeof value !== 'number' ? value : null;
    if (typeof value === 'string') return value.length > 500 ? value.slice(0, 500) + '...' : value;
    if (typeof value === 'function') return 'fn';
    if (depth > 4) return '...';
    if (Array.isArray(value)) return value.slice(0, 50).map((item) => safe(item, depth + 1));
    if (typeof value === 'object') {
      const out = {};
      for (const key of Object.keys(value).slice(0, 50)) out[key] = safe(value[key], depth + 1);
      return out;
    }
    return String(value);
  };
  const codeOf = (name) => {
    if (name === ' ' || name === 'Spacebar') return 'Space';
    if (/^[a-zA-Z]$/.test(name)) return 'Key' + name.toUpperCase();
    if (/^[0-9]$/.test(name)) return 'Digit' + name;
    if (['Shift', 'Control', 'Alt', 'Meta'].includes(name)) return name + 'Left';
    return ({ Up: 'ArrowUp', Down: 'ArrowDown', Left: 'ArrowLeft', Right: 'ArrowRight', Esc: 'Escape', Return: 'Enter' })[name] || name;
  };
  const nativeName = (code) => {
    if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
    if (/^Digit\d$/.test(code)) return code.slice(5);
    const side = /^(Shift|Control|Alt|Meta)(Left|Right)$/.exec(code);
    return side ? side[1] : code;
  };
  const keyInit = (code) => {
    const named = { Space: ' ', Enter: 'Enter', Escape: 'Escape', Tab: 'Tab', Backspace: 'Backspace', Delete: 'Delete', ShiftLeft: 'Shift', ShiftRight: 'Shift', ControlLeft: 'Control', ControlRight: 'Control', AltLeft: 'Alt', AltRight: 'Alt', MetaLeft: 'Meta', MetaRight: 'Meta' };
    let key = named[code];
    if (key === undefined) key = /^Key[A-Z]$/.test(code) ? code.slice(3).toLowerCase() : /^Digit\d$/.test(code) ? code.slice(5) : code;
    return { code, key, bubbles: true, cancelable: true, composed: true, view: window };
  };
  const status = (loop) => ({
    name: loop.name, running: loop.running, trusted: loop.trusted, reason: loop.reason, frames: loop.frames, fps: loop.fps, error: loop.error,
    elapsed_ms: Math.round(performance.now() - loop.started), held: [...loop.held], value: safe(loop.value), state: safe(loop.state),
    logs: loop.logs.slice(-(request.tail || 20)),
  });
  const stop = (loop, reason) => {
    if (!loop.running) return;
    loop.running = false;
    loop.reason = reason;
    for (const code of [...loop.held]) loop.api.key(code, 'up');
    for (const index of loop.pads) input.unpad(index);
    if (loop.pointerDown && loop.lastPointer) loop.api.pointer('up', loop.lastPointer[0], loop.lastPointer[1]);
    if (loop.raf) cancelAnimationFrame(loop.raf);
    if (loop.timer) clearInterval(loop.timer);
  };
  if (request.action === 'list') return Object.values(root.loops).map((loop) => ({ name: loop.name, running: loop.running, frames: loop.frames, reason: loop.reason }));
  if (request.action === 'read') {
    const loop = root.loops[request.name];
    if (!loop) throw new Error('browser_loop.unknown: no loop named ' + request.name + '; start one first');
    return status(loop);
  }
  if (request.action === 'stop') {
    const loop = root.loops[request.name];
    if (!loop) return { name: request.name, running: false, reason: 'none' };
    stop(loop, 'stopped');
    return status(loop);
  }
  if (root.loops[request.name]) stop(root.loops[request.name], 'replaced');
  const loop = { pads: new Set(), trusted: !!bridge, name: request.name, running: true, reason: null, frames: 0, fps: 0, error: null, started: performance.now(), state: {}, logs: [], held: new Set(), value: null, raf: 0, timer: 0 };
  const target = () => document.activeElement && document.activeElement !== document.body ? document.activeElement : document.body;
  const api = {
    state: loop.state,
    trusted: !!bridge,
    key(name, mode = 'tap') {
      const code = codeOf(name);
      if (bridge) {
        bridge.postMessage({ nonce, kind: 'key', phase: mode === 'down' || mode === 'up' ? mode : 'tap', key: nativeName(code) });
        if (mode === 'down') loop.held.add(code); else if (mode === 'up') loop.held.delete(code);
        return;
      }
      api.fire(code, mode);
    },
    fire(name, mode = 'tap') {
      const code = codeOf(name);
      const fire = (type) => target().dispatchEvent(new KeyboardEvent(type, { ...keyInit(code), repeat: type === 'keydown' && loop.held.has(code) }));
      if (mode === 'down') { fire('keydown'); loop.held.add(code); }
      else if (mode === 'up') { loop.held.delete(code); fire('keyup'); }
      else { fire('keydown'); fire('keyup'); }
    },
    keys(names) {
      const codes = names.map(codeOf);
      for (const code of [...loop.held]) if (!codes.includes(code)) api.key(code, 'up');
      for (const code of codes) if (!loop.held.has(code)) api.key(code, 'down');
    },
    wheel(dx, dy, x = innerWidth / 2, y = innerHeight / 2, zoom = false) {
      if (bridge) { bridge.postMessage({ nonce, kind: 'wheel', phase: 'move', x: +x + offsetX, y: +y + offsetY, dx: +dx, dy: +dy, zoom: !!zoom }); return; }
      (document.elementFromPoint(x, y) || document.body).dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, composed: true, view: window, clientX: x, clientY: y, deltaX: dx, deltaY: dy, deltaMode: 0, ctrlKey: !!zoom }));
    },
    look(dx, dy) { return input.look(dx, dy); },
    locked() { return input.locked(); },
    pad(state, index = 0) { loop.pads.add(index); return input.pad(state, index); },
    unpad(index = 0) { return input.unpad(index); },
    touch(id, phase, x, y) {
      if (bridge && nativeTouch) { bridge.postMessage({ nonce, kind: 'touch', phase, id, x: +x + offsetX, y: +y + offsetY }); return; }
      input.touch(id, phase, x, y);
    },
    pinch(x, y, scale, spread = 60) {
      const from = spread / 2, to = (spread * scale) / 2;
      api.touch(0, 'down', x - from, y); api.touch(1, 'down', x + from, y);
      input.gesture('gesturestart', x, y, 1);
      for (let step = 1; step <= 6; step++) {
        const reach = from + ((to - from) * step) / 6;
        api.touch(0, 'move', x - reach, y); api.touch(1, 'move', x + reach, y);
        input.gesture('gesturechange', x, y, 1 + ((scale - 1) * step) / 6);
      }
      api.touch(0, 'up', x - to, y); api.touch(1, 'up', x + to, y);
      input.gesture('gestureend', x, y, scale);
      api.wheel(0, -Math.log(scale) * 100, x, y, true);
    },
    pointer(type, x, y) {
      loop.lastPointer = [x, y];
      if (bridge) {
        bridge.postMessage({ nonce, kind: 'pointer', phase: type, x: +x + offsetX, y: +y + offsetY });
        if (type === 'down') loop.pointerDown = true; else if (type === 'up') loop.pointerDown = false;
        return;
      }
      if (type === 'tap') { api.pointer('down', x, y); api.pointer('up', x, y); (document.elementFromPoint(x, y) || document.body).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, composed: true, clientX: x, clientY: y, view: window })); return; }
      if (type === 'down') loop.pointerDown = true; else if (type === 'up') loop.pointerDown = false;
      const buttons = type === 'up' ? 0 : loop.pointerDown ? 1 : 0;
      const node = document.elementFromPoint(x, y) || document.body;
      const init = { bubbles: true, cancelable: true, composed: true, clientX: x, clientY: y, view: window, button: 0, buttons };
      node.dispatchEvent(new PointerEvent('pointer' + type, { ...init, pointerId: 1, pointerType: 'mouse', isPrimary: true }));
      node.dispatchEvent(new MouseEvent('mouse' + type, init));
    },
    pixels(x, y, w = 1, h = 1) {
      const inside = (canvas) => { const r = canvas.getBoundingClientRect(); return x >= r.left && x < r.right && y >= r.top && y < r.bottom; };
      const hit = document.elementFromPoint(x, y);
      const canvas = hit && hit.tagName === 'CANVAS' ? hit : [...document.querySelectorAll('canvas')].reverse().find(inside);
      if (!canvas) return null;
      const r = canvas.getBoundingClientRect(), sx = canvas.width / r.width, sy = canvas.height / r.height;
      const width = Math.max(1, Math.min(256, Math.round(w))), height = Math.max(1, Math.min(256, Math.round(h)));
      const scratch = (loop.scratch ||= document.createElement('canvas'));
      scratch.width = width; scratch.height = height;
      const context = scratch.getContext('2d', { willReadFrequently: true });
      try {
        context.clearRect(0, 0, width, height);
        context.drawImage(canvas, (x - r.left) * sx, (y - r.top) * sy, w * sx, h * sy, 0, 0, width, height);
        return context.getImageData(0, 0, width, height).data;
      } catch (error) { return null; }
    },
    sample(x, y) {
      const data = api.pixels(x, y, 1, 1);
      return data ? [data[0], data[1], data[2], data[3]] : null;
    },
    log(value) {
      loop.logs.push([Math.round(performance.now() - loop.started), safe(value)]);
      if (loop.logs.length > 200) loop.logs.shift();
    },
    stop(reason) { stop(loop, reason || 'stopped'); },
    now: () => performance.now(),
  };
  loop.api = api;
  const run = (0, eval)('(' + request.code + ')');
  if (typeof run !== 'function') throw new Error('browser_loop.invalid: code must be a function such as (api, tick) => { ... }');
  let last = performance.now();
  let windowStart = last;
  let windowFrames = 0;
  const tick = (time) => {
    if (!loop.running) return;
    const now = performance.now();
    loop.frames++;
    windowFrames++;
    if (now - windowStart >= 1000) { loop.fps = Math.round((windowFrames * 1000) / (now - windowStart)); windowStart = now; windowFrames = 0; }
    try {
      loop.value = run(api, { time, frame: loop.frames, dt: now - last, elapsed: now - loop.started });
    } catch (error) {
      loop.error = String((error && error.message) || error);
      stop(loop, 'error');
      return;
    }
    last = now;
    if (now - loop.started >= request.max_ms) { stop(loop, 'time'); return; }
    if (request.every === 'frame') loop.raf = requestAnimationFrame(tick);
  };
  root.loops[loop.name] = loop;
  if (request.every === 'frame') loop.raf = requestAnimationFrame(tick);
  else loop.timer = setInterval(() => tick(performance.now()), request.every);
  return status(loop);
})(__REQUEST__)
