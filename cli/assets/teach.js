(function () {
  const BINDING = "__commissionTeach";
  const STATE = "__commissionTeachState";
  const TEST_ATTRIBUTES = ["data-testid", "data-test-id", "data-test", "data-qa", "data-cy", "data-automation-id"];
  const LABEL_ATTRIBUTES = ["name", "aria-label", "placeholder", "title", "alt"];
  const INTERACTIVE_ROLES = new Set(["button", "link", "checkbox", "radio", "switch", "tab", "menuitem", "menuitemcheckbox", "menuitemradio", "option", "treeitem", "combobox", "textbox", "searchbox", "slider", "spinbutton", "gridcell", "row"]);
  const NAME_FROM_CONTENT = new Set(["button", "link", "checkbox", "radio", "switch", "tab", "menuitem", "menuitemcheckbox", "menuitemradio", "option", "treeitem", "heading", "cell", "columnheader", "rowheader", "gridcell", "tooltip"]);
  const TEXT_TYPES = new Set(["", "text", "search", "email", "url", "tel", "number", "password", "date", "datetime-local", "month", "time", "week"]);
  const SKIPPED_TAGS = new Set(["script", "style", "noscript", "template", "head", "title", "meta", "link", "html", "body"]);
  const ALWAYS_KEYS = new Set(["Enter", "Escape", "Tab", "ArrowUp", "ArrowDown", "PageUp", "PageDown"]);
  const OUTSIDE_KEYS = new Set(["ArrowLeft", "ArrowRight", "Home", "End", "Backspace", "Delete"]);
  const EDIT_KEYS = new Set(["a", "c", "v", "x", "z", "y"]);
  const MODIFIERS = new Set(["Shift", "Control", "Alt", "Meta", "CapsLock", "Fn", "AltGraph"]);
  const SECRET = /(pass(word|code|phrase)?|secret|token|one-time-code|otp|2fa|mfa|totp|cvv|cvc|cc-number|cc-csc|card.?number|security.?code|ssn|social.?security)/i;
  const FILL_DELAY = 600;

  function clean(value, limit) {
    return String(value == null ? "" : value).replace(/\s+/g, " ").trim().slice(0, limit || 120);
  }

  function parentOf(node) {
    if (!node) return null;
    if (node.parentElement) return node.parentElement;
    const parent = node.parentNode;
    return parent && parent.host ? parent.host : null;
  }

  function rootOf(node) {
    return node && node.getRootNode ? node.getRootNode() : document;
  }

  function attribute(el, name) {
    return el && el.getAttribute ? el.getAttribute(name) : null;
  }

  function inputType(el) {
    return (attribute(el, "type") || "text").toLowerCase();
  }

  function implicitRole(el) {
    const tag = el.localName;
    if (tag === "a" || tag === "area") return el.hasAttribute("href") ? "link" : null;
    if (tag === "button") return "button";
    if (tag === "input") {
      const type = inputType(el);
      if (type === "checkbox") return "checkbox";
      if (type === "radio") return "radio";
      if (type === "range") return "slider";
      if (type === "number") return "spinbutton";
      if (type === "hidden") return null;
      if (type === "button" || type === "submit" || type === "reset" || type === "image") return "button";
      if (type === "search") return el.hasAttribute("list") ? "combobox" : "searchbox";
      return el.hasAttribute("list") ? "combobox" : "textbox";
    }
    if (tag === "select") return el.multiple || el.size > 1 ? "listbox" : "combobox";
    if (tag === "textarea") return "textbox";
    if (tag === "option") return "option";
    if (/^h[1-6]$/.test(tag)) return "heading";
    if (tag === "img") return attribute(el, "alt") === "" ? "presentation" : "img";
    if (tag === "li") return "listitem";
    if (tag === "tr") return "row";
    if (tag === "td") return "cell";
    if (tag === "th") return "columnheader";
    if (tag === "dialog") return "dialog";
    if (tag === "nav") return "navigation";
    if (el.isContentEditable && !(el.parentElement && el.parentElement.isContentEditable)) return "textbox";
    return null;
  }

  function roleOf(el) {
    if (!el || el.nodeType !== 1) return null;
    const explicit = clean(attribute(el, "role"), 60).split(" ")[0];
    return explicit || implicitRole(el);
  }

  function textOf(el) {
    if (!el) return "";
    const raw = typeof el.innerText === "string" && el.localName !== "select" && el.localName !== "option" ? el.innerText : el.textContent;
    return clean(raw, 400);
  }

  function lookup(el, id) {
    const root = rootOf(el);
    return (root && root.getElementById ? root.getElementById(id) : null) || document.getElementById(id);
  }

  function labelledBy(el) {
    const ids = clean(attribute(el, "aria-labelledby"), 400);
    if (!ids) return "";
    return clean(ids.split(" ").map((id) => {
      const node = lookup(el, id);
      return node ? textOf(node) : "";
    }).join(" "), 200);
  }

  function innerName(el) {
    const inner = el.querySelector ? el.querySelector("[aria-label], img[alt], [title]") : null;
    if (!inner) return "";
    return clean(attribute(inner, "aria-label") || attribute(inner, "alt") || attribute(inner, "title"), 200);
  }

  function nameOf(el) {
    if (!el || el.nodeType !== 1) return "";
    const by = labelledBy(el);
    if (by) return by;
    const aria = clean(attribute(el, "aria-label"), 200);
    if (aria) return aria;
    const tag = el.localName;
    if (tag === "input" || tag === "select" || tag === "textarea") {
      const type = inputType(el);
      if (tag === "input" && (type === "button" || type === "submit" || type === "reset")) return clean(el.value || (type === "submit" ? "Submit" : type === "reset" ? "Reset" : ""), 200);
      if (tag === "input" && type === "image") return clean(attribute(el, "alt") || attribute(el, "value"), 200);
      const labels = el.labels ? clean(Array.from(el.labels).map(textOf).join(" "), 200) : "";
      if (labels) return labels;
      return clean(attribute(el, "title") || attribute(el, "placeholder"), 200);
    }
    if (tag === "img") return clean(attribute(el, "alt") || attribute(el, "title"), 200);
    const role = roleOf(el);
    if ((role && NAME_FROM_CONTENT.has(role)) || tag === "label" || tag === "summary") {
      const content = textOf(el) || innerName(el);
      if (content) return clean(content, 200);
    }
    return clean(attribute(el, "title"), 200);
  }

  function visible(el) {
    if (!el || !el.isConnected || !el.getClientRects || !el.getClientRects().length) return false;
    const style = getComputedStyle(el);
    return style.visibility !== "hidden" && style.display !== "none";
  }

  function everything(root, out) {
    const list = out || [];
    const nodes = root && root.querySelectorAll ? root.querySelectorAll("*") : [];
    for (const el of nodes) {
      if (el.closest("[data-commission-overlay]")) continue;
      list.push(el);
      if (el.shadowRoot) everything(el.shadowRoot, list);
    }
    return list;
  }

  function byRoleAndName(role, name) {
    const want = clean(name, 400).toLowerCase();
    return everything(document).filter((el) => (!role || roleOf(el) === role) && nameOf(el).toLowerCase() === want);
  }

  function deepest(list) {
    return list.filter((el) => !list.some((other) => other !== el && el.contains(other)));
  }

  function byText(text, partial) {
    const want = clean(text, 400).toLowerCase();
    if (!want) return [];
    const rough = everything(document).filter((el) => !SKIPPED_TAGS.has(el.localName) && clean(el.textContent, 100000).toLowerCase().includes(want));
    const exact = deepest(rough.filter((el) => textOf(el).toLowerCase() === want));
    if (exact.length || !partial) return exact;
    return deepest(rough.filter((el) => textOf(el).toLowerCase().includes(want)));
  }

  function deepQuery(selector) {
    const out = [];
    const visit = (root) => {
      try {
        out.push(...root.querySelectorAll(selector));
      } catch (error) {
        return;
      }
      for (const el of root.querySelectorAll("*")) {
        if (el.shadowRoot) visit(el.shadowRoot);
      }
    };
    visit(document);
    return out;
  }

  function cssEscape(value) {
    return window.CSS && CSS.escape ? CSS.escape(value) : String(value).replace(/[^a-zA-Z0-9_-]/g, "\\$&");
  }

  function cssString(value) {
    return String(value).replace(/\\/g, "\\\\").replace(/"/g, "\\\"");
  }

  function stableToken(value) {
    return Boolean(value) && value.length <= 48 && !/\d{3,}/.test(value) && !/^[a-f0-9-]{8,}$/i.test(value) && !/[:[\]/.@\s]/.test(value) && !/^(ember|react|radix|headlessui|mui|rc-|ext-|yui|uid|j_id|__|aria-|id-)/i.test(value);
  }

  function stableClass(value) {
    return stableToken(value) && !/^(css|sc|jsx|emotion|styled|tw|chakra|svelte)-/i.test(value) && !/(^|[_-])(?=[a-z]*\d)(?=\d*[a-z])[a-z0-9]{5,}($|[_-])/i.test(value) && !/__/.test(value);
  }

  function unique(selector, root) {
    try {
      return (root || document).querySelectorAll(selector).length === 1;
    } catch (error) {
      return false;
    }
  }

  function segment(el) {
    let part = el.localName;
    const classes = Array.from(el.classList || []).filter(stableClass).slice(0, 2);
    if (classes.length) part += classes.map((name) => "." + cssEscape(name)).join("");
    const parent = el.parentNode;
    const siblings = parent && parent.children ? Array.from(parent.children).filter((child) => child.localName === el.localName) : [];
    if (siblings.length > 1) part += ":nth-of-type(" + (siblings.indexOf(el) + 1) + ")";
    return part;
  }

  function cssPath(el) {
    const root = rootOf(el);
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1 && node !== document.documentElement && parts.length < 10) {
      const anchor = node.id && stableToken(node.id) ? "#" + cssEscape(node.id) : null;
      parts.unshift(anchor || segment(node));
      const candidate = parts.join(" > ");
      if (unique(candidate, root)) return candidate;
      if (anchor) parts[0] = segment(node);
      node = node.parentElement;
    }
    const whole = parts.join(" > ");
    return whole && unique(whole, root) ? whole : null;
  }

  function xpath(el) {
    if (rootOf(el) !== document || (el.closest && el.closest("svg"))) return null;
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1) {
      if (node.id && stableToken(node.id) && document.querySelectorAll("#" + cssEscape(node.id)).length === 1 && !node.id.includes("\"")) {
        parts.unshift("//*[@id=\"" + node.id + "\"]");
        return parts.join("/");
      }
      const parent = node.parentElement;
      const same = parent ? Array.from(parent.children).filter((child) => child.localName === node.localName) : [node];
      parts.unshift(same.length > 1 ? node.localName + "[" + (same.indexOf(node) + 1) + "]" : node.localName);
      node = parent;
    }
    return "/" + parts.join("/");
  }

  function selectorsFor(el) {
    const out = [];
    const root = rootOf(el);
    const shadow = root !== document;
    const push = (value) => {
      if (value && !out.includes(value)) out.push(value);
    };
    const css = (selector) => (shadow ? "pierce/" + selector : selector);
    for (const name of TEST_ATTRIBUTES) {
      const value = attribute(el, name);
      if (!value) continue;
      const plain = "[" + name + "=\"" + cssString(value) + "\"]";
      if (unique(plain, root)) {
        push(css(plain));
        break;
      }
      if (unique(el.localName + plain, root)) {
        push(css(el.localName + plain));
        break;
      }
    }
    const role = roleOf(el);
    const name = nameOf(el);
    let aria = null;
    if (role && name && name.length <= 80 && role !== "presentation" && role !== "none" && role !== "generic") {
      aria = "aria/" + name + "[role=\"" + role + "\"]";
      if (byRoleAndName(role, name).length === 1) push(aria);
    }
    if (el.id && stableToken(el.id) && unique("#" + cssEscape(el.id), root)) push(css("#" + cssEscape(el.id)));
    for (const name of LABEL_ATTRIBUTES) {
      const value = attribute(el, name);
      if (!value || value.length > 80) continue;
      const selector = el.localName + "[" + name + "=\"" + cssString(value) + "\"]";
      if (unique(selector, root)) {
        push(css(selector));
        break;
      }
    }
    const href = attribute(el, "href");
    if (el.localName === "a" && href && href.length <= 160) {
      const selector = "a[href=\"" + cssString(href) + "\"]";
      if (unique(selector, root)) push(css(selector));
    }
    const text = textOf(el);
    if (text && text.length <= 60 && !["input", "select", "textarea"].includes(el.localName) && byText(text, false).length === 1) push("text/" + text);
    if (aria) push(aria);
    const path = cssPath(el);
    if (path) push(css(path));
    if (!shadow) {
      const x = xpath(el);
      if (x) push("xpath/" + x);
    }
    return out.slice(0, 7);
  }

  function describe(el) {
    const test = TEST_ATTRIBUTES.map((name) => attribute(el, name)).find(Boolean);
    return {
      tag: el.localName,
      role: roleOf(el) || null,
      name: nameOf(el) || null,
      text: clean(textOf(el), 80) || null,
      placeholder: attribute(el, "placeholder") || null,
      test_id: test || null,
      input_type: el.localName === "input" ? inputType(el) : null,
    };
  }

  function origin(event) {
    const path = event.composedPath ? event.composedPath() : [];
    const first = path.find((node) => node && node.nodeType === 1);
    return first || event.target;
  }

  function field(el) {
    for (let node = el; node && node.nodeType === 1; node = parentOf(node)) {
      if (node.localName === "textarea") return node;
      if (node.localName === "input" && TEXT_TYPES.has(inputType(node))) return node;
      if (node.isContentEditable) {
        let top = node;
        while (top.parentElement && top.parentElement.isContentEditable) top = top.parentElement;
        return top;
      }
    }
    return null;
  }

  function toggle(el) {
    return el && el.localName === "input" && (inputType(el) === "checkbox" || inputType(el) === "radio");
  }

  function actionable(el) {
    for (let node = el; node && node.nodeType === 1; node = parentOf(node)) {
      if (node === document.body || node === document.documentElement) break;
      if (node.matches && node.matches("a[href], button, input, select, textarea, summary, option, label, [onclick], [contenteditable='true'], [contenteditable='']")) return node;
      const role = clean(attribute(node, "role"), 60).split(" ")[0];
      if (role && INTERACTIVE_ROLES.has(role)) return node;
    }
    let pointer = null;
    for (let node = el; node && node.nodeType === 1 && node !== document.body; node = parentOf(node)) {
      if (getComputedStyle(node).cursor === "pointer") pointer = node;
      else if (pointer) break;
    }
    return pointer || el;
  }

  function secret(el) {
    if (el.localName === "input" && inputType(el) === "password") return true;
    const hints = [attribute(el, "autocomplete"), attribute(el, "name"), el.id, attribute(el, "aria-label"), attribute(el, "placeholder")].filter(Boolean).join(" ");
    return SECRET.test(hints);
  }

  function valueOf(el) {
    if (el.isContentEditable) return String(el.innerText || "").replace(/\n+$/, "");
    return String(el.value == null ? "" : el.value);
  }

  function topUrl() {
    try {
      return window.top.location.href;
    } catch (error) {
      return document.referrer || location.href;
    }
  }

  function payload(state, body) {
    return JSON.stringify(Object.assign({ t: state.token, url: topUrl(), frame: window === window.top ? null : location.href }, body));
  }

  function fillBody(el) {
    const hidden = secret(el);
    return { type: "fill", target: describe(el), selectors: selectorsFor(el), value: hidden ? "" : valueOf(el).slice(0, 5000), sensitive: hidden };
  }

  function chord(event) {
    const parts = [];
    if (event.ctrlKey) parts.push("Control");
    if (event.altKey) parts.push("Alt");
    if (event.metaKey) parts.push("Meta");
    if (event.shiftKey && (event.key.length > 1 || parts.length)) parts.push("Shift");
    parts.push(event.key === " " ? "Space" : event.key);
    return parts.join("+");
  }

  function deepActive() {
    let active = document.activeElement;
    while (active && active.shadowRoot && active.shadowRoot.activeElement) active = active.shadowRoot.activeElement;
    return active;
  }

  function record(token) {
    const existing = window[STATE];
    if (existing) {
      existing.token = token;
      return true;
    }
    const state = { token, pending: new Map(), handlers: [], enter: 0 };
    window[STATE] = state;
    const send = (body) => {
      try {
        if (typeof window[BINDING] === "function") window[BINDING](payload(state, body));
      } catch (error) {
        return;
      }
    };
    const flushOne = (el) => {
      const timer = state.pending.get(el);
      if (timer === undefined) return;
      clearTimeout(timer);
      state.pending.delete(el);
      send(fillBody(el));
    };
    const flushAll = () => {
      for (const el of Array.from(state.pending.keys())) flushOne(el);
    };
    state.flush = () => {
      const out = [];
      for (const [el, timer] of Array.from(state.pending.entries())) {
        clearTimeout(timer);
        state.pending.delete(el);
        try {
          out.push(payload(state, fillBody(el)));
        } catch (error) {
          continue;
        }
      }
      return out;
    };
    const listen = (type, handler) => {
      const wrapped = (event) => {
        try {
          handler(event);
        } catch (error) {
          return;
        }
      };
      window.addEventListener(type, wrapped, true);
      state.handlers.push([type, wrapped]);
    };
    listen("input", (event) => {
      if (!event.isTrusted) return;
      const el = field(origin(event));
      if (!el) return;
      const timer = state.pending.get(el);
      if (timer !== undefined) clearTimeout(timer);
      state.pending.set(el, setTimeout(() => {
        state.pending.delete(el);
        send(fillBody(el));
      }, FILL_DELAY));
    });
    listen("change", (event) => {
      const el = origin(event);
      if (!el || el.nodeType !== 1) return;
      if (el.localName === "select") {
        flushAll();
        const picked = Array.from(el.selectedOptions || []).map((option) => clean(option.label || option.textContent, 200)).filter(Boolean);
        send({ type: "select", target: describe(el), selectors: selectorsFor(el), value: picked.join(", "), sensitive: false });
        return;
      }
      if (toggle(el)) {
        flushAll();
        send({ type: "check", target: describe(el), selectors: selectorsFor(el), value: String(Boolean(el.checked)), sensitive: false });
        return;
      }
      const text = field(el);
      if (text) {
        if (state.pending.has(text)) flushOne(text);
        else send(fillBody(text));
      }
    });
    listen("focusout", (event) => {
      const el = field(origin(event));
      if (el) flushOne(el);
    });
    listen("click", (event) => {
      if (!event.isTrusted || event.button !== 0) return;
      if (event.detail === 0 && performance.now() - state.enter < 500) return;
      flushAll();
      const el = actionable(origin(event));
      if (!el || el.nodeType !== 1) return;
      if (toggle(el) || el.localName === "select" || el.localName === "option" && el.closest("select")) return;
      if (field(el) === el) return;
      if (el.localName === "label" && el.control) return;
      send({ type: "click", target: describe(el), selectors: selectorsFor(el), value: null, sensitive: false });
    });
    listen("dblclick", (event) => {
      if (!event.isTrusted) return;
      const el = actionable(origin(event));
      if (!el || el.nodeType !== 1 || field(el) === el) return;
      send({ type: "dblclick", target: describe(el), selectors: selectorsFor(el), value: null, sensitive: false });
    });
    listen("keydown", (event) => {
      if (!event.isTrusted || event.repeat || event.isComposing || MODIFIERS.has(event.key)) return;
      const el = origin(event);
      const text = field(el);
      const modified = event.ctrlKey || event.metaKey || event.altKey;
      if (modified) {
        if (text && EDIT_KEYS.has(event.key.toLowerCase())) return;
      } else if (ALWAYS_KEYS.has(event.key)) {
        if (event.key === "Enter" && text && (text.localName === "textarea" || text.isContentEditable)) return;
        if (event.key === "Tab" && text) return;
      } else if (OUTSIDE_KEYS.has(event.key)) {
        if (text) return;
      } else {
        return;
      }
      flushAll();
      if (event.key === "Enter") state.enter = performance.now();
      const active = deepActive();
      const target = text || (active && active !== document.body ? active : null);
      send({ type: "key", target: target ? describe(target) : null, selectors: target ? selectorsFor(target) : [], value: chord(event), sensitive: false });
    });
    listen("pagehide", () => flushAll());
    return true;
  }

  function stop() {
    const state = window[STATE];
    if (!state) return false;
    for (const [type, handler] of state.handlers) window.removeEventListener(type, handler, true);
    for (const timer of state.pending.values()) clearTimeout(timer);
    delete window[STATE];
    return true;
  }

  function flush() {
    const state = window[STATE];
    return state && state.flush ? state.flush() : [];
  }

  function locate(selector) {
    const text = String(selector == null ? "" : selector).trim();
    if (!text) return null;
    let matches = [];
    if (text.startsWith("aria/")) {
      const parsed = /^([\s\S]*?)(?:\[role="([^"]*)"\])?$/.exec(text.slice(5));
      const name = parsed ? parsed[1] : text.slice(5);
      const role = parsed && parsed[2] ? parsed[2] : null;
      matches = byRoleAndName(role, name);
    } else if (text.startsWith("text/")) {
      matches = byText(text.slice(5), true);
    } else if (text.startsWith("xpath/")) {
      const result = document.evaluate(text.slice(6), document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
      for (let index = 0; index < result.snapshotLength; index += 1) matches.push(result.snapshotItem(index));
    } else if (text.startsWith("pierce/")) {
      matches = deepQuery(text.slice(7));
    } else {
      matches = Array.from(document.querySelectorAll(text));
      if (!matches.length) matches = deepQuery(text);
    }
    matches = matches.filter((node) => node && node.nodeType === 1);
    return matches.find(visible) || matches[0] || null;
  }

  return { record, stop, flush, locate };
})()
