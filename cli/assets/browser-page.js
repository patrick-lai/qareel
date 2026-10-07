(options) => {
  const started = performance.now();
  const KEY = '__commissionPageMap';
  const strip = value => String(value ?? '').replace(/[\u0000-\u0008\u000e-\u001f\u007f]/g, '');
  const squash = value => value.replace(/\s+/g, ' ').trim();
  const norm = value => squash(strip(value));
  const clip = (value, max) => value.length > max ? value.slice(0, max - 1) + '…' : value;
  const fit = (value, max) => value.includes('\u0001') ? value : clip(value, max);
  const fingerprint = node => [node.localName, node.getAttribute('role') || '', node.getAttribute('data-testid') || node.getAttribute('data-test-id') || '', node.getAttribute('aria-label') || '', node.getAttribute('name') || '', node.getAttribute('type') || '', node.getAttribute('placeholder') || '', norm(String(node.textContent || '').slice(0, 400)).slice(0, 40)].join('|');
  const hash = text => {
    let value = 0x811c9dc5;
    for (let index = 0; index < text.length; index++) value = Math.imul(value ^ text.charCodeAt(index), 0x01000193) >>> 0;
    return value.toString(36);
  };
  let state = window[KEY];
  if (!state || state.version !== 1 || !(state.nodes instanceof Map) || !(state.ids instanceof WeakMap)) {
    state = { version: 1, token: Array.from(crypto.getRandomValues(new Uint32Array(2)), part => part.toString(36)).join(''), next: 0, ids: new WeakMap(), nodes: new Map(), digests: new Map() };
    try { Object.defineProperty(window, KEY, { value: state, configurable: true }); } catch { window[KEY] = state; }
  }
  const children = node => node.shadowRoot ? node.shadowRoot.childNodes : node.localName === 'slot' ? node.assignedNodes({ flatten: true }) : node.childNodes;
  const FORM = new Set(['input', 'select', 'textarea']);
  const shown = node => {
    if (node.checkVisibility ? node.checkVisibility(FORM.has(node.localName) ? { checkVisibilityCSS: true } : { checkOpacity: true, checkVisibilityCSS: true }) : node.getClientRects().length > 0) return 1;
    return getComputedStyle(node).display === 'contents' ? 2 : 0;
  };
  if (options.resolve) {
    const [token, id, digest] = String(options.resolve).split(':');
    if (token !== state.token) return null;
    const entry = state.nodes.get(Number(id));
    if (!entry) return null;
    const node = entry.ref.deref();
    if (node && node.isConnected && hash(fingerprint(node)) === digest) return node;
    const found = [];
    const stack = [document.documentElement];
    while (stack.length) {
      const candidate = stack.pop();
      if (candidate.nodeType === 1 && candidate.localName === entry.tag && hash(fingerprint(candidate)) === digest && shown(candidate) === 1) {
        found.push(candidate);
        if (found.length > 1) return null;
      }
      const list = candidate.nodeType === 1 && candidate.shadowRoot ? [...candidate.shadowRoot.childNodes, ...candidate.childNodes] : candidate.childNodes;
      for (const child of list) if (child.nodeType === 1 || child.nodeType === 11) stack.push(child);
    }
    if (found.length !== 1) return null;
    state.ids.set(found[0], Number(id));
    entry.ref = new WeakRef(found[0]);
    return found[0];
  }
  const budget = Math.max(500, Math.min(40000, Number(options.max_chars) || 12000));
  const deadline = started + Math.max(50, Math.min(2000, Number(options.time_ms) || 500));
  const only = options.interactive === true;
  const SKIP = new Set(['script', 'style', 'noscript', 'template', 'head', 'meta', 'link', 'title', 'svg', 'canvas', 'video', 'audio', 'picture', 'img', 'source', 'track', 'map', 'object', 'embed', 'math', 'option', 'optgroup', 'datalist']);
  const INLINE = new Set(['span', 'a', 'b', 'strong', 'em', 'i', 'u', 's', 'small', 'code', 'kbd', 'samp', 'var', 'abbr', 'cite', 'q', 'mark', 'sub', 'sup', 'time', 'data', 'label', 'font', 'bdi', 'bdo', 'del', 'ins', 'wbr', 'br']);
  const ROLES = new Set(['button', 'link', 'checkbox', 'radio', 'switch', 'tab', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option', 'combobox', 'textbox', 'searchbox', 'spinbutton', 'slider', 'treeitem']);
  const FIELDS = new Set(['textbox', 'searchbox', 'combobox', 'spinbutton', 'checkbox', 'radio', 'switch', 'slider', 'listbox', 'file', 'color', 'date', 'datetime-local', 'month', 'time', 'week']);
  const LANDMARKS = { nav: 'nav', footer: 'footer', aside: 'aside', navigation: 'nav', contentinfo: 'footer', complementary: 'aside' };
  const MARKERS = { dialog: 'dialog', alertdialog: 'dialog', menu: 'menu', listbox: 'listbox', tablist: 'tabs', alert: 'alert', tooltip: 'tooltip' };
  const CONTROLS = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=switch],[role=tab],[role=menuitem],[role=option],[role=combobox],[role=textbox],[contenteditable=true],[contenteditable=""]';
  const secretPattern = /(?:password|passwd|secret|token|api[-_ ]?key|authorization|card[-_ ]?(?:number|security|cvc|cvv)|one-time-code|cc-number|cc-csc)/i;
  const vw = innerWidth, vh = innerHeight, sy = scrollY, sx = scrollX;
  const reactKeys = [];
  let scanned = 0, controls = 0, truncated = false, framework = 0;
  const late = () => { if (++scanned % 64 === 0 && performance.now() > deadline) truncated = true; return truncated; };
  const propsOf = node => {
    for (const key of reactKeys) if (node[key]) return node[key];
    for (const key of Object.keys(node)) if (key.startsWith('__reactProps$')) { reactKeys.push(key); return node[key]; }
    return null;
  };
  const handler = node => {
    const props = propsOf(node);
    if (props && (props.onClick || props.onMouseDown || props.onPointerDown || props.onMouseUp || props.onPointerUp)) return true;
    const vue = node._vei;
    if (vue && (vue.onClick || vue.onMousedown || vue.onPointerdown || vue.onMouseup)) return true;
    if (node.onclick || node.hasAttribute('onclick') || node.hasAttribute('ng-click') || node.hasAttribute('(click)') || node.hasAttribute('@click') || node.hasAttribute('v-on:click')) return true;
    const events = window.jQuery?._data?.(node, 'events');
    return Boolean(events && (events.click || events.mousedown));
  };
  const pointerLeaf = node => {
    if (node.localName === 'label' || node.childElementCount > 4 || (node.textContent || '').length > 80) return false;
    if (getComputedStyle(node).cursor !== 'pointer') return false;
    const parent = node.parentElement;
    return !parent || getComputedStyle(parent).cursor !== 'pointer';
  };
  const inputRole = node => {
    const type = (node.getAttribute('type') || 'text').toLowerCase();
    if (type === 'hidden') return null;
    if (type === 'checkbox' || type === 'radio') return type;
    if (['button', 'submit', 'reset', 'image'].includes(type)) return 'button';
    if (type === 'search') return 'searchbox';
    if (type === 'number') return 'spinbutton';
    if (type === 'range') return 'slider';
    if (['file', 'color', 'date', 'datetime-local', 'month', 'time', 'week'].includes(type)) return type;
    return 'textbox';
  };
  const control = node => {
    const explicit = (node.getAttribute('role') || '').split(' ')[0];
    if (ROLES.has(explicit)) return explicit;
    switch (node.localName) {
      case 'a': if (node.hasAttribute('href')) return 'link'; break;
      case 'button': return 'button';
      case 'select': return node.multiple ? 'listbox' : 'combobox';
      case 'textarea': return 'textbox';
      case 'input': return inputRole(node);
      case 'summary': return 'button';
      case 'body': case 'html': return null;
    }
    if (node.isContentEditable && !node.parentElement?.isContentEditable) return 'textbox';
    const tabbable = node.getAttribute('tabindex') !== null && Number(node.getAttribute('tabindex')) >= 0;
    if (handler(node) || (tabbable && !explicit && getComputedStyle(node).cursor === 'pointer') || (options.pointer !== false && !explicit && pointerLeaf(node))) {
      const rect = node.getBoundingClientRect();
      if (rect.height > 400 || rect.width * rect.height > vw * vh * 0.4 || (rect.width === 0 && rect.height === 0)) return null;
      return 'clickable';
    }
    return null;
  };
  const secret = node => node.type === 'password' || secretPattern.test(`${node.name || ''} ${node.id || ''} ${node.getAttribute('autocomplete') || ''} ${node.getAttribute('aria-label') || ''}`);
  const textOf = (node, max) => {
    const raw = node.textContent || '';
    if (raw.length > 4000) return clip(norm(raw.slice(0, 4000)), max);
    return clip(norm(node.innerText) || norm(raw), max);
  };
  const iconName = node => {
    for (const element of node.querySelectorAll('svg, i, span[class*="icon" i], [data-icon], img')) {
      const label = element.getAttribute('aria-label') || element.getAttribute('title') || element.querySelector?.('title')?.textContent || element.getAttribute('alt') || element.getAttribute('data-icon');
      if (norm(label)) return norm(label);
      const match = /(?:octicon|icon|fa|glyph)[-_]([a-z][a-z0-9-]{2,})/i.exec(element.getAttribute('class') || '');
      if (match) return match[1].replace(/-/g, ' ');
      if (element.getAttribute('data-testid')) return element.getAttribute('data-testid').replace(/[-_]/g, ' ');
    }
    return '';
  };
  const nameOf = (node, role) => {
    const root = node.getRootNode();
    const labelled = (node.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean).slice(0, 6).map(key => root.getElementById?.(key) || document.getElementById(key)).filter(Boolean).map(element => textOf(element, 120)).join(' ');
    let name = norm(labelled) || norm(node.getAttribute('aria-label'));
    const field = FIELDS.has(role) && FORM.has(node.localName);
    if (!name && field) name = norm(Array.from(node.labels || []).map(label => textOf(label, 120)).join(' ')) || norm(node.getAttribute('title')) || norm(node.getAttribute('placeholder')) || norm(node.getAttribute('name'));
    if (!name && node.localName === 'input' && ['button', 'submit', 'reset'].includes(node.type)) name = norm(node.value);
    if (!name && !field && !node.isContentEditable) name = textOf(node, role === 'clickable' ? 120 : 100);
    if (!name) name = norm(node.getAttribute('title'));
    if (!name) { const props = propsOf(node); name = norm(props && (props.title || props.label || props.tooltip || (typeof props.content === 'string' ? props.content : ''))); }
    if (!name) name = iconName(node);
    if (!name) name = norm(node.getAttribute('data-testid') || node.getAttribute('data-test-id') || '').replace(/[-_]/g, ' ');
    return clip(name, role === 'clickable' ? 120 : 100);
  };
  const refs = {};
  const pending = [];
  const counts = new Map();
  const register = node => {
    const digest = hash(fingerprint(node));
    let id = state.ids.get(node);
    if (id === undefined || refs[id] !== undefined) {
      const prior = state.digests.get(digest);
      const alive = prior === undefined ? null : state.nodes.get(prior)?.ref.deref();
      id = prior !== undefined && refs[prior] === undefined && !(alive && alive.isConnected) ? prior : ++state.next;
      state.ids.set(node, id);
    }
    state.nodes.set(id, { ref: new WeakRef(node), tag: node.localName, digest });
    counts.set(digest, counts.has(digest) ? -1 : id);
    refs[id] = digest;
    return id;
  };
  let deepActive = document.activeElement;
  while (deepActive?.shadowRoot?.activeElement) deepActive = deepActive.shadowRoot.activeElement;
  const describe = (node, role, frame, prose) => {
    controls++;
    if (role === 'clickable') framework++;
    const name = nameOf(node, role);
    let line = role;
    if (name) line += ` "${name.replace(/"/g, '\'')}"`;
    else {
      const near = node.nextElementSibling || node.previousElementSibling || node.parentElement;
      const hint = near ? clip(norm(near.textContent), 40) : '';
      if (hint) line += ` (near "${hint.replace(/"/g, '\'')}")`;
    }
    if (!frame) { pending.push(node); line += ` [ref=\u0001${pending.length - 1}\u0001]`; }
    const flags = [];
    if (node.disabled || node.getAttribute('aria-disabled') === 'true') flags.push('disabled');
    if (['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio'].includes(role)) flags.push((node.localName === 'input' ? node.checked : node.getAttribute('aria-checked') === 'true') ? 'checked' : 'unchecked');
    const expanded = node.getAttribute('aria-expanded');
    if (expanded === 'true') flags.push('expanded'); else if (expanded === 'false') flags.push('collapsed');
    if (node.getAttribute('aria-selected') === 'true') flags.push('selected');
    if (node.getAttribute('aria-pressed') === 'true') flags.push('pressed');
    if (node.getAttribute('aria-current') && node.getAttribute('aria-current') !== 'false') flags.push('current');
    if (node.required || node.getAttribute('aria-required') === 'true') flags.push('required');
    if (node.getAttribute('aria-invalid') === 'true' || (node.validity && node.willValidate && !node.validity.valid && node.value)) flags.push('invalid');
    if (node.readOnly) flags.push('readonly');
    if (node === deepActive) flags.push('focused');
    if (flags.length) line += ` (${flags.join(', ')})`;
    if (role === 'link' && !only && !prose) {
      const href = node.getAttribute('href') || '';
      try {
        const url = new URL(href, location.href);
        const path = url.origin === location.origin ? url.pathname + url.search : '';
        if (path && path.length <= 60 && path !== location.pathname + location.search && !href.startsWith('#')) line += ` →${path}`;
      } catch {}
    }
    if (node.localName === 'select') {
      const picked = Array.from(node.selectedOptions || []).map(option => norm(option.label || option.textContent)).join(', ');
      line += `: ${JSON.stringify(clip(picked, 80))}`;
      const all = Array.from(node.options || []).map(option => norm(option.label || option.textContent)).filter(Boolean);
      if (all.length) line += ` options: ${all.slice(0, 12).map(value => clip(value, 30)).join(' | ')}${all.length > 12 ? ` | +${all.length - 12} more` : ''}`;
    } else if (FORM.has(node.localName)) {
      if (secret(node)) line += ': (secret field: never shown; the user must type it)';
      else if (node.type === 'radio' || node.type === 'checkbox') { if (!name && node.value && node.value !== 'on') line += ` value=${JSON.stringify(clip(node.value, 40))}`; }
      else if (['button', 'submit', 'reset', 'image', 'file'].includes(node.type)) {}
      else if (node.value) line += `: ${JSON.stringify(clip(node.value, 200))}`;
      else if (node.placeholder && node.placeholder !== name) line += ` placeholder=${JSON.stringify(clip(node.placeholder, 60))}`;
    } else if (node.isContentEditable) {
      const text = textOf(node, 300);
      line += text ? ` rich text: ${JSON.stringify(text)}` : ' (empty rich text)';
    } else if (['combobox', 'textbox', 'searchbox', 'slider', 'spinbutton'].includes(role)) {
      const value = norm(node.getAttribute('aria-valuetext') || node.getAttribute('aria-valuenow') || '');
      if (value) line += `: ${JSON.stringify(clip(value, 80))}`;
    }
    return line;
  };
  const lines = [];
  let aside = 0;
  const push = (text, y, kind) => {
    if (!text) return;
    const last = lines[lines.length - 1];
    if (last && last.text === text) return;
    lines.push({ text, y, kind, aside: aside > 0 || kind === 'n' });
  };
  const topOf = node => node.getBoundingClientRect().top + sy;
  const skipped = node => SKIP.has(node.localName) || node.hidden || node.getAttribute('aria-hidden') === 'true' || node.hasAttribute('inert');
  const offPage = node => { const rect = node.getBoundingClientRect(); return rect.right + sx <= 0 || rect.bottom + sy <= 0; };
  const wrapper = (node, role) => role === 'clickable' && node.querySelector(CONTROLS);
  const labelFor = node => node.localName === 'label' && node.control && node.control !== node;
  const labelled = (node, frame) => {
    const target = node.control;
    const kind = node.contains(target) && shown(target) ? control(target) : null;
    return kind ? describe(target, kind, frame) : '';
  };
  const inline = (node, frame, context, bare) => {
    let out = '';
    const visit = parent => {
      const prose = Array.prototype.some.call(parent.childNodes, part => part.nodeType === 3 && part.nodeValue.trim());
      for (const child of children(parent)) {
        if (child.nodeType === 3) { if (!bare) out += strip(child.nodeValue); continue; }
        if (child.nodeType !== 1 || skipped(child) || late()) continue;
        const seen = shown(child);
        if (!seen) continue;
        const role = seen === 1 ? control(child) : null;
        if (role && !wrapper(child, role)) {
          if (!offPage(child)) { out += ` ${describe(child, role, frame, prose)} `; context.controls++; }
          continue;
        }
        if (labelFor(child)) { const piece = labelled(child, frame); if (piece) { out += ` ${piece} `; context.controls++; } continue; }
        if (child.localName === 'br') { out += ' '; continue; }
        const block = !INLINE.has(child.localName);
        if (block) out += ' ';
        visit(child);
        if (block) out += ' ';
      }
    };
    visit(node);
    return squash(out);
  };
  const headingLevel = node => {
    const match = /^h([1-6])$/.exec(node.localName);
    if (match) return Number(match[1]);
    return node.getAttribute('role') === 'heading' ? Number(node.getAttribute('aria-level')) || 2 : 0;
  };
  const rowCells = row => Array.from(row.localName === 'tr' ? row.cells : row.querySelectorAll(':scope > [role=cell], :scope > [role=gridcell], :scope > [role=columnheader], :scope > [role=rowheader], :scope > * > [role=cell], :scope > * > [role=gridcell], :scope > * > [role=columnheader], :scope > * > [role=rowheader]'));
  const table = (node, frame) => {
    const rows = node.localName === 'table' ? Array.from(node.rows) : Array.from(node.querySelectorAll('[role=row]'));
    if (!rows.length || rows.some(row => rowCells(row).some(cell => (cell.textContent || '').length > 600 || cell.querySelector('table,[role=table],[role=grid]')))) return false;
    const label = norm(node.getAttribute('aria-label') || node.querySelector('caption')?.textContent || '');
    push(`[table${label ? ` "${clip(label, 80)}"` : ''}]`, topOf(node), 'm');
    let count = 0;
    for (const row of rows) {
      if (late()) break;
      if (shown(row) !== 1) continue;
      const context = { controls: 0 };
      const role = control(row);
      const prefix = role && !wrapper(row, role) ? `${describe(row, role, frame)} ` : '';
      const text = rowCells(row).filter(cell => shown(cell) === 1).map(cell => fit(inline(cell, frame, context, only), 160) || '·').join(' | ');
      if (!text && !prefix) continue;
      if (only && !prefix && !context.controls) continue;
      count++;
      push(`${prefix}| ${text} |`, topOf(row), 'r');
    }
    const total = Number(node.getAttribute('aria-rowcount'));
    if (total > count + 1) push(`(table shows ${count} of ${total} rows; scroll it to load more)`, topOf(node), 'm');
    return true;
  };
  const block = (node, frame) => {
    const role = control(node);
    if (role && !wrapper(node, role)) {
      if (!offPage(node)) push(describe(node, role, frame), topOf(node), 'c');
      return;
    }
    const level = headingLevel(node);
    if (level) { push(`${'#'.repeat(level)} ${clip(inline(node, frame, { controls: 0 }), 200)}`, topOf(node), 'h'); return; }
    const tag = node.localName;
    const aria = node.getAttribute('role') || '';
    if (tag === 'iframe' || tag === 'frame') {
      let doc = null;
      try { doc = node.contentDocument; } catch {}
      const title = norm(node.getAttribute('title') || node.getAttribute('name') || '');
      if (doc?.body) { push(`[frame${title ? ` "${clip(title, 60)}"` : ''}: read only, its controls have no refs]`, topOf(node), 'm'); walk(doc.body, true); }
      else { let host = ''; try { host = new URL(node.src, location.href).host; } catch {} if (host) push(`[frame ${host}${title ? ` "${clip(title, 60)}"` : ''}: cross-origin, not readable]`, topOf(node), 'm'); }
      return;
    }
    if ((tag === 'table' || aria === 'table' || aria === 'grid' || aria === 'treegrid') && table(node, frame)) return;
    const landmark = LANDMARKS[aria] || (aria ? '' : LANDMARKS[tag]);
    if (landmark) {
      const label = norm(node.getAttribute('aria-label') || '');
      const head = `[${landmark}${label ? ` "${clip(label, 60)}"` : ''}]`;
      if ((node.textContent || '').length <= 1500) {
        const context = { controls: 0 };
        const text = inline(node, frame, context, only);
        if (text && (!only || context.controls)) push(`${head} ${text}`, topOf(node), 'n');
        return;
      }
      push(head, topOf(node), 'm');
      aside++;
      walk(node, frame);
      aside--;
      return;
    }
    const marker = MARKERS[aria] || (tag === 'dialog' ? 'dialog' : '');
    if (marker) {
      const label = norm(node.getAttribute('aria-label') || (node.getAttribute('aria-labelledby') ? nameOf(node, 'dialog') : ''));
      push(`[${marker}${label ? ` "${clip(label, 80)}"` : ''}]`, topOf(node), 'm');
    }
    const rect = node.getBoundingClientRect();
    if (rect.height > 0 && rect.height <= 64 && !node.shadowRoot && (node.textContent || '').length <= 400 && !node.querySelector('h1,h2,h3,h4,h5,h6,[role=heading],table,[role=table],[role=grid],iframe,[role=dialog],dialog')) {
      const context = { controls: 0 };
      const text = inline(node, frame, context, only);
      if (text && (!only || context.controls)) push(`${tag === 'li' ? '- ' : ''}${fit(text, 600)}`, rect.top + sy, context.controls ? 'r' : 't');
      return;
    }
    walk(node, frame);
  };
  const walk = (node, frame) => {
    let buffer = '', anchor = null;
    const flush = () => {
      const text = squash(buffer);
      buffer = '';
      if (text && (!only || text.includes('\u0001'))) push(fit(text, 1000), anchor ? topOf(anchor) : 0, text.includes('\u0001') ? 'r' : 't');
      anchor = null;
    };
    for (const child of children(node)) {
      if (truncated) break;
      if (child.nodeType === 3) { if (child.nodeValue.trim()) { buffer += strip(child.nodeValue); anchor ||= child.parentElement; } continue; }
      if (child.nodeType !== 1 || skipped(child) || late()) continue;
      const seen = shown(child);
      if (!seen) continue;
      if (seen === 2) { flush(); walk(child, frame); continue; }
      if (labelFor(child)) { const piece = labelled(child, frame); if (piece) { flush(); push(piece, topOf(child), 'c'); } continue; }
      const role = INLINE.has(child.localName) ? control(child) : null;
      if (role && !wrapper(child, role)) {
        if (offPage(child)) continue;
        if (only || child.getBoundingClientRect().height > 40 || child.querySelector('div,p,li,h1,h2,h3,h4,h5,h6')) { flush(); push(describe(child, role, frame), topOf(child), 'c'); } else { buffer += ` ${describe(child, role, frame, true)} `; anchor ||= child; }
        continue;
      }
      if (INLINE.has(child.localName) && !role && (child.textContent || '').length < 400 && !child.querySelector('div,p,li,h1,h2,h3,h4,h5,h6,table')) {
        const context = { controls: 0 };
        const piece = inline(child, frame, context, only);
        if (piece) { if (only && context.controls) { flush(); push(piece, topOf(child), 'r'); } else { buffer += ` ${piece} `; anchor ||= child; } }
        continue;
      }
      flush();
      block(child, frame);
    }
    flush();
  };
  let root = document.body || document.documentElement;
  let scope = '';
  if (options.root) {
    root = options.root;
    scope = 'subtree';
  } else {
    const modals = Array.from(document.querySelectorAll('dialog, [aria-modal="true"]')).filter(node => { try { return (node.matches(':modal') || node.getAttribute('aria-modal') === 'true') && shown(node) === 1; } catch { return false; } });
    if (modals.length) {
      root = modals[modals.length - 1];
      const label = clip(nameOf(root, 'dialog'), 80);
      scope = `modal${label ? ` "${label.replace(/"/g, '\'')}"` : ''}`;
      push(`[dialog${label ? ` "${label.replace(/"/g, '\'')}"` : ''} (modal: only the dialog is shown until it closes)]`, topOf(root), 'm');
    }
  }
  if (root.nodeType === 1 && root !== document.body && root !== document.documentElement && scope === 'subtree') block(root, false);
  else walk(root, false);
  for (let index = lines.length - 2; index >= 0; index--) {
    const line = lines[index], next = lines[index + 1];
    if (line.kind === 't' && line.text.length <= 40 && line.text.endsWith(':') && (next.kind === 't' || next.kind === 'r') && next.text.length <= 200) {
      lines.splice(index, 2, { text: `${line.text} ${next.text}`, y: line.y, kind: next.kind, aside: line.aside });
    }
  }
  const keep = new Array(lines.length).fill(true);
  if (lines.reduce((sum, line) => sum + line.text.length + 1, 0) > budget) {
    keep.fill(false);
    const top = sy, bottom = sy + vh;
    const distance = line => (line.y < top ? top - line.y + vh * 0.25 : line.y > bottom ? line.y - bottom : 0) - (line.kind === 'h' ? vh * 2 : 0) + (line.aside ? vh * 3 : 0);
    const order = lines.map((line, index) => index).sort((a, b) => distance(lines[a]) - distance(lines[b]) || a - b);
    let used = 0;
    for (const index of order) {
      const size = lines[index].text.length + 1;
      if (used + size > budget - 160) continue;
      keep[index] = true;
      used += size;
    }
  }
  const out = [];
  let gap = 0, omitted = 0;
  const close = index => {
    if (!gap) return;
    out.push(`… ${gap} line${gap === 1 ? '' : 's'} omitted`);
    omitted += gap;
    gap = 0;
  };
  lines.forEach((line, index) => {
    if (!keep[index]) { gap++; return; }
    close(index);
    out.push(line.text.replace(/\u0001(\d+)\u0001/g, (match, slot) => `\u0001${register(pending[Number(slot)])}`));
  });
  close(lines.length);
  if (omitted) out.push(`(${omitted} lines far from the viewport were omitted to stay within ${budget} characters; scroll there, pass selector= for one region, or raise max_chars)`);
  for (const [digest, id] of counts) { if (id > 0) state.digests.set(digest, id); else state.digests.delete(digest); }
  if (state.nodes.size > 3000) for (const [id, entry] of state.nodes) if (refs[id] === undefined && !entry.ref.deref()?.isConnected) state.nodes.delete(id);
  if (truncated) out.push(`… stopped reading after ${Math.round(performance.now() - started)} ms on a very large page; pass selector= to read one region`);
  return {
    token: state.token, url: location.href, title: document.title, scope, text: out.join('\n'), refs,
    scroll: [Math.round(sy), Math.round(document.documentElement.scrollHeight), vw, vh],
    stats: { ms: Math.round(performance.now() - started), controls, framework, lines: lines.length, omitted, scanned, truncated }
  };
}
