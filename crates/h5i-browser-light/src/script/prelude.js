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
    set textContent(value) {
      api.setText(this._id, String(value));
      record({
        type: "characterData", target: this, addedNodes: [], removedNodes: [],
        attributeName: null, oldValue: null,
      });
      childListRecord(this, [], []);
    }

    appendChild(child) {
      // Inserting a fragment inserts its children and leaves the fragment
      // behind, which is the whole reason a fragment exists.
      if (child instanceof DocumentFragment) {
        const moved = child.childNodes;
        for (const kid of moved) api.append(this._id, kid._id);
        child._children.length = 0;
        childListRecord(this, moved, []);
        return child;
      }
      api.append(this._id, child._id);
      childListRecord(this, [child], []);
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
      childListRecord(this, [child], []);
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
    removeChild(child) {
      api.removeNode(child._id);
      childListRecord(this, [], [child]);
      return child;
    }
    remove() {
      const parent = this.parentNode;
      api.removeNode(this._id);
      if (parent) childListRecord(parent, [], [this]);
    }
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
    setAttribute(name, value) {
      const previous = api.getAttr(this._id, String(name));
      api.setAttr(this._id, String(name), String(value));
      record({
        type: "attributes", target: this, addedNodes: [], removedNodes: [],
        attributeName: String(name).toLowerCase(), oldValue: previous,
      });
    }
    removeAttribute(name) {
      const previous = api.getAttr(this._id, String(name));
      api.removeAttr(this._id, String(name));
      record({
        type: "attributes", target: this, addedNodes: [], removedNodes: [],
        attributeName: String(name).toLowerCase(), oldValue: previous,
      });
    }
    hasAttribute(name) { return api.getAttr(this._id, String(name)) !== null; }

    get id() { return this.getAttribute("id") || ""; }
    set id(v) { this.setAttribute("id", v); }
    get className() { return this.getAttribute("class") || ""; }
    set className(v) { this.setAttribute("class", v); }
    get classList() { return new ClassList(this); }

    get value() {
      const tag = this.tagName;
      if (tag === "SELECT") {
        const chosen = this.querySelectorAll("option").find((o) => o.selected);
        return chosen ? chosen.value : "";
      }
      if (tag === "OPTION") {
        const explicit = api.getAttr(this._id, "value");
        return explicit === null ? this.textContent : explicit;
      }
      const kind = (api.getAttr(this._id, "type") || "").toLowerCase();
      if (kind === "checkbox" || kind === "radio") {
        const v = api.getAttr(this._id, "value");
        return v === null ? "on" : v;
      }
      return api.getValue(this._id);
    }
    set value(v) {
      api.setValue(this._id, String(v));
      // A page that sets `.value` from script does not get input/change: the
      // spec fires those for *user* edits, and a framework that re-rendered on
      // its own write would loop. `Page::type_into` is the user path.
    }

    get checked() { return api.getAttr(this._id, "checked") !== null; }
    set checked(on) {
      if (on) api.setAttr(this._id, "checked", "");
      else api.removeAttr(this._id, "checked");
    }
    get selected() { return api.getAttr(this._id, "selected") !== null; }
    set selected(on) {
      if (on) api.setAttr(this._id, "selected", "");
      else api.removeAttr(this._id, "selected");
    }
    get disabled() { return api.getAttr(this._id, "disabled") !== null; }
    get name() { return api.getAttr(this._id, "name") || ""; }
    get type() { return (api.getAttr(this._id, "type") || "text").toLowerCase(); }
    get options() { return this.querySelectorAll("option"); }

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

    click() {
      // A real click on a checkbox toggles it *and* fires input then change,
      // in that order. A page that only listens for `change` — which is most
      // of them — sees nothing without this.
      const kind = this.type;
      if (this.tagName === "INPUT" && (kind === "checkbox" || kind === "radio")) {
        if (kind === "radio") {
          const name = this.name;
          if (name) {
            for (const other of document.querySelectorAll(`input[type=radio][name="${name}"]`)) {
              other.checked = false;
            }
          }
          this.checked = true;
        } else {
          this.checked = !this.checked;
        }
        dispatch(this, new MouseEvent("click", { bubbles: true }));
        dispatch(this, new InputEvent("input", { bubbles: true }));
        dispatch(this, new Event("change", { bubbles: true }));
        return;
      }
      dispatch(this, new MouseEvent("click", { bubbles: true }));
    }
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

  // An API this engine does not implement, made to say so.
  //
  // The corpus run found the gap this closes: a global we never defined throws
  // a bare `ReferenceError`, and a method on a half-defined object throws
  // `TypeError: not a callable function`. Neither names the API, so neither
  // reaches the unsupported list an agent reads — the page just looks broken.
  // These record themselves on *any* access and throw a message that names what
  // was wanted, so a missing API is legible from both directions.
  function missingApi(name) {
    const report = (property) => {
      const full = property ? `${name}.${String(property)}` : name;
      api.unsupported(full);
      throw new TypeError(`${full} is not implemented by this engine`);
    };
    return new Proxy(function () {}, {
      get(_t, property) {
        if (property === Symbol.toPrimitive || property === "toString") {
          return () => `[unsupported ${name}]`;
        }
        if (property === "then") return undefined; // not a thenable
        report(property);
      },
      apply() { report(null); },
      construct() { report(null); },
    });
  }

  // Real, because the engine already contains a correct URL parser and a
  // second one written in JavaScript would disagree with it about exactly the
  // cases that matter.
  class URLSearchParams {
    constructor(init) {
      this._pairs = [];
      if (typeof init === "string") {
        for (const part of init.replace(/^\?/, "").split("&")) {
          if (!part) continue;
          const at = part.indexOf("=");
          const k = at < 0 ? part : part.slice(0, at);
          const v = at < 0 ? "" : part.slice(at + 1);
          this._pairs.push([decodeURIComponent(k.replace(/\+/g, " ")),
                            decodeURIComponent(v.replace(/\+/g, " "))]);
        }
      } else if (init && typeof init === "object") {
        for (const k of Object.keys(init)) this._pairs.push([k, String(init[k])]);
      }
    }
    get(k) { const hit = this._pairs.find(([n]) => n === String(k)); return hit ? hit[1] : null; }
    getAll(k) { return this._pairs.filter(([n]) => n === String(k)).map(([, v]) => v); }
    has(k) { return this._pairs.some(([n]) => n === String(k)); }
    set(k, v) { this.delete(k); this.append(k, v); }
    append(k, v) { this._pairs.push([String(k), String(v)]); }
    delete(k) { this._pairs = this._pairs.filter(([n]) => n !== String(k)); }
    forEach(fn) { for (const [k, v] of this._pairs) fn(v, k, this); }
    keys() { return this._pairs.map(([k]) => k)[Symbol.iterator](); }
    values() { return this._pairs.map(([, v]) => v)[Symbol.iterator](); }
    entries() { return this._pairs[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() {
      return this._pairs
        .map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v))
        .join("&");
    }
  }

  class URL {
    constructor(href, base) {
      const parts = api.parseUrl(String(href), base === undefined ? "" : String(base));
      if (!parts) throw new TypeError(`Invalid URL: ${href}`);
      Object.assign(this, parts);
      this.searchParams = new URLSearchParams(parts.search);
    }
    toString() { return this.href; }
    toJSON() { return this.href; }
  }

  // A case-insensitive header map, which is what `Headers` is: `get("ETag")`
  // must find a header the server spelled `etag`.
  class Headers {
    constructor(init) {
      this._map = new Map();
      if (init instanceof Headers) for (const [k, v] of init._map) this._map.set(k, v);
      else if (Array.isArray(init)) for (const [k, v] of init) this.append(k, v);
      else if (init && typeof init === "object") {
        for (const k of Object.keys(init)) this.set(k, init[k]);
      }
    }
    get(name) { const v = this._map.get(String(name).toLowerCase()); return v === undefined ? null : v; }
    set(name, value) { this._map.set(String(name).toLowerCase(), String(value)); }
    has(name) { return this._map.has(String(name).toLowerCase()); }
    delete(name) { this._map.delete(String(name).toLowerCase()); }
    append(name, value) {
      const key = String(name).toLowerCase();
      const existing = this._map.get(key);
      this._map.set(key, existing === undefined ? String(value) : existing + ", " + value);
    }
    forEach(fn) { for (const [k, v] of this._map) fn(v, k, this); }
    keys() { return this._map.keys(); }
    values() { return this._map.values(); }
    entries() { return this._map.entries(); }
    [Symbol.iterator]() { return this._map.entries(); }
  }

  class Request {
    constructor(input, init) {
      this.url = typeof input === "string" ? input : String(input && input.url);
      const i = init || {};
      this.method = (i.method || "GET").toUpperCase();
      this.headers = new Headers(i.headers);
      this.body = i.body ?? null;
      this.signal = i.signal || null;
    }
  }

  // Real enough to be useful: a fetch already aborted is refused, and abort
  // fires its listeners. It cannot cancel a request in flight, because this
  // engine's fetch is synchronous underneath — that limit is stated rather
  // than papered over with a promise that never settles.
  class AbortSignal {
    constructor() { this.aborted = false; this.reason = undefined; this._listeners = []; }
    addEventListener(type, handler) { if (type === "abort") this._listeners.push(handler); }
    removeEventListener(type, handler) {
      if (type !== "abort") return;
      const at = this._listeners.indexOf(handler);
      if (at >= 0) this._listeners.splice(at, 1);
    }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    static abort(reason) {
      const s = new AbortSignal();
      s.aborted = true;
      s.reason = reason ?? new Error("aborted");
      return s;
    }
  }

  class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(reason) {
      if (this.signal.aborted) return;
      this.signal.aborted = true;
      this.signal.reason = reason ?? new Error("aborted");
      const event = new Event("abort", { bubbles: false });
      for (const handler of this.signal._listeners.slice()) {
        try { handler.call(this.signal, event); } catch (e) { console.error("abort listener threw: " + e); }
      }
      if (typeof this.signal.onabort === "function") this.signal.onabort(event);
    }
  }

  class FormData {
    constructor(form) {
      this._entries = [];
      if (form) {
        for (const field of form.querySelectorAll("input, select, textarea")) {
          const name = field.name;
          if (!name || field.disabled) continue;
          const kind = field.type;
          if ((kind === "checkbox" || kind === "radio") && !field.checked) continue;
          if (kind === "submit" || kind === "button" || kind === "file") continue;
          this._entries.push([name, field.value]);
        }
      }
    }
    append(k, v) { this._entries.push([String(k), String(v)]); }
    set(k, v) {
      this.delete(k);
      this.append(k, v);
    }
    get(k) { const hit = this._entries.find(([n]) => n === String(k)); return hit ? hit[1] : null; }
    getAll(k) { return this._entries.filter(([n]) => n === String(k)).map(([, v]) => v); }
    has(k) { return this._entries.some(([n]) => n === String(k)); }
    delete(k) { this._entries = this._entries.filter(([n]) => n !== String(k)); }
    entries() { return this._entries[Symbol.iterator](); }
    keys() { return this._entries.map(([n]) => n)[Symbol.iterator](); }
    values() { return this._entries.map(([, v]) => v)[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() {
      return this._entries
        .map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v))
        .join("&");
    }
  }

  // ── mutation observation ─────────────────────────────────────────────────
  //
  // Records are produced by the mutating methods above rather than by polling
  // the tree, because those methods are the only way script can change it. That
  // is also the honest limit: a change made by the *parser* (an external script
  // arriving, say) is not observed, so this reports what script did rather than
  // everything that happened. Callbacks are delivered as a microtask, which is
  // when a real browser delivers them and what lets a framework batch.

  const observers = [];
  let deliveryQueued = false;

  class MutationObserver {
    constructor(callback) { this._callback = callback; this._records = []; this._targets = []; }
    observe(target, options) {
      this._targets.push({ target, options: options || { childList: true } });
      if (!observers.includes(this)) observers.push(this);
    }
    disconnect() {
      this._targets.length = 0;
      const at = observers.indexOf(this);
      if (at >= 0) observers.splice(at, 1);
    }
    takeRecords() { const r = this._records; this._records = []; return r; }
  }

  function observes(entry, record) {
    const { target, options } = entry;
    const inScope = options.subtree
      ? target.contains(record.target)
      : target._id === record.target._id;
    if (!inScope) return false;
    if (record.type === "childList") return !!options.childList;
    if (record.type === "attributes") {
      if (!options.attributes) return false;
      const filter = options.attributeFilter;
      return !filter || filter.includes(record.attributeName);
    }
    if (record.type === "characterData") return !!options.characterData;
    return false;
  }

  function record(mutation) {
    if (observers.length === 0) return;
    let queued = false;
    for (const observer of observers) {
      if (observer._targets.some((entry) => observes(entry, mutation))) {
        observer._records.push(mutation);
        queued = true;
      }
    }
    if (queued && !deliveryQueued) {
      deliveryQueued = true;
      Promise.resolve().then(deliver);
    }
  }

  function deliver() {
    deliveryQueued = false;
    for (const observer of observers.slice()) {
      const records = observer.takeRecords();
      if (records.length === 0) continue;
      try {
        observer._callback(records, observer);
      } catch (error) {
        console.error("MutationObserver callback threw: " + error);
      }
    }
  }

  function childListRecord(target, added, removed) {
    record({
      type: "childList",
      target,
      addedNodes: added || [],
      removedNodes: removed || [],
      attributeName: null,
      oldValue: null,
    });
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
    matchMedia: (query) => {
      // Answered rather than thrown, because a page that cannot ask about the
      // viewport usually stops rendering entirely. `matches: false` is honest
      // for a fixed-size headless viewport, and the call is still recorded.
      api.unsupported("matchMedia");
      return {
        media: String(query || ""), matches: false, onchange: null,
        addListener() {}, removeListener() {},
        addEventListener() {}, removeEventListener() {}, dispatchEvent() { return false; },
      };
    },
    URL, URLSearchParams,
    queueMicrotask: (fn) => { Promise.resolve().then(fn); },
    structuredClone: (value) => JSON.parse(JSON.stringify(value)),
    requestIdleCallback: (fn) => setTimeout(() => fn({ didTimeout: false, timeRemaining: () => 0 }), 1),
    cancelIdleCallback: clearTimeout,
    navigator: {
      userAgent: "Mozilla/5.0 (compatible; h5i-browser-light)",
      platform: "", language: "en-US", languages: ["en-US"],
      onLine: true, cookieEnabled: false, maxTouchPoints: 0,
      clipboard: missingApi("navigator.clipboard"),
      serviceWorker: missingApi("navigator.serviceWorker"),
      geolocation: missingApi("navigator.geolocation"),
      mediaDevices: missingApi("navigator.mediaDevices"),
    },
    // Named rather than absent. A page reaching for these gets a message that
    // says which API it wanted, and the name reaches the snapshot.
    customElements: missingApi("customElements"),
    WebSocket: missingApi("WebSocket"),
    Worker: missingApi("Worker"),
    SharedWorker: missingApi("SharedWorker"),
    XMLHttpRequest: missingApi("XMLHttpRequest"),
    EventSource: missingApi("EventSource"),
    indexedDB: missingApi("indexedDB"),
    caches: missingApi("caches"),
    crypto: missingApi("crypto"),
    WebAssembly: missingApi("WebAssembly"),
    BroadcastChannel: missingApi("BroadcastChannel"),
    Notification: missingApi("Notification"),
    FileReader: missingApi("FileReader"),
    Blob: missingApi("Blob"),
    Image: missingApi("Image"),
    TextEncoder: missingApi("TextEncoder"),
    TextDecoder: missingApi("TextDecoder"),
    HTMLCanvasElement: missingApi("HTMLCanvasElement"),
    getComputedStyle: (element) => {
      // Reads what Stylo resolved. Properties outside the curated set record
      // themselves as unsupported rather than returning a plausible lie: a
      // wrong `display` sends a framework down a branch a real browser never
      // would, and it would never find out.
      if (!element || element._id === undefined) return { getPropertyValue: () => "" };
      const read = (name) => api.computedStyle(element._id, String(name)) || "";
      return new Proxy(
        { getPropertyValue: read },
        {
          get(target, key) {
            if (typeof key !== "string" || key in target) return Reflect.get(target, key);
            return read(camelToDash(key));
          },
        }
      );
    },
    localStorage: makeStorage(),
    sessionStorage: makeStorage(),
    CustomEvent, UIEvent, MouseEvent, KeyboardEvent, InputEvent,
    DocumentFragment, Headers, Request, AbortController, AbortSignal, FormData,
    MutationObserver,
    IntersectionObserver: class { constructor() { api.unsupported("IntersectionObserver"); } observe() {} disconnect() {} },
    ResizeObserver: class { constructor() { api.unsupported("ResizeObserver"); } observe() {} disconnect() {} },
  });

  // `fetch`, over the host's broker. Every request is policy-checked and
  // receipted before it moves, which is the property this engine exists for.
  function fetch(input, init) {
    const request = input instanceof Request ? input : new Request(input, init);
    const signal = (init && init.signal) || request.signal;
    if (signal && signal.aborted) {
      return Promise.reject(signal.reason ?? new Error("aborted"));
    }

    let body = request.body ?? "";
    if (body instanceof FormData) body = body.toString();
    else if (body && typeof body !== "string") {
      try { body = JSON.stringify(body); } catch (_) { body = String(body); }
    }

    const res = api.fetch(request.url, request.method, body);
    if (res.error) return Promise.reject(new Error(res.error));

    const headers = new Headers();
    for (const [name, value] of res.headers || []) headers.append(name, value);

    const response = {
      ok: res.ok,
      status: res.status,
      statusText: res.status === 200 ? "OK" : "",
      url: res.url,
      redirected: res.url !== request.url,
      headers,
      text: () => Promise.resolve(res.text),
      json: () => Promise.resolve(JSON.parse(res.text)),
      clone() { return { ...response }; },
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
