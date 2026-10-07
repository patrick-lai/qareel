(() => {
  const key = Symbol.for('commission.input');
  if (window[key]) return window[key];
  const documentProto = Document.prototype;
  const elementProto = Element.prototype;
  const nativeLockElement = Object.getOwnPropertyDescriptor(documentProto, 'pointerLockElement');
  const nativeRequest = elementProto.requestPointerLock;
  const nativeExit = documentProto.exitPointerLock;
  let locked = null;
  const lockChanged = () => document.dispatchEvent(new Event('pointerlockchange', { bubbles: true }));
  Object.defineProperty(documentProto, 'pointerLockElement', {
    configurable: true,
    get() { return locked || (nativeLockElement && nativeLockElement.get ? nativeLockElement.get.call(this) : null); },
  });
  elementProto.requestPointerLock = function requestPointerLock(options) {
    if (document.hasFocus() && nativeRequest) {
      try {
        const result = nativeRequest.call(this, options);
        if (result && typeof result.then === 'function') return result.catch(() => { locked = this; lockChanged(); });
        return result;
      } catch (error) {}
    }
    locked = this;
    queueMicrotask(lockChanged);
    return Promise.resolve();
  };
  documentProto.exitPointerLock = function exitPointerLock() {
    if (locked) { locked = null; queueMicrotask(lockChanged); return; }
    if (nativeExit) return nativeExit.call(this);
  };

  const buttonIndex = { a: 0, cross: 0, b: 1, circle: 1, x: 2, square: 2, y: 3, triangle: 3, lb: 4, l1: 4, rb: 5, r1: 5, lt: 6, l2: 6, rt: 7, r2: 7, select: 8, back: 8, view: 8, start: 9, menu: 9, options: 9, ls: 10, l3: 10, rs: 11, r3: 11, up: 12, down: 13, left: 14, right: 15, home: 16, guide: 16 };
  const axisIndex = { lx: 0, ly: 1, rx: 2, ry: 3 };
  const pads = [];
  const nativePads = Navigator.prototype.getGamepads;
  Navigator.prototype.getGamepads = function getGamepads() {
    const list = nativePads ? Array.from(nativePads.call(this) || []) : [];
    for (const pad of pads) if (pad) list[pad.index] = pad;
    return list;
  };
  const padEvent = (type, pad) => {
    const event = new Event(type);
    Object.defineProperty(event, 'gamepad', { value: pad });
    window.dispatchEvent(event);
  };
  const button = (value) => ({ pressed: value > 0.5, touched: value > 0, value });

  const pointerInit = (x, y, extra) => ({ bubbles: true, cancelable: true, composed: true, view: window, clientX: x, clientY: y, screenX: x, screenY: y, ...extra });
  const touches = new Map();

  const control = {
    locked: () => !!locked,
    look(dx, dy) {
      const x = innerWidth / 2, y = innerHeight / 2;
      const target = locked || document.elementFromPoint(x, y) || document.body;
      const init = pointerInit(x, y, { movementX: dx, movementY: dy, buttons: 0 });
      target.dispatchEvent(new PointerEvent('pointermove', { ...init, pointerId: 1, pointerType: 'mouse', isPrimary: true }));
      const move = new MouseEvent('mousemove', init);
      if (move.movementX !== dx) { Object.defineProperty(move, 'movementX', { value: dx }); Object.defineProperty(move, 'movementY', { value: dy }); }
      target.dispatchEvent(move);
      return true;
    },
    pad(state = {}, index = 0) {
      let pad = pads[index];
      if (!pad) {
        pad = { id: 'CommissionAI virtual gamepad (STANDARD GAMEPAD Vendor: 045e Product: 028e)', index, connected: true, mapping: 'standard', timestamp: performance.now(), axes: [0, 0, 0, 0], buttons: Array.from({ length: 17 }, () => button(0)), vibrationActuator: null, hapticActuators: [] };
        pads[index] = pad;
        padEvent('gamepadconnected', pad);
      }
      for (const [name, value] of Object.entries(state)) {
        const lower = name.toLowerCase();
        if (lower in axisIndex) pad.axes[axisIndex[lower]] = Math.max(-1, Math.min(1, Number(value) || 0));
        else if (lower === 'axes' && Array.isArray(value)) value.forEach((axis, at) => { pad.axes[at] = Math.max(-1, Math.min(1, Number(axis) || 0)); });
        else {
          const at = lower in buttonIndex ? buttonIndex[lower] : Number(lower);
          if (Number.isInteger(at) && at >= 0 && at < pad.buttons.length) pad.buttons[at] = button(typeof value === 'number' ? Math.max(0, Math.min(1, value)) : value ? 1 : 0);
        }
      }
      pad.timestamp = performance.now();
      return { index, axes: [...pad.axes], pressed: pad.buttons.map((entry, at) => entry.pressed ? at : -1).filter((at) => at >= 0) };
    },
    unpad(index = 0) {
      const pad = pads[index];
      if (!pad) return false;
      pad.connected = false;
      pads[index] = undefined;
      padEvent('gamepaddisconnected', pad);
      return true;
    },
    touch(id, phase, x, y) {
      const pointerId = 20 + Number(id);
      const target = phase === 'down' ? document.elementFromPoint(x, y) || document.body : touches.get(pointerId) || document.elementFromPoint(x, y) || document.body;
      const type = { down: 'pointerdown', move: 'pointermove', up: 'pointerup', cancel: 'pointercancel' }[phase] || 'pointermove';
      if (phase === 'down') touches.set(pointerId, target);
      if (phase === 'up' || phase === 'cancel') touches.delete(pointerId);
      target.dispatchEvent(new PointerEvent(type, pointerInit(x, y, { pointerId, pointerType: 'touch', isPrimary: touches.size <= 1 && pointerId === Math.min(...touches.keys(), pointerId), buttons: phase === 'up' || phase === 'cancel' ? 0 : 1, width: 20, height: 20, pressure: phase === 'up' ? 0 : 0.5 })));
      return touches.size;
    },
    gesture(type, x, y, scale, rotation = 0) {
      const target = document.elementFromPoint(x, y) || document.body;
      let event;
      try { event = new GestureEvent(type, { bubbles: true, cancelable: true, scale, rotation, clientX: x, clientY: y }); } catch (error) { event = null; }
      if (!event || event.scale !== scale) { event = new Event(type, { bubbles: true, cancelable: true }); Object.defineProperties(event, { scale: { value: scale }, rotation: { value: rotation }, clientX: { value: x }, clientY: { value: y } }); }
      target.dispatchEvent(event);
      return true;
    },
  };
  Object.defineProperty(window, key, { value: control });
  return control;
})()
