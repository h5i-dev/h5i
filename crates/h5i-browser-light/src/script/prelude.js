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

    appendChild(child) { api.append(this._id, child._id); return child; }
    insertBefore(child, anchor) {
      if (!anchor) return this.appendChild(child);
      api.insertBefore(anchor._id, child._id);
      return child;
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

    get innerHTML() { return api.getText(this._id); }
    set innerHTML(html) { api.setInnerHtml(this._id, String(html)); }

    querySelector(sel) { return wrap(api.query(String(sel), this._id)); }
    querySelectorAll(sel) { return api.queryAll(String(sel), this._id).map(wrap); }

    click() { dispatch(this, new Event("click", { bubbles: true })); }
    focus() {}
    blur() {}

    // Layout reads are the commonest thing a real app asks for that this
    // engine does not answer yet. Reported rather than faked: zeros would send
    // a positioning library into a loop that never converges.
    getBoundingClientRect() {
      api.unsupported("Element.getBoundingClientRect");
      return { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
    }
  }

  // ── events ───────────────────────────────────────────────────────────────

  const listeners = [];

  class Event {
    constructor(type, init) {
      this.type = String(type);
      this.bubbles = !!(init && init.bubbles);
      this.cancelable = !!(init && init.cancelable);
      this.defaultPrevented = false;
      this.target = null;
      this.currentTarget = null;
      this._stopped = false;
    }
    preventDefault() { this.defaultPrevented = true; }
    stopPropagation() { this._stopped = true; }
    stopImmediatePropagation() { this._stopped = true; }
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
    createDocumentFragment() {
      api.unsupported("document.createDocumentFragment");
      return wrap(api.createElement("div"));
    },
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

  const history = {
    length: 1,
    pushState() { api.unsupported("history.pushState"); },
    replaceState() { api.unsupported("history.replaceState"); },
    back() { api.unsupported("history.back"); },
    forward() { api.unsupported("history.forward"); },
  };

  const performance = { now: () => clock };

  const window = globalThis;
  Object.assign(globalThis, {
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
    localStorage: makeUnsupportedStorage("localStorage"),
    sessionStorage: makeUnsupportedStorage("sessionStorage"),
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

  function makeUnsupportedStorage(name) {
    return {
      getItem() { api.unsupported(name); return null; },
      setItem() { api.unsupported(name); },
      removeItem() { api.unsupported(name); },
      clear() { api.unsupported(name); },
    };
  }
})();
