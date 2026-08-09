// The DOM object model, built on the native primitives in `dom_api.rs`.
//
// This is JavaScript on purpose. Event listeners, timer callbacks and promise
// resolvers are all GC-managed values, and holding them on the Rust side would
// mean tracing them through Boa's collector. Keeping them here means the engine
// that already manages their lifetime keeps managing it, and the Rust surface
// stays plain ids and strings.
//
// Nothing here caches the tree. A wrapper holds a node id and asks for
// everything else, so a JS reference cannot go stale in a way the document
// disagrees with.
(function () {
  "use strict";

  const api = globalThis.__h5i;

  // ── nodes ────────────────────────────────────────────────────────────────

  const wrappers = new Map(); // id -> Node, so identity holds across lookups

  function wrap(id) {
    if (id === null || id === undefined) return null;
    let existing = wrappers.get(id);
    if (existing) return existing;
    const node = api.isElement(id) ? new Element(id) : new Text(id);
    wrappers.set(id, node);
    return node;
  }

  class ClassList {
    constructor(node) { this._node = node; }
    _all() {
      const raw = api.getAttr(this._node._id, "class") || "";
      return raw.split(/\s+/).filter(Boolean);
    }
    _write(list) {
      api.setAttr(this._node._id, "class", list.join(" "));
    }
    add(...names) {
      const list = this._all();
      for (const n of names) if (!list.includes(n)) list.push(n);
      this._write(list);
    }
    remove(...names) {
      this._write(this._all().filter((n) => !names.includes(n)));
    }
    contains(name) { return this._all().includes(name); }
    toggle(name, force) {
      const has = this.contains(name);
      const want = force === undefined ? !has : !!force;
      if (want) this.add(name); else this.remove(name);
      return want;
    }
    get length() { return this._all().length; }
    toString() { return this._all().join(" "); }
  }

  class Node {
    constructor(id) { this._id = id; }

    get nodeType() { return api.isElement(this._id) ? 1 : 3; }
    get parentNode() { return wrap(api.parent(this._id)); }
    get parentElement() { return this.parentNode; }
    get childNodes() { return api.children(this._id).map(wrap); }
    get firstChild() { return this.childNodes[0] || null; }
    get lastChild() { const c = this.childNodes; return c[c.length - 1] || null; }

    get textContent() { return api.getText(this._id); }
    set textContent(value) { api.setText(this._id, String(value)); }

    appendChild(child) {
      // Inserting a fragment inserts its children and leaves the fragment
      // behind, which is the whole reason a fragment exists.
      if (child instanceof DocumentFragment) {
        for (const kid of child.childNodes) api.append(this._id, kid._id);
        child._children.length = 0;
        return child;
      }
      api.append(this._id, child._id);
      return child;
    }
    insertBefore(child, anchor) {
      if (!anchor) return this.appendChild(child);
      if (child instanceof DocumentFragment) {
        for (const kid of child.childNodes) api.insertBefore(anchor._id, kid._id);
        child._children.length = 0;
        return child;
      }
      api.insertBefore(anchor._id, child._id);
      return child;
    }
    cloneNode(deep) {
      const copy = this.nodeType === 3
        ? document.createTextNode(this.textContent)
        : document.createElement(this.tagName);
      if (this.nodeType === 1) {
        if (this.className) copy.className = this.className;
        const style = api.getAttr(this._id, "style");
        if (style) copy.setAttribute("style", style);
        if (deep) copy.innerHTML = this.innerHTML;
      }
      return copy;
    }
    get nextSibling() {
      const kids = this.parentNode ? this.parentNode.childNodes : [];
      const at = kids.findIndex((n) => n._id === this._id);
      return at >= 0 ? kids[at + 1] || null : null;
    }
    get previousSibling() {
      const kids = this.parentNode ? this.parentNode.childNodes : [];
      const at = kids.findIndex((n) => n._id === this._id);
      return at > 0 ? kids[at - 1] : null;
    }
    removeChild(child) { api.removeNode(child._id); return child; }
    remove() { api.removeNode(this._id); }
    append(...items) {
      for (const item of items) {
        this.appendChild(item instanceof Node ? item : document.createTextNode(String(item)));
      }
    }
    contains(other) {
      for (let n = other; n; n = n.parentNode) if (n._id === this._id) return true;
      return false;
    }

    addEventListener(type, handler, options) {
      if (!handler) return;
      const capture = options === true || (options && options.capture) || false;
      listeners.push({ id: this._id, type: String(type), handler, capture });
    }
    removeEventListener(type, handler) {
      for (let i = listeners.length - 1; i >= 0; i--) {
        const l = listeners[i];
        if (l.id === this._id && l.type === String(type) && l.handler === handler) {
          listeners.splice(i, 1);
        }
      }
    }
    dispatchEvent(event) { return dispatch(this, event); }
  }

  // A holder that is not in the document. Returning a `<div>` — which is what
  // this did before — injected a real element that the page never created,
  // breaking `.parent > .child` and the layout under it. Children live here
  // until the fragment is inserted, and then they move.
  class DocumentFragment {
    constructor() { this._children = []; this.nodeType = 11; }
    get childNodes() { return this._children.slice(); }
    get firstChild() { return this._children[0] || null; }
    appendChild(child) { this._children.push(child); return child; }
    append(...items) {
      for (const item of items) {
        this.appendChild(item instanceof Node ? item : document.createTextNode(String(item)));
      }
    }
    removeChild(child) {
      const at = this._children.indexOf(child);
      if (at >= 0) this._children.splice(at, 1);
      return child;
    }
    querySelector() { api.unsupported("DocumentFragment.querySelector"); return null; }
  }

  class Text extends Node {
    get data() { return this.textContent; }
    set data(v) { this.textContent = v; }
  }

  class Element extends Node {
    get tagName() { return api.tagName(this._id); }
    get nodeName() { return this.tagName; }
    get children() { return this.childNodes.filter((n) => n.nodeType === 1); }

    getAttribute(name) { return api.getAttr(this._id, String(name)); }
    setAttribute(name, value) { api.setAttr(this._id, String(name), String(value)); }
    removeAttribute(name) { api.removeAttr(this._id, String(name)); }
    hasAttribute(name) { return api.getAttr(this._id, String(name)) !== null; }

    get id() { return this.getAttribute("id") || ""; }
    set id(v) { this.setAttribute("id", v); }
    get className() { return this.getAttribute("class") || ""; }
    set className(v) { this.setAttribute("class", v); }
    get classList() { return new ClassList(this); }

    get value() { return api.getValue(this._id); }
    set value(v) { api.setValue(this._id, String(v)); }

    // Real serialisation. Returning textContent here silently stripped every
    // tag, so `el.innerHTML = el.innerHTML` destroyed the subtree.
    get innerHTML() { return api.innerHtml(this._id); }
    set innerHTML(html) { api.setInnerHtml(this._id, String(html)); }
    get outerHTML() { return api.outerHtml(this._id); }

    get style() { return new StyleDeclaration(this); }

    get dataset() {
      const node = this;
      return new Proxy({}, {
        get(_t, key) {
          if (typeof key !== "string") return undefined;
          const v = api.getAttr(node._id, "data-" + camelToDash(key));
          return v === null ? undefined : v;
        },
        set(_t, key, value) {
          api.setAttr(node._id, "data-" + camelToDash(String(key)), String(value));
          return true;
        },
        has(_t, key) {
          return api.getAttr(node._id, "data-" + camelToDash(String(key))) !== null;
        },
      });
    }

    querySelector(sel) { return wrap(api.query(String(sel), this._id)); }
    querySelectorAll(sel) { return api.queryAll(String(sel), this._id).map(wrap); }

    matches(sel) {
      // Asked of the document and filtered, because the selector engine
      // underneath searches from the root. Correct, if not the cheapest route.
      const id = this._id;
      return api.queryAll(String(sel), 0).some((m) => m === id);
    }
    closest(sel) {
      for (let n = this; n; n = n.parentNode) {
        if (n.nodeType === 1 && n.matches(sel)) return n;
      }
      return null;
    }

    insertAdjacentHTML(position, html) {
      const where = String(position).toLowerCase();
      const host = document.createElement("div");
      api.setInnerHtml(host._id, String(html));
      const kids = host.childNodes;
      if (where === "beforeend") { for (const k of kids) this.appendChild(k); }
      else if (where === "afterbegin") {
        const first = this.firstChild;
        for (const k of kids) first ? this.insertBefore(k, first) : this.appendChild(k);
      } else if (where === "beforebegin") {
        for (const k of kids) this.parentNode.insertBefore(k, this);
      } else if (where === "afterend") {
        const parent = this.parentNode;
        const next = this.nextSibling;
        for (const k of kids) next ? parent.insertBefore(k, next) : parent.appendChild(k);
      } else {
        throw new TypeError("bad insertAdjacentHTML position: " + position);
      }
      host.remove();
    }

    click() { dispatch(this, new MouseEvent("click", { bubbles: true })); }
    focus() {}
    blur() {}

    // Answered from the layout the engine already computed. Returning zeros —
    // which is what this did before — sends a positioning library into a loop
    // that never converges.
    getBoundingClientRect() {
      const r = api.rect(this._id) || [0, 0, 0, 0];
      const [x, y, width, height] = r;
      return {
        x, y, width, height,
        top: y, left: x, right: x + width, bottom: y + height,
        toJSON() { return { x, y, width, height, top: y, left: x, right: x + width, bottom: y + height }; },
      };
    }
    getClientRects() { return [this.getBoundingClientRect()]; }
    get offsetWidth() { return this.getBoundingClientRect().width; }
    get offsetHeight() { return this.getBoundingClientRect().height; }
    get clientWidth() { return this.getBoundingClientRect().width; }
    get clientHeight() { return this.getBoundingClientRect().height; }
  }

  function camelToDash(name) {
    return name.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
  }

  // Inline style, backed by the element's own `style` attribute rather than by
  // a parallel object, so what script sets is what the cascade sees and what a
  // later `getAttribute("style")` returns. One source of truth, same rule the
  // DOM follows.
  class StyleDeclaration {
    constructor(node) { this._node = node; }

    _read() {
      const raw = api.getAttr(this._node._id, "style") || "";
      const out = new Map();
      for (const part of raw.split(";")) {
        const at = part.indexOf(":");
        if (at < 0) continue;
        const name = part.slice(0, at).trim().toLowerCase();
        const value = part.slice(at + 1).trim();
        if (name) out.set(name, value);
      }
      return out;
    }
    _write(map) {
      const text = [...map.entries()].map(([k, v]) => `${k}: ${v}`).join("; ");
      if (text) api.setAttr(this._node._id, "style", text);
      else api.removeAttr(this._node._id, "style");
    }

    getPropertyValue(name) { return this._read().get(String(name).toLowerCase()) || ""; }
    setProperty(name, value) {
      const map = this._read();
      if (value === "" || value === null || value === undefined) {
        map.delete(String(name).toLowerCase());
      } else {
        map.set(String(name).toLowerCase(), String(value));
      }
      this._write(map);
    }
    removeProperty(name) {
      const map = this._read();
      const had = map.get(String(name).toLowerCase()) || "";
      map.delete(String(name).toLowerCase());
      this._write(map);
      return had;
    }
    get cssText() { return api.getAttr(this._node._id, "style") || ""; }
    set cssText(text) { api.setAttr(this._node._id, "style", String(text)); }
  }

  // `el.style.backgroundColor = 'red'` has to reach `background-color`, so the
  // camelCase surface is a proxy over the dashed one rather than a second list
  // that could disagree with it.
  const styleHandler = {
    get(target, key) {
      if (typeof key !== "string" || key in target) return Reflect.get(target, key);
      return target.getPropertyValue(camelToDash(key));
    },
    set(target, key, value) {
      if (typeof key === "string" && !(key in target)) {
        target.setProperty(camelToDash(key), value);
        return true;
      }
      return Reflect.set(target, key, value);
    },
  };
  const RawStyleDeclaration = StyleDeclaration;
  StyleDeclaration = function (node) {
    return new Proxy(new RawStyleDeclaration(node), styleHandler);
  };

  // ── events ───────────────────────────────────────────────────────────────

  const listeners = [];

  class Event {
    constructor(type, init) {
      this.type = String(type);
      this.bubbles = !!(init && init.bubbles);
      this.cancelable = !!(init && init.cancelable);
      this.composed = !!(init && init.composed);
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = 0;
      this.timeStamp = clock;
      this.isTrusted = false;
      this._stopped = false;
    }
    preventDefault() { if (this.cancelable !== false) this.defaultPrevented = true; }
    stopPropagation() { this._stopped = true; }
    stopImmediatePropagation() { this._stopped = true; }
    composedPath() { return path(this.target || null); }
  }

  // The concrete types a page actually constructs and reads fields off. A
  // single generic Event meant `event.detail` and `event.key` were undefined,
  // which is the kind of gap a framework notices immediately and silently.
  class CustomEvent extends Event {
    constructor(type, init) { super(type, init); this.detail = (init && init.detail) ?? null; }
  }
  class UIEvent extends Event {
    constructor(type, init) { super(type, init); this.detail = (init && init.detail) || 0; }
  }
  class MouseEvent extends UIEvent {
    constructor(type, init) {
      super(type, init);
      const i = init || {};
      this.clientX = i.clientX || 0; this.clientY = i.clientY || 0;
      this.screenX = i.screenX || 0; this.screenY = i.screenY || 0;
      this.pageX = i.pageX || this.clientX; this.pageY = i.pageY || this.clientY;
      this.button = i.button || 0; this.buttons = i.buttons || 0;
      this.altKey = !!i.altKey; this.ctrlKey = !!i.ctrlKey;
      this.shiftKey = !!i.shiftKey; this.metaKey = !!i.metaKey;
    }
  }
  class KeyboardEvent extends UIEvent {
    constructor(type, init) {
      super(type, init);
      const i = init || {};
      this.key = i.key || ""; this.code = i.code || "";
      this.repeat = !!i.repeat; this.isComposing = !!i.isComposing;
      this.altKey = !!i.altKey; this.ctrlKey = !!i.ctrlKey;
      this.shiftKey = !!i.shiftKey; this.metaKey = !!i.metaKey;
    }
  }
  class InputEvent extends UIEvent {
    constructor(type, init) {
      super(type, init);
      const i = init || {};
      this.data = i.data ?? null; this.inputType = i.inputType || "insertText";
    }
  }

  function path(node) {
    const chain = [];
    for (let n = node; n; n = n.parentNode) chain.push(n);
    return chain;
  }

  // Capture down, then bubble up: the order a page's handlers were written
  // against. A listener that throws does not stop the others, because one bad
  // handler taking the page down is worse than one handler not running.
  function dispatch(target, event) {
    event.target = target;
    const chain = path(target);

    const fire = (node, capture) => {
      if (event._stopped) return;
      event.currentTarget = node;
      for (const l of listeners.slice()) {
        if (l.id !== node._id || l.type !== event.type || l.capture !== capture) continue;
        try {
          if (typeof l.handler === "function") l.handler.call(node, event);
          else if (l.handler && typeof l.handler.handleEvent === "function") {
            l.handler.handleEvent(event);
          }
        } catch (error) {
          console.error("listener for " + event.type + " threw: " + error);
        }
      }
    };

    for (let i = chain.length - 1; i >= 1; i--) fire(chain[i], true);
    fire(target, true);
    fire(target, false);
    if (event.bubbles) for (let i = 1; i < chain.length; i++) fire(chain[i], false);

    return !event.defaultPrevented;
  }

  // ── document and window ──────────────────────────────────────────────────

  const document = {
    get documentElement() { return wrap(api.root()); },
    get body() { return wrap(api.body()); },
    get head() { return wrap(api.query("head", 0)); },
    createElement(tag) { return wrap(api.createElement(String(tag))); },
    createTextNode(text) { return wrap(api.createText(String(text))); },
    createDocumentFragment() { return new DocumentFragment(); },
    querySelector(sel) { return wrap(api.query(String(sel), 0)); },
    querySelectorAll(sel) { return api.queryAll(String(sel), 0).map(wrap); },
    getElementById(id) { return wrap(api.query("#" + String(id), 0)); },
    getElementsByTagName(tag) { return api.queryAll(String(tag), 0).map(wrap); },
    getElementsByClassName(cls) { return api.queryAll("." + String(cls), 0).map(wrap); },
    addEventListener(type, handler, options) {
      const root = wrap(api.root());
      if (root) root.addEventListener(type, handler, options);
    },
    removeEventListener(type, handler) {
      const root = wrap(api.root());
      if (root) root.removeEventListener(type, handler);
    },
    get cookie() { api.unsupported("document.cookie"); return ""; },
    set cookie(_v) { api.unsupported("document.cookie"); },
    get readyState() { return "complete"; },
  };

  const console = {
    log: (...a) => api.log("log", a.map(render).join(" ")),
    info: (...a) => api.log("info", a.map(render).join(" ")),
    warn: (...a) => api.log("warn", a.map(render).join(" ")),
    error: (...a) => api.log("error", a.map(render).join(" ")),
    debug: (...a) => api.log("debug", a.map(render).join(" ")),
  };

  function render(v) {
    if (typeof v === "string") return v;
    try { return JSON.stringify(v) ?? String(v); } catch (_) { return String(v); }
  }

  // ── timers ───────────────────────────────────────────────────────────────
  //
  // The queue lives here and the host drains it, so "has this page settled"
  // is a question with an answer rather than a guess about wall-clock time.

  let nextTimer = 1;
  const timers = new Map();
  let clock = 0;

  function setTimeout(fn, delay, ...args) {
    const id = nextTimer++;
    timers.set(id, { fn, due: clock + Math.max(0, delay | 0), args });
    return id;
  }
  function clearTimeout(id) { timers.delete(id); }

  // Returns the number of callbacks run. The host calls this until it returns
  // zero, advancing its own clock, which is what makes a timer chain settle
  // deterministically instead of racing a real one.
  globalThis.__h5iRunTimers = function (now) {
    clock = now;
    let ran = 0;
    for (const [id, timer] of [...timers.entries()].sort((a, b) => a[1].due - b[1].due)) {
      if (timer.due > clock) continue;
      timers.delete(id);
      try { timer.fn(...timer.args); } catch (error) {
        console.error("timer threw: " + error);
      }
      ran++;
    }
    return ran;
  };
  globalThis.__h5iPendingTimers = function () { return timers.size; };

  // How the host reaches a node it knows only by id, to fire a real event at
  // it. Exposed rather than reimplemented on the Rust side so a synthetic
  // click takes exactly the path a page's own `.click()` takes.
  globalThis.__h5iWrapById = wrap;

  // ── the rest of the window ───────────────────────────────────────────────

  const location = {
    get href() { return globalThis.__h5iUrl; },
    get protocol() { return String(globalThis.__h5iUrl).split(":")[0] + ":"; },
    toString() { return globalThis.__h5iUrl; },
    assign(u) { api.unsupported("location.assign"); void u; },
    replace(u) { api.unsupported("location.replace"); void u; },
    reload() { api.unsupported("location.reload"); },
  };

  // Client-side routing goes through this, so a stub meant an SPA changed
  // nothing when it navigated. In memory, current entry plus a short list: the
  // page's own router reads `state` and listens for `popstate`, and both work.
  const entries = [{ state: null, url: globalThis.__h5iUrl }];
  let entryAt = 0;
  const history = {
    get length() { return entries.length; },
    get state() { return entries[entryAt].state ?? null; },
    pushState(state, _title, url) {
      entries.length = entryAt + 1;
      entries.push({ state: state ?? null, url: url ? String(url) : entries[entryAt].url });
      entryAt = entries.length - 1;
    },
    replaceState(state, _title, url) {
      entries[entryAt] = { state: state ?? null, url: url ? String(url) : entries[entryAt].url };
    },
    go(delta) {
      const next = entryAt + (delta | 0);
      if (next < 0 || next >= entries.length) return;
      entryAt = next;
      const event = new Event("popstate", { bubbles: false });
      event.state = entries[entryAt].state;
      dispatch(wrap(api.root()), event);
    },
    back() { history.go(-1); },
    forward() { history.go(1); },
  };

  const performance = { now: () => clock };

  // Window-level listeners land on the root element, which is where document
  // and window events already propagate to. Without these, `addEventListener`
  // at global scope is simply undefined, and popstate/DOMContentLoaded
  // handlers — which most routers install — throw on the way in.
  function addEventListener(type, handler, options) {
    const root = wrap(api.root());
    if (root) root.addEventListener(type, handler, options);
  }
  function removeEventListener(type, handler) {
    const root = wrap(api.root());
    if (root) root.removeEventListener(type, handler);
  }
  function dispatchEvent(event) {
    const root = wrap(api.root());
    return root ? root.dispatchEvent(event) : true;
  }

  const window = globalThis;
  Object.assign(globalThis, {
    addEventListener, removeEventListener, dispatchEvent,
    window, document, console, location, history, performance,
    setTimeout, clearTimeout,
    setInterval: (fn, d) => { api.unsupported("setInterval"); return setTimeout(fn, d); },
    clearInterval: clearTimeout,
    requestAnimationFrame: (fn) => setTimeout(() => fn(clock), 16),
    cancelAnimationFrame: clearTimeout,
    Node, Element, Text, Event,
    alert: () => api.unsupported("alert"),
    matchMedia: () => { api.unsupported("matchMedia"); return { matches: false, addListener() {}, addEventListener() {} }; },
    getComputedStyle: () => { api.unsupported("getComputedStyle"); return {}; },
    localStorage: makeStorage(),
    sessionStorage: makeStorage(),
    CustomEvent, UIEvent, MouseEvent, KeyboardEvent, InputEvent,
    DocumentFragment,
    MutationObserver: class { constructor() { api.unsupported("MutationObserver"); } observe() {} disconnect() {} },
    IntersectionObserver: class { constructor() { api.unsupported("IntersectionObserver"); } observe() {} disconnect() {} },
    ResizeObserver: class { constructor() { api.unsupported("ResizeObserver"); } observe() {} disconnect() {} },
  });

  // `fetch`, over the host's broker. Every request is policy-checked and
  // receipted before it moves, which is the property this engine exists for.
  function fetch(input, init) {
    const url = typeof input === "string" ? input : String(input && input.url);
    const method = (init && init.method) || "GET";
    let body = (init && init.body) || "";
    if (body && typeof body !== "string") {
      try { body = JSON.stringify(body); } catch (_) { body = String(body); }
    }
    const res = api.fetch(url, method, body);
    if (res.error) return Promise.reject(new Error(res.error));

    const response = {
      ok: res.ok,
      status: res.status,
      url: res.url,
      headers: { get() { api.unsupported("Response.headers"); return null; } },
      text: () => Promise.resolve(res.text),
      json: () => Promise.resolve(JSON.parse(res.text)),
    };
    return Promise.resolve(response);
  }
  globalThis.fetch = fetch;

  // In memory and nowhere else. A disposable box has no business writing a
  // page's storage to a filesystem, and "restart the session" is a complete
  // clear — the same rule the cookie jar follows.
  function makeStorage() {
    const map = new Map();
    return {
      getItem(k) { const v = map.get(String(k)); return v === undefined ? null : v; },
      setItem(k, v) { map.set(String(k), String(v)); },
      removeItem(k) { map.delete(String(k)); },
      clear() { map.clear(); },
      key(i) { return [...map.keys()][i] ?? null; },
      get length() { return map.size; },
    };
  }
})();
