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

  /// A live-enough `NodeList`/`HTMLCollection`.
  ///
  /// An array, because everything in this engine already treats one as a list
  /// and frameworks reach for `map` and `filter` on the result — but with the
  /// two methods a real collection has and an array does not. `list.item(0)`
  /// was `undefined`, and calling it is "not a callable function", which is
  /// exactly the error fourteen module failures reported and nothing named.
  ///
  /// Deliberately *not* watched, and this one was measured rather than argued.
  /// Wrapping the list in the reporting proxy cost 3.9x on iteration — 674us
  /// against 174us for a 400-node result — because every index read goes
  /// through a trap, and `for (const el of query)` is the hottest line in DOM
  /// code. An array already answers everything a `NodeList` does except `item`
  /// and `namedItem`, which are right here, so the naming it bought was small
  /// and the price was not.
  function collection(nodes, label) {
    void label;
    const list = nodes.slice();
    list.item = (index) => list[index] ?? null;
    list.namedItem = (name) =>
      list.find((n) => n.id === String(name) || n.getAttribute?.("name") === String(name)) ?? null;
    return list;
  }

  /// Take a node out of whatever parent it is in, before putting it somewhere.
  ///
  /// The DOM defines insertion as removing the node from its old parent first,
  /// and this engine was not doing it: the tree underneath drops a node
  /// inserted while still parented, so *moving* a node deleted it. Three
  /// appends followed by two moves left one child of three.
  ///
  /// That is the operation a keyed diff is built out of. It is why preactjs.com
  /// rendered its shell and its sidebar and then nothing where the article
  /// should be: preact reorders by re-inserting nodes it already has, and every
  /// reorder threw one away.
  ///
  /// Deliberately the raw detach rather than `removeChild`: a move is one
  /// operation, and firing a disconnect for the half of it that is a removal
  /// would tell a custom element it had left a document it is still in.
  function detachFromParent(node) {
    if (!node || node._id === undefined || node._id === null) return;
    if (api.parent(node._id) === null || api.parent(node._id) === undefined) return;
    api.removeNode(node._id);
  }

  /// The id of the document node, worked out once. It is the parent of the
  /// root element and never changes.
  let knownDocumentNode;
  function documentNodeId() {
    if (knownDocumentNode === undefined) knownDocumentNode = api.parent(api.root());
    return knownDocumentNode;
  }

  function wrap(id) {
    if (id === null || id === undefined) return null;
    let existing = wrappers.get(id);
    if (existing) return existing;
    // Re-entrant construction — a custom element's constructor asking the
    // document for itself — gets a plain wrapper rather than recursing forever.
    if (constructing.has(id)) return observed(new Element(id), "Element");


    // The tree decides what a node is. A set of ids on this side only knew
    // about comments script had made, so every comment the *parser* produced
    // was wrapped as a text node.
    let raw;
    let label;
    const kind = api.nodeKind(id);
    if (kind === 8) { raw = new Comment(id); label = "Comment"; }
    else if (kind === 1) { raw = constructElement(id); label = "Element"; }
    else { raw = new Text(id); label = "Text"; }

    // Labelled by what the node actually is. Calling a text node "Element"
    // reported `Element.tagName` as missing when what happened was a page
    // reading `tagName` off a text node, where no engine has one.
    raw._kind = kind;
    const node = observed(raw, label);
    wrappers.set(id, node);
    return node;
  }

  // ── custom elements ──────────────────────────────────────────────────────
  //
  // The corpus asked for `customElements.define` once it could get that far,
  // which happened only after `HTMLElement` existed for `class X extends
  // HTMLElement` to name. Defining without upgrading would be the worse kind of
  // half-support: the page would register its components, see no error, and
  // render nothing.

  const definitions = new Map();
  const constructing = new Set();
  // Which custom elements have had `connectedCallback` run. Kept beside the
  // nodes rather than on them: a flag stored as a property is a property, and
  // the reporting proxy rightly named our own bookkeeping as a missing API the
  // first time a page's code reached a node before we had set it.
  const connected = new Set();
  const comments = new Set();
  let upgrading = null;

  function constructElement(id) {
    const definition = definitions.get(api.tagName(id).toLowerCase());
    if (!definition) return new Element(id);

    const previousUpgrade = upgrading;
    upgrading = id;
    constructing.add(id);
    try {
      return new definition.ctor();
    } catch (error) {
      // A component whose constructor throws must not take the page with it.
      console.error(`custom element <${definition.name}> threw while upgrading: ${error}`);
      return new Element(id);
    } finally {
      upgrading = previousUpgrade;
      constructing.delete(id);
    }
  }

  function isCustom(node) {
    return node && node.nodeType === 1 && definitions.has(api.tagName(node._id).toLowerCase());
  }

  /// Every custom element at or under `node`, in document order.
  function collectCustom(node) {
    const found = [];
    if (definitions.size === 0) return found;
    const visit = (n) => {
      if (!n || n.nodeType !== 1) return;
      if (isCustom(n)) found.push(n);
      for (const kid of n.children) visit(kid);
    };
    visit(node);
    return found;
  }

  function notifyConnection(node) {
    // Nothing to notify if nothing is defined, and most pages define nothing.
    // Without this every insertion walked to the root and then over the whole
    // inserted subtree to find custom elements that could not exist — which
    // made attaching a node cost three times what building one detached does.
    if (definitions.size === 0) return;
    if (!node || node.nodeType !== 1 || !node.isConnected) return;
    for (const found of collectCustom(node)) fireConnected(found);
  }

  function fireConnected(node) {
    if (connected.has(node._id)) return;
    connected.add(node._id);
    try {
      if (typeof node.connectedCallback === "function") node.connectedCallback();
    } catch (error) {
      console.error(`custom element connectedCallback threw: ${error}`);
    }
  }

  function fireDisconnected(node) {
    if (!connected.has(node._id)) return;
    connected.delete(node._id);
    try {
      if (typeof node.disconnectedCallback === "function") node.disconnectedCallback();
    } catch (error) {
      console.error(`custom element disconnectedCallback threw: ${error}`);
    }
  }

  function fireAttributeChanged(node, name, oldValue, newValue) {
    if (!isCustom(node)) return;
    const observedNames = node.constructor && node.constructor.observedAttributes;
    if (!Array.isArray(observedNames) || !observedNames.includes(name)) return;
    try {
      if (typeof node.attributeChangedCallback === "function") {
        node.attributeChangedCallback(name, oldValue, newValue);
      }
    } catch (error) {
      console.error(`custom element attributeChangedCallback threw: ${error}`);
    }
  }

  const pendingDefinitions = new Map();

  const customElements = {
    define(name, ctor, options) {
      const key = String(name).toLowerCase();
      // The spec's rule, and worth keeping: a name without a dash could collide
      // with an element the parser already knows.
      if (!key.includes("-")) {
        throw new SyntaxError(`custom element name \`${key}\` must contain a dash`);
      }
      if (definitions.has(key)) {
        throw new Error(`custom element \`${key}\` is already defined`);
      }
      if (options && options.extends) {
        api.unsupported("customElements.define({ extends })");
      }
      definitions.set(key, { name: key, ctor });

      // Upgrade what is already on the page. Without this, a page that ships
      // its markup server-side and defines its components afterwards — which is
      // most of them — would define and then do nothing.
      for (const id of api.queryAll(key, 0)) {
        wrappers.delete(id);
        const node = wrap(id);
        // The observed attributes have their initial values delivered, as they
        // are on upgrade in a real engine, or a component that only renders
        // from `attributeChangedCallback` renders blank.
        const observedNames = ctor.observedAttributes;
        if (Array.isArray(observedNames)) {
          for (const attribute of observedNames) {
            const value = api.getAttr(id, attribute);
            if (value !== null) fireAttributeChanged(node, attribute, null, value);
          }
        }
        if (node.isConnected) fireConnected(node);
      }

      const waiting = pendingDefinitions.get(key);
      if (waiting) { pendingDefinitions.delete(key); waiting.forEach((resolve) => resolve(ctor)); }
    },
    get(name) {
      const definition = definitions.get(String(name).toLowerCase());
      return definition ? definition.ctor : undefined;
    },
    getName(ctor) {
      for (const [key, definition] of definitions) {
        if (definition.ctor === ctor) return key;
      }
      return null;
    },
    whenDefined(name) {
      const key = String(name).toLowerCase();
      const definition = definitions.get(key);
      if (definition) return Promise.resolve(definition.ctor);
      return new Promise((resolve) => {
        const waiting = pendingDefinitions.get(key) || [];
        waiting.push(resolve);
        pendingDefinitions.set(key, waiting);
      });
    },
    upgrade(node) {
      for (const found of collectCustom(node)) {
        if (found.isConnected) fireConnected(found);
      }
    },
  };

  /// Every node in the tree, in document order. Used for the two questions that
  /// need it — `compareDocumentPosition` and the traversal objects — and
  /// rebuilt each time, because script mutates the tree between calls.
  function documentOrder() {
    const out = [];
    const visit = (id) => {
      out.push(id);
      for (const kid of api.children(id)) visit(kid);
    };
    visit(api.root());
    return out;
  }

  // Wrap an object we own so that reading a property it does not have is
  // *recorded* rather than silently undefined.
  //
  // The corpus found the gap this closes. `missingApi` names globals, so
  // `WebSocket` reports itself — but a page reading `el.scrollIntoView` or
  // `document.activeElement` got `undefined`, then threw
  // `TypeError: not a callable function` somewhere further along, and nothing
  // anywhere named the property. The measurement could not see what was left,
  // which is a different thing from nothing being left.
  //
  // Only genuinely unknown names are recorded. Anything on the prototype chain
  // is a property we implement, and anything the page itself assigned is an
  // expando it expects to read back — both take the plain path, so a working
  // page records nothing at all.
  function observed(target, label) {
    return new Proxy(target, {
      get(object, property, receiver) {
        if (typeof property === "symbol" || property in object) {
          // The raw target as receiver, not the proxy. A getter invoked with
          // the proxy as `this` pays another trap for every `this._id` it
          // reads, so each of our own accessors cost two — `nodeType` was
          // 2.15 µs for what is a field lookup. Passing the target takes the
          // hot properties to 0.85 µs.
          //
          // What it narrows: a getter *defined by the page* on its own class
          // runs with the target as `this`, so an unknown property read inside
          // one is not reported. Methods are unaffected — `el.method()` still
          // calls with the proxy as `this` — and the reporting that has found
          // real bugs has always been about properties the page reads *off* a
          // node, which is unchanged.
          return Reflect.get(object, property, object);
        }
        // `then` is probed by the promise machinery on anything it is handed;
        // recording it would report a missing API every time a node passed
        // through an await.
        if (property === "then") return undefined;

        // A gap is only a gap if a real browser would have answered. Reading
        // `tagName` off a text node gets undefined in every engine there is —
        // reporting it would send us building something that does not exist,
        // and the corpus did exactly that until this rule was written.
        if (label !== "Element" && property in Element.prototype) return undefined;

        // Nor is a page's own bookkeeping. No web platform property begins with
        // an underscore or a dollar; frameworks' private fields routinely do.
        // Solid reads `document._$DX_DELEGATE` before it sets it, and the list
        // an agent reads carried that as something this engine was missing.
        const name = String(property);
        const first = name.charCodeAt(0);
        if (first === 95 || first === 36) return undefined;

        // Nor is a generated key. jQuery and Sizzle stamp elements with names
        // like `jQuery360062973586668224961` and `sizzle1786301869537`, read
        // before they are written — one page produced 5265 such "gaps" and put
        // them at the top of the list, which is exactly the burying this filter
        // exists to prevent. No web platform property carries a run of digits
        // that long, because it would have to be typed by a person.
        if (/\d{6}/.test(name)) return undefined;

        api.unsupported(`${label}.${String(property)}`);
        return undefined;
      },
    });
  }

  // `class` is the famous one, but `rel` is a token list too, and so are
  // `sandbox` and `headers`. Parameterising the attribute is the difference
  // between one implementation and four.
  class DOMTokenList {
    constructor(node, attribute) { this._node = node; this._attr = attribute; }
    _all() {
      const raw = api.getAttr(this._node._id, this._attr) || "";
      return raw.split(/\s+/).filter(Boolean);
    }
    _write(list) {
      api.setAttr(this._node._id, this._attr, list.join(" "));
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
    item(index) { return this._all()[index] ?? null; }
    // False, and deliberately. `supports` asks whether *this engine* acts on a
    // token — `rel="preload"`, `sandbox="allow-scripts"` — and this one acts on
    // none of them. Answering true would send a page down a path expecting
    // behaviour that will not happen, which is the plausible-wrong answer this
    // engine keeps having to refuse.
    supports() { return false; }
    get length() { return this._all().length; }
    get value() { return this._all().join(" "); }
    set value(v) { api.setAttr(this._node._id, this._attr, String(v)); }
    forEach(fn, thisArg) { this._all().forEach(fn, thisArg); }
    keys() { return this._all().keys(); }
    values() { return this._all().values(); }
    entries() { return this._all().entries(); }
    [Symbol.iterator]() { return this._all()[Symbol.iterator](); }
    toString() { return this.value; }
    toggle(name, force) {
      const has = this.contains(name);
      const want = force === undefined ? !has : !!force;
      if (want) this.add(name); else this.remove(name);
      return want;
    }
    get length() { return this._all().length; }
    toString() { return this._all().join(" "); }
  }

  /// The base every event-dispatching thing extends, including code that has
  /// nothing to do with the document.
  ///
  /// Frameworks write `class Store extends EventTarget`, and its absence was a
  /// bare `ReferenceError: EventTarget is not defined` that took down whole
  /// bundles. Deliberately *not* the DOM's `Node`: a store is not in the tree,
  /// and giving it a node id it does not have would be the plausible-wrong
  /// answer this engine keeps having to avoid.
  class EventTarget {
    addEventListener(type, handler, options) {
      if (typeof handler !== "function" && typeof handler?.handleEvent !== "function") return;
      (this.__listeners ??= new Map()).set(handler, { type: String(type), options });
    }
    removeEventListener(type, handler) {
      void type;
      this.__listeners?.delete(handler);
    }
    dispatchEvent(event) {
      if (!event || typeof event.type !== "string") return true;
      if (event.target === null || event.target === undefined) {
        try { event.target = this; event.currentTarget = this; } catch (_) {}
      }
      for (const [handler, registered] of this.__listeners ?? []) {
        if (registered.type !== event.type) continue;
        try {
          if (typeof handler === "function") handler.call(this, event);
          else handler.handleEvent(event);
        } catch (error) {
          console.error(`a ${event.type} listener threw: ${error}`);
        }
        if (registered.options && registered.options.once) this.__listeners.delete(handler);
      }
      return !event.defaultPrevented;
    }
  }

  class Node {
    constructor(id) {
      // `undefined` means "the element currently being upgraded". A custom
      // element's constructor runs as `super()` with no arguments — the class
      // never sees the node it is being attached to — so the id arrives out of
      // band, exactly as the construction stack works in a real engine.
      this._id = id === undefined ? upgrading : id;
    }

    get ownerDocument() { return document; }
    get isConnected() { return api.isConnected(this._id); }
    getRootNode() {
      // The document when attached, the top of the detached fragment when not
      // — which is how code decides whether it is inside the page yet.
      if (this.isConnected) return document;
      let top = this;
      while (top.parentNode) top = top.parentNode;
      return top;
    }
    contains(other) {
      for (let n = other; n; n = n.parentNode) {
        if (n._id === this._id) return true;
      }
      return false;
    }
    compareDocumentPosition(other) {
      if (!other || other._id === undefined) return 1; // DISCONNECTED
      if (other._id === this._id) return 0;
      if (this.contains(other)) return 20;  // CONTAINED_BY | FOLLOWING
      if (other.contains(this)) return 10;  // CONTAINS | PRECEDING
      const order = documentOrder();
      const mine = order.indexOf(this._id);
      const theirs = order.indexOf(other._id);
      if (mine < 0 || theirs < 0) return 1; // DISCONNECTED
      return theirs > mine ? 4 : 2;         // FOLLOWING : PRECEDING
    }

    // Cached at wrap time. A node's kind is fixed when it is created, and this
    // is read constantly — every `nodeType === 1` filter, every tree walk, and
    // `children` on top of that — so paying a call into the tree for a constant
    // was 1.9 µs on the hottest property in the DOM.
    get nodeType() {
      return this._kind !== undefined ? this._kind : api.nodeKind(this._id);
    }
    get parentNode() {
      const parent = api.parent(this._id);
      if (parent === null || parent === undefined) return null;
      // The parent of `<html>` is the document, and it has to *be* the document
      // — code walks up until it finds node type 9 and then asks that thing for
      // `body` and `documentElement`. Compared against a remembered id rather
      // than asked of the tree, because this is a walk: one call per ancestor
      // was two.
      if (parent === documentNodeId()) return document;
      return wrap(parent);
    }
    get parentElement() {
      const parent = this.parentNode;
      return parent && parent.nodeType === 1 ? parent : null;
    }
    get childNodes() { return api.children(this._id).map(wrap); }
    get firstChild() { return this.childNodes[0] || null; }
    get lastChild() { const c = this.childNodes; return c[c.length - 1] || null; }

    // Text for a text node, null for an element — the distinction is the whole
    // reason the property exists, and code that walks a tree branches on it.
    get nodeName() {
      if (this.nodeType === 3) return "#text";
      if (this.nodeType === 8) return "#comment";
      if (this.nodeType === 11) return "#document-fragment";
      return api.tagName(this._id);
    }

    get nodeValue() { return this.nodeType === 3 ? api.getText(this._id) : null; }
    set nodeValue(value) {
      if (this.nodeType === 3) this.textContent = value;
    }

    get textContent() { return api.getText(this._id); }
    set textContent(value) {
      api.setText(this._id, String(value));
      if (observers.length === 0) return;
      record({
        type: "characterData", target: this, addedNodes: [], removedNodes: [],
        attributeName: null, oldValue: null,
      });
      childListRecord(this, [], []);
    }

    appendChild(child) {
      // Inserting a fragment inserts its children and leaves the fragment
      // behind, which is the whole reason a fragment exists.
      if (child && child.nodeType === 11) {
        const moved = child.childNodes;
        for (const kid of moved) api.append(this._id, kid._id);
        if (child._children) child._children.length = 0;
        childListRecord(this, moved, []);
        notifyConnection(this);
        return child;
      }
      detachFromParent(child);
      api.append(this._id, child._id);
      childListRecord(this, [child], []);
      notifyConnection(child);
      return child;
    }
    insertBefore(child, anchor) {
      if (!anchor) return this.appendChild(child);
      if (child && child.nodeType === 11) {
        for (const kid of child.childNodes) api.insertBefore(anchor._id, kid._id);
        if (child._children) child._children.length = 0;
        notifyConnection(this);
        return child;
      }
      detachFromParent(child);
      api.insertBefore(anchor._id, child._id);
      childListRecord(this, [child], []);
      notifyConnection(child);
      return child;
    }
    cloneNode(deep) {
      // A fragment clones to a fragment holding clones of its children, which
      // is the shape `appendChild` then expects.
      if (this.nodeType === 11) {
        const fragment = new DocumentFragment();
        if (deep) {
          for (const kid of this.childNodes) fragment.appendChild(kid.cloneNode(true));
        }
        return fragment;
      }
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
    replaceChild(fresh, stale) {
      // Core DOM, and its absence is not a small gap: a hydrator that cannot
      // replace a node creates a new one beside it, which is how a page ends up
      // rendering its own content twice.
      if (!stale || stale.parentNode?._id !== this._id) {
        throw new TypeError("replaceChild: the node to replace is not a child of this node");
      }
      this.insertBefore(fresh, stale);
      this.removeChild(stale);
      return stale;
    }
    removeChild(child) {
      const leaving = collectCustom(child);
      api.removeNode(child._id);
      childListRecord(this, [], [child]);
      for (const node of leaving) fireDisconnected(node);
      return child;
    }
    remove() {
      const parent = this.parentNode;
      const leaving = collectCustom(this);
      api.removeNode(this._id);
      if (parent) childListRecord(parent, [], [this]);
      for (const node of leaving) fireDisconnected(node);
    }
    /// Insert siblings, and replace. All four are the same operation seen from
    /// four angles, and all four are what a framework calls to move a node.
    after(...items) {
      const parent = this.parentNode;
      if (!parent) return;
      const next = this.nextSibling;
      for (const item of items) {
        const node = item instanceof Node ? item : document.createTextNode(String(item));
        next ? parent.insertBefore(node, next) : parent.appendChild(node);
      }
    }
    before(...items) {
      const parent = this.parentNode;
      if (!parent) return;
      for (const item of items) {
        const node = item instanceof Node ? item : document.createTextNode(String(item));
        parent.insertBefore(node, this);
      }
    }
    replaceWith(...items) {
      this.before(...items);
      this.remove();
    }
    replaceChildren(...items) {
      for (const kid of this.childNodes) kid.remove();
      this.append(...items);
    }

    prepend(...items) {
      const first = this.firstChild;
      for (const item of items) {
        const node = item instanceof Node ? item : document.createTextNode(String(item));
        first ? this.insertBefore(node, first) : this.appendChild(node);
      }
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
    // Searchable, because clone-query-fill-append is how a framework renders a
    // row and the query happens while the fragment is still detached. Each
    // child is its own scope: a fragment has no node of its own to search from.
    querySelector(sel) {
      for (const kid of this._children) {
        if (kid.nodeType !== 1) continue;
        if (kid.matches(sel)) return kid;
        const found = kid.querySelector(sel);
        if (found) return found;
      }
      return null;
    }
    querySelectorAll(sel) {
      const out = [];
      for (const kid of this._children) {
        if (kid.nodeType !== 1) continue;
        if (kid.matches(sel)) out.push(kid);
        out.push(...kid.querySelectorAll(sel));
      }
      return out;
    }
    get children() { return this._children.filter((n) => n.nodeType === 1); }
    get lastChild() { return this._children[this._children.length - 1] || null; }
    cloneNode(deep) {
      const copy = new DocumentFragment();
      if (deep) for (const kid of this._children) copy.appendChild(kid.cloneNode(true));
      return copy;
    }
  }

  /// The `CharacterData` interface, shared by text and comment nodes.
  ///
  /// `splitText` is the one that matters and the one that was missing:
  /// hydration splits a server-rendered text node when several vnodes share it,
  /// and a hydrator that cannot split creates fresh nodes instead — which is
  /// how preactjs.com rendered its version number twice and lost 147 lines of
  /// the page behind the mismatch.
  class CharacterData extends Node {
    get data() { return this.textContent; }
    set data(v) { this.textContent = v; }
    get length() { return this.data.length; }
    substringData(offset, count) { return this.data.substr(offset, count); }
    appendData(text) { this.data = this.data + String(text); }
    insertData(offset, text) {
      const current = this.data;
      this.data = current.slice(0, offset) + String(text) + current.slice(offset);
    }
    deleteData(offset, count) {
      const current = this.data;
      this.data = current.slice(0, offset) + current.slice(offset + count);
    }
    replaceData(offset, count, text) {
      const current = this.data;
      this.data = current.slice(0, offset) + String(text) + current.slice(offset + count);
    }
  }

  class Text extends CharacterData {
    get wholeText() {
      // Adjacent text nodes read as one run, which is what the property means.
      let text = "";
      let first = this;
      while (first.previousSibling && first.previousSibling.nodeType === 3) {
        first = first.previousSibling;
      }
      for (let n = first; n && n.nodeType === 3; n = n.nextSibling) text += n.data;
      return text;
    }
    splitText(offset) {
      const current = this.data;
      const at = Math.max(0, Math.min(Number(offset) || 0, current.length));
      const tail = document.createTextNode(current.slice(at));
      this.data = current.slice(0, at);
      const parent = this.parentNode;
      if (parent) {
        const next = this.nextSibling;
        if (next) parent.insertBefore(tail, next);
        else parent.appendChild(tail);
      }
      return tail;
    }
  }

  // A real comment node, not a text node wearing a hat: a marker that showed up
  // in `textContent` would appear in the outline an agent reads.
  class Comment extends CharacterData {
    get nodeType() { return 8; }
    get nodeValue() { return api.getText(this._id); }
  }

  class Element extends Node {
    get tagName() { return api.tagName(this._id); }
    get nodeName() { return this.tagName; }
    get children() { return collection(this.childNodes.filter((n) => n.nodeType === 1), "HTMLCollection"); }

    getAttribute(name) { return api.getAttr(this._id, String(name)); }
    setAttribute(name, value) {
      // The old value is only wanted by an observer or a custom element, and
      // reading it is a call into the tree. Skipping it when nobody is watching
      // is most of what `setAttribute` used to cost.
      const watched = observers.length > 0 || isCustom(this);
      const previous = watched ? api.getAttr(this._id, String(name)) : null;
      api.setAttr(this._id, String(name), String(value));
      if (watched) {
        const lowered = String(name).toLowerCase();
        recordAttribute(this, lowered, previous);
        fireAttributeChanged(this, lowered, previous, String(value));
      }
    }
    removeAttribute(name) {
      const watched = observers.length > 0 || isCustom(this);
      const previous = watched ? api.getAttr(this._id, String(name)) : null;
      api.removeAttr(this._id, String(name));
      if (watched) {
        const lowered = String(name).toLowerCase();
        recordAttribute(this, lowered, previous);
        fireAttributeChanged(this, lowered, previous, null);
      }
    }
    hasAttribute(name) { return api.getAttr(this._id, String(name)) !== null; }
    toggleAttribute(name, force) {
      const has = this.hasAttribute(name);
      const want = force === undefined ? !has : !!force;
      if (want) this.setAttribute(name, "");
      else this.removeAttribute(name);
      return want;
    }
    // Namespaces are not modelled — this engine parses HTML and nothing else —
    // so the namespace is dropped and the local name is used. Dropping it is
    // right for the case that actually occurs (`setAttributeNS(null, ...)`) and
    // honest for the rest: the attribute is set, under the name given.
    setAttributeNS(_namespace, name, value) { this.setAttribute(name, value); }
    getAttributeNS(_namespace, name) { return this.getAttribute(name); }
    removeAttributeNS(_namespace, name) { this.removeAttribute(name); }
    hasAttributeNS(_namespace, name) { return this.hasAttribute(name); }

    get id() { return this.getAttribute("id") || ""; }
    set id(v) { this.setAttribute("id", v); }
    get className() { return this.getAttribute("class") || ""; }
    set className(v) { this.setAttribute("class", v); }
    get classList() { return observed(new DOMTokenList(this, "class"), "DOMTokenList"); }
    set classList(v) { this.setAttribute("class", String(v)); }
    get relList() { return observed(new DOMTokenList(this, "rel"), "DOMTokenList"); }

    // Setting a URL part rewrites the href it came from, which is how routing
    // code edits a link in place.
    set protocol(v) { this._setUrlPart("protocol", v); }
    set host(v) { this._setUrlPart("host", v); }
    set hostname(v) { this._setUrlPart("hostname", v); }
    set port(v) { this._setUrlPart("port", v); }
    set pathname(v) { this._setUrlPart("pathname", v); }
    set search(v) { this._setUrlPart("search", v); }
    set hash(v) { this._setUrlPart("hash", v); }
    _setUrlPart(part, value) {
      const raw = api.getAttr(this._id, "href");
      if (raw === null) return;
      const url = new URL(raw, currentAddress);
      url[part] = value;
      this.setAttribute("href", url.href);
    }

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
    // `href` and `src` are *resolved*, which is the difference between the
    // property and `getAttribute`. A page comparing `link.href` to
    // `location.href`, or reading `script.src` to find its own origin, gets the
    // absolute URL a browser would give it rather than the raw `../x` in the
    // markup.
    get href() { return this._resolved("href"); }
    set href(v) { this.setAttribute("href", v); }
    get src() { return this._resolved("src"); }
    set src(v) { this.setAttribute("src", v); }
    _resolved(name) {
      const raw = api.getAttr(this._id, name);
      if (raw === null) return "";
      const parts = api.parseUrl(String(raw), currentAddress);
      return parts ? parts.href : raw;
    }

    // The pieces of that URL, which is how link-handling code decides whether a
    // click stays on the site. Empty on an element with no URL attribute, as in
    // a browser, rather than absent.
    get protocol() { return this._urlPart("protocol"); }
    get hostname() { return this._urlPart("hostname"); }
    get host() { return this._urlPart("host"); }
    get port() { return this._urlPart("port"); }
    get pathname() { return this._urlPart("pathname"); }
    get search() { return this._urlPart("search"); }
    get hash() { return this._urlPart("hash"); }
    get origin() { return this._urlPart("origin"); }
    _urlPart(part) {
      const raw = api.getAttr(this._id, "href") ?? api.getAttr(this._id, "src");
      if (raw === null) return "";
      const parts = api.parseUrl(String(raw), currentAddress);
      return parts ? parts[part] : "";
    }

    // HTML, always: this engine parses HTML and nothing else. `document` has no
    // such property in the DOM at all, which is why it is *defined as undefined*
    // there rather than left to report itself as a gap.
    get namespaceURI() { return "http://www.w3.org/1999/xhtml"; }

    // What a reset button restores, and what `dirty` checks compare against.
    // The attribute for an input, the original text for a textarea — the two
    // places HTML keeps it.
    get defaultValue() {
      if (this.tagName === "TEXTAREA") return api.getText(this._id);
      return api.getAttr(this._id, "value") || "";
    }
    set defaultValue(v) {
      if (this.tagName === "TEXTAREA") this.textContent = String(v);
      else this.setAttribute("value", v);
    }

    // `el.scrollTop + el.clientHeight >= el.scrollHeight` is how every
    // "am I at the bottom" check is written, so all six come from one call and
    // agree with each other. Only the document actually scrolls here: nothing
    // in this engine clips and scrolls a subtree, and a scrollTop that can
    // never change is better reported as zero than invented.
    get scrollTop() { return (api.scrollMetrics(this._id) || [0])[0]; }
    set scrollTop(y) { api.setScrollTop(this._id, Number(y)); }
    get scrollLeft() { return (api.scrollMetrics(this._id) || [0, 0])[1]; }
    // Nothing here scrolls horizontally — no subtree clips and scrolls — so the
    // write is accepted and does nothing rather than throwing at a page that is
    // merely restoring a saved position.
    set scrollLeft(_x) {}
    get scrollHeight() { return (api.scrollMetrics(this._id) || [0, 0, 0])[2]; }
    get scrollWidth() { return (api.scrollMetrics(this._id) || [0, 0, 0, 0])[3]; }

    // `<template>.content` is a fragment view; `<meta content>` reflects the
    // attribute. Same property name, two unrelated meanings, both real.
    get content() {
      if (this.tagName === "TEMPLATE") return new TemplateContent(this._id);
      if (this.tagName === "META") return api.getAttr(this._id, "content") || "";
      return undefined;
    }

    get firstElementChild() { return this.children[0] || null; }
    get lastElementChild() { const c = this.children; return c[c.length - 1] || null; }
    get childElementCount() { return this.children.length; }
    get nextElementSibling() {
      for (let n = this.nextSibling; n; n = n.nextSibling) if (n.nodeType === 1) return n;
      return null;
    }
    get previousElementSibling() {
      for (let n = this.previousSibling; n; n = n.previousSibling) if (n.nodeType === 1) return n;
      return null;
    }

    // The live list, in source order. Enough of a `NamedNodeMap` for the two
    // things code does with it: iterate it, and look a name up.
    get attributes() {
      const node = this;
      const list = api.attrNames(this._id).map((name) => ({
        name,
        value: api.getAttr(node._id, name),
      }));
      list.getNamedItem = (name) =>
        list.find((a) => a.name === String(name).toLowerCase()) || null;
      return list;
    }
    hasAttributes() { return api.attrNames(this._id).length > 0; }
    getAttributeNames() { return api.attrNames(this._id); }

    // Nothing is animating: this engine has no frames at rest, so there is
    // never an animation in progress to report. An empty list is what a browser
    // returns in that state, and it is the truth rather than a stub.
    getAnimations() { return []; }

    // An iframe's document, which this engine does not load — see the note the
    // snapshot carries when a page has frames. Null is what a browser returns
    // for a frame it will not let you into, so a page's fallback path is the
    // right one to take.
    get contentDocument() { return null; }
    get contentWindow() { return null; }

    // Lowercase, always: this engine parses HTML, where the local name is
    // case-insensitive and canonically lower, while `tagName` is upper.
    get localName() { return this.tagName.toLowerCase(); }

    get contentEditable() {
      const raw = api.getAttr(this._id, "contenteditable");
      return raw === null ? "inherit" : (raw === "" ? "true" : raw);
    }
    set contentEditable(v) { this.setAttribute("contenteditable", v); }
    get isContentEditable() {
      for (let n = this; n; n = n.parentElement) {
        const raw = api.getAttr(n._id, "contenteditable");
        if (raw === "true" || raw === "") return true;
        if (raw === "false") return false;
      }
      return false;
    }

    get lang() { return api.getAttr(this._id, "lang") || ""; }
    set lang(v) { this.setAttribute("lang", v); }
    get title() { return api.getAttr(this._id, "title") || ""; }
    set title(v) { this.setAttribute("title", v); }
    get alt() { return api.getAttr(this._id, "alt") || ""; }
    set alt(v) { this.setAttribute("alt", v); }

    // Bring the element into view for a screenshot or a live viewer. The
    // outline an agent reads covers the whole document either way, so this
    // changes what a *human* watching sees and nothing about what is readable.
    // Attach a shadow root, flattened into this element. See `ShadowRoot` for
    // what that costs and why it is the right trade for a reading engine.
    attachShadow(init) {
      if (this._shadow) {
        throw new Error("attachShadow: this element already has a shadow root");
      }
      const mode = String((init && init.mode) || "open");
      // Light children are taken out of the way first. A browser stops
      // rendering them once a shadow root exists unless they are slotted, and
      // leaving them would show a component's input and its output at once.
      const light = this.childNodes;
      for (const kid of light) detachFromParent(kid);

      const root = new ShadowRoot(this._id, mode);
      root._light = light;
      this._shadow = root;
      return root;
    }
    // Null for a closed root, as in a browser: the component asked for that,
    // and the flattening already leaks more than it should.
    get shadowRoot() {
      return this._shadow && this._shadow.mode === "open" ? this._shadow : null;
    }

    scrollIntoView() { api.scrollToNode(this._id); }
    // An element does not scroll here — nothing clips and scrolls a subtree —
    // so this moves the document, which is what the caller wanted when the
    // element was the document's own scroller.
    scrollTo(x, y) {
      const top = typeof x === "object" && x !== null ? x.top : y;
      api.setScrollTop(this._id, Number(top) || 0);
    }
    scrollBy(x, y) {
      const by = typeof x === "object" && x !== null ? x.top : y;
      api.setScrollTop(this._id, this.scrollTop + (Number(by) || 0));
    }

    // `<select>`. `selectedIndex` is how form code both reads and sets a
    // choice, and assigning it has to move the `selected` attribute or the
    // element and the DOM disagree about what is chosen.
    get selectedIndex() {
      const options = this.options;
      const at = options.findIndex((o) => o.selected);
      // A `<select>` with nothing marked selects its first option; -1 is only
      // right when there are no options at all.
      if (at >= 0) return at;
      return options.length ? 0 : -1;
    }
    set selectedIndex(index) {
      const options = this.options;
      const want = Number(index);
      options.forEach((option, at) => { option.selected = at === want; });
    }
    add(option, before) {
      if (before === undefined || before === null) return this.appendChild(option);
      const anchor = typeof before === "number" ? this.options[before] : before;
      return anchor ? this.insertBefore(option, anchor) : this.appendChild(option);
    }

    get disabled() { return api.getAttr(this._id, "disabled") !== null; }
    set disabled(on) {
      if (on) this.setAttribute("disabled", "");
      else this.removeAttribute("disabled");
    }
    get name() { return api.getAttr(this._id, "name") || ""; }
    set name(v) { this.setAttribute("name", v); }
    get type() { return (api.getAttr(this._id, "type") || "text").toLowerCase(); }
    set type(v) { this.setAttribute("type", v); }
    get options() { return this.querySelectorAll("option"); }

    // Real serialisation. Returning textContent here silently stripped every
    // tag, so `el.innerHTML = el.innerHTML` destroyed the subtree.
    get innerHTML() { return api.innerHtml(this._id); }
    set innerHTML(html) { api.setInnerHtml(this._id, String(html)); }
    get outerHTML() { return api.outerHtml(this._id); }
    set outerHTML(html) {
      // Replacing an element with its own markup, which is how a component
      // swaps itself out. The node is gone afterwards, as in a browser.
      const parent = this.parentNode;
      if (!parent) return;
      const host = document.createElement("div");
      host.innerHTML = String(html);
      const replacements = host.childNodes;
      for (const kid of replacements) parent.insertBefore(kid, this);
      this.remove();
    }

    // Deliberately not watched. A style declaration answers *any* CSS property
    // name by design — it is already a proxy over the dashed surface — so there
    // is no such thing as a name it is missing, and wrapping one proxy in
    // another defeats the `in` check the reporting one relies on.
    get style() { return new StyleDeclaration(this); }
    set style(text) { this.setAttribute("style", String(text)); }

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
    querySelectorAll(sel) { return collection(api.queryAll(String(sel), this._id).map(wrap)); }
    getElementsByTagName(tag) { return collection(api.queryAll(String(tag), this._id).map(wrap), "HTMLCollection"); }
    getElementsByClassName(cls) { return collection(api.queryAll("." + String(cls), this._id).map(wrap), "HTMLCollection"); }

    matches(sel) { return api.matchesSelector(this._id, String(sel)); }
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
    // Not the bounding rect: for `documentElement` and `body` this is the
    // *viewport*, not the element's own height, and the bottom-of-page check
    // every page writes compares the two.
    get clientWidth() { return (api.scrollMetrics(this._id) || [0, 0, 0, 0, 0, 0])[5]; }
    get clientHeight() { return (api.scrollMetrics(this._id) || [0, 0, 0, 0, 0])[4]; }
  }

  // What `attachShadow` hands back.
  //
  // This engine has one tree, and blitz has no notion of a shadow one — so a
  // shadow root here is a *view of the host element*, and everything a
  // component renders into it lands in the host. That is the flattening a
  // browser's accessibility tree performs anyway, and it is what an agent
  // wants: the component's rendered output, in the page, readable.
  //
  // The cost is stated rather than hidden: **encapsulation is not enforced**.
  // `document.querySelector` reaches inside a shadow root here and would not in
  // a browser, and styles do not scope. What is preserved is the part that
  // decides whether a page can be read at all — the content renders, `host` and
  // `mode` answer, and light children are projected into a `<slot>` if the
  // component declares one.
  class ShadowRoot extends Element {
    constructor(hostId, mode) {
      super(hostId);
      this._mode = mode;
      this._light = [];
    }
    get nodeType() { return 11; }
    get nodeName() { return "#document-fragment"; }
    get mode() { return this._mode; }
    get host() { return wrap(this._id); }
    get activeElement() { return null; }
    get styleSheets() { return []; }
    // The host is where content actually lives, so `innerHTML` on the root and
    // on the host are the same string — and setting it re-runs projection,
    // because the `<slot>` a component declares usually arrives with it.
    set innerHTML(html) {
      api.setInnerHtml(this._id, String(html));
      this._project();
    }
    get innerHTML() { return api.innerHtml(this._id); }
    appendChild(child) {
      const out = Element.prototype.appendChild.call(this, child);
      this._project();
      return out;
    }
    // Put the light children where the component said they should go. One
    // unnamed slot, which is what the overwhelming majority declare; a page
    // using named slots keeps its light content out of the way instead, which
    // is a gap worth reporting rather than guessing at.
    _project() {
      if (this._light.length === 0) return;
      const slot = wrap(api.query("slot", this._id));
      if (!slot) return;
      const pending = this._light;
      this._light = [];
      for (const node of pending) slot.appendChild(node);
    }
  }

  // What `<template>.content` hands back.
  //
  // A template's children are parsed into the tree here rather than into a
  // separate document, so this is a *view* of the template node that answers
  // `nodeType` as a fragment. That is enough for the two things pages do with
  // it — clone it, and query inside it — without pretending there is a second
  // document underneath.
  //
  // Its absence was not a small gap: `template.content.cloneNode(true)` threw
  // `cannot convert 'null' or 'undefined' to object`, which was the *entire*
  // text of fifteen module failures across the application corpus.
  class TemplateContent extends Element {
    get nodeType() { return 11; }
    get nodeName() { return "#document-fragment"; }
  }

  // `el.onclick = fn` is the other way to bind a handler, and plenty of
  // generated code still uses it. Defined rather than enumerated by hand so the
  // property and `addEventListener` cannot disagree about what is registered.
  // Attributes that are nothing but a string on the element. Defined from a
  // table rather than written out twice, so a getter can never exist without
  // its setter — which is the bug this table was written to end: in a module,
  // which is strict, assigning to a getter-only property *throws*, and a
  // classic-script test cannot see it because sloppy mode swallows it.
  const REFLECTED_ATTRIBUTES = {
    dir: "dir",
    rel: "rel",
    slot: "slot",
    crossOrigin: "crossorigin",
    integrity: "integrity",
    referrerPolicy: "referrerpolicy",
    accessKey: "accesskey",
    placeholder: "placeholder",
    htmlFor: "for",
    target: "target",
    media: "media",
    charset: "charset",
    loading: "loading",
    decoding: "decoding",
    autocomplete: "autocomplete",
  };
  for (const [property, attribute] of Object.entries(REFLECTED_ATTRIBUTES)) {
    Object.defineProperty(Element.prototype, property, {
      configurable: true,
      get() { return api.getAttr(this._id, attribute) || ""; },
      set(value) { this.setAttribute(attribute, value); },
    });
  }

  // Boolean and numeric reflections, which convert rather than pass through.
  Object.defineProperty(Element.prototype, "hidden", {
    configurable: true,
    get() { return api.getAttr(this._id, "hidden") !== null; },
    set(on) { if (on) this.setAttribute("hidden", ""); else this.removeAttribute("hidden"); },
  });
  Object.defineProperty(Element.prototype, "tabIndex", {
    configurable: true,
    get() {
      const raw = api.getAttr(this._id, "tabindex");
      return raw === null ? -1 : Number(raw) || 0;
    },
    set(value) { this.setAttribute("tabindex", String(Number(value) || 0)); },
  });

  const HANDLER_EVENTS = [
    "click", "dblclick", "mousedown", "mouseup", "mouseover", "mouseout", "mousemove",
    "input", "change", "submit", "focus", "blur", "keydown", "keyup", "keypress",
    "load", "error", "scroll", "wheel", "contextmenu", "pointerdown", "pointerup",
    "touchstart", "touchend", "animationend", "transitionend",
  ];
  for (const type of HANDLER_EVENTS) {
    const slot = `__on_${type}`;
    Object.defineProperty(Element.prototype, `on${type}`, {
      configurable: true,
      get() { return this[slot] ?? null; },
      set(handler) {
        // Assigning replaces whatever the property held before, which is what
        // makes it different from `addEventListener` — two assignments leave
        // one handler, not two.
        if (this[slot]) this.removeEventListener(type, this[slot]);
        this[slot] = typeof handler === "function" ? handler : null;
        if (this[slot]) this.addEventListener(type, this[slot]);
      },
    });
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
  // There was a `missingApi(name)` helper here that returned a proxy throwing a
  // named TypeError for anything this engine lacked. It is gone, and its
  // removal is the point.
  //
  // It made `typeof WebSocket` answer "function" and `'serviceWorker' in
  // navigator` answer true — so every page that *correctly* feature-detects
  // took the branch for an API that then threw. That is the plausible-wrong
  // answer this engine exists to refuse, written by us, and it cost three real
  // sites their whole bundle.
  //
  // An API we do not have is now simply absent, which is what a browser lacking
  // it looks like, and absence is already named: a global reports itself
  // through the ReferenceError parser in `Script::name_missing_global`, and a
  // property through the reporting proxy in `observed`.

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

  // A media query, answered from the viewport the engine actually renders at.
  //
  // Returning `false` to everything — which is what a stub does — is not
  // neutral: a responsive layout asks `(min-width: …)` and then commits to the
  // branch it was told, so a wrong answer is a wrong page rather than a missing
  // feature. The features below have real answers here; anything else records
  // itself and reports no match, so the gap is visible instead of guessed at.
  function matchMedia(query) {
    const text = String(query || "");
    const view = api.viewport();
    const list = {
      media: text,
      matches: evaluateQuery(text, view),
      onchange: null,
      // The viewport never changes size mid-session, so a listener here can
      // never fire. Accepted silently rather than recorded, because a page
      // registering one is not asking for anything we lack.
      addListener() {}, removeListener() {},
      addEventListener() {}, removeEventListener() {},
      dispatchEvent() { return false; },
    };
    return list;
  }

  function evaluateQuery(text, view) {
    const clauses = text.split(",").map((c) => c.trim()).filter(Boolean);
    if (clauses.length === 0) return false;
    // A comma-separated list is a disjunction, and `and` within a clause is a
    // conjunction. That is the whole grammar a page in practice uses.
    return clauses.some((clause) =>
      clause
        .split(/\band\b/)
        .map((part) => part.trim())
        .every((part) => evaluateFeature(part, view))
    );
  }

  function evaluateFeature(part, view) {
    const bare = part.replace(/^\(|\)$/g, "").trim().toLowerCase();
    if (!bare) return false;
    if (bare === "all" || bare === "screen") return true;
    if (bare === "print" || bare === "speech") return false;

    const at = bare.indexOf(":");
    if (at < 0) {
      api.unsupported(`matchMedia(${bare})`);
      return false;
    }
    const name = bare.slice(0, at).trim();
    const value = bare.slice(at + 1).trim();
    const px = (v) => parseFloat(v.replace(/px$/, ""));

    switch (name) {
      case "min-width": return view.width >= px(value);
      case "max-width": return view.width <= px(value);
      case "width": return view.width === px(value);
      case "min-height": return view.height >= px(value);
      case "max-height": return view.height <= px(value);
      case "height": return view.height === px(value);
      case "orientation": return value === (view.width >= view.height ? "landscape" : "portrait");
      case "prefers-color-scheme": return value === view.colorScheme;
      // Nothing animates here and there is no pointer, so these are not
      // guesses — they are what this engine is.
      case "prefers-reduced-motion": return value === "reduce";
      case "hover": return value === "none";
      case "any-hover": return value === "none";
      case "pointer": return value === "none";
      case "any-pointer": return value === "none";
      default:
        api.unsupported(`matchMedia(${name})`);
        return false;
    }
  }

  // ── layout observers ─────────────────────────────────────────────────────
  //
  // Both are driven from the settle loop rather than from a frame clock: this
  // engine has no frames at rest, and an observer that only fired on a repaint
  // would never fire at all. Checked after layout has been resolved, so the
  // rectangles they report are the ones that were actually laid out.

  const intersectionObservers = [];
  const resizeObservers = [];

  class IntersectionObserver {
    constructor(callback, options) {
      this._callback = callback;
      this._targets = [];
      this._seen = new Map();
      const raw = (options && options.threshold) ?? 0;
      this._thresholds = Array.isArray(raw) ? raw.slice().sort() : [raw];
      this.root = (options && options.root) || null;
      this.rootMargin = (options && options.rootMargin) || "0px";
    }
    observe(target) {
      if (!this._targets.includes(target)) this._targets.push(target);
      if (!intersectionObservers.includes(this)) intersectionObservers.push(this);
    }
    unobserve(target) {
      const at = this._targets.indexOf(target);
      if (at >= 0) this._targets.splice(at, 1);
    }
    disconnect() {
      this._targets.length = 0;
      const at = intersectionObservers.indexOf(this);
      if (at >= 0) intersectionObservers.splice(at, 1);
    }
    takeRecords() { return []; }

    _check(view) {
      const entries = [];
      for (const target of this._targets) {
        const [x, y, width, height] = api.rect(target._id) || [0, 0, 0, 0];
        const visibleW = Math.max(0, Math.min(x + width, view.width) - Math.max(x, 0));
        const visibleH = Math.max(0, Math.min(y + height, view.height) - Math.max(y, 0));
        const area = width * height;
        const ratio = area > 0 ? (visibleW * visibleH) / area : 0;
        const isIntersecting = this._thresholds.some(
          (t) => (t === 0 ? ratio > 0 : ratio >= t)
        );
        // Edges only: a page that lazy-loads on entry should be told once, not
        // on every settle for as long as the element stays on screen.
        if (this._seen.get(target._id) === isIntersecting) continue;
        this._seen.set(target._id, isIntersecting);
        entries.push({
          target, isIntersecting, intersectionRatio: ratio,
          boundingClientRect: target.getBoundingClientRect(),
          intersectionRect: { x, y, width: visibleW, height: visibleH,
                              top: y, left: x, right: x + visibleW, bottom: y + visibleH },
          rootBounds: { x: 0, y: 0, width: view.width, height: view.height,
                        top: 0, left: 0, right: view.width, bottom: view.height },
          time: clock,
        });
      }
      if (entries.length) deliverTo(this, entries);
    }
  }

  class ResizeObserver {
    constructor(callback) { this._callback = callback; this._targets = []; this._seen = new Map(); }
    observe(target) {
      if (!this._targets.includes(target)) this._targets.push(target);
      if (!resizeObservers.includes(this)) resizeObservers.push(this);
    }
    unobserve(target) {
      const at = this._targets.indexOf(target);
      if (at >= 0) this._targets.splice(at, 1);
    }
    disconnect() {
      this._targets.length = 0;
      const at = resizeObservers.indexOf(this);
      if (at >= 0) resizeObservers.splice(at, 1);
    }

    _check() {
      const entries = [];
      for (const target of this._targets) {
        const [, , width, height] = api.rect(target._id) || [0, 0, 0, 0];
        const previous = this._seen.get(target._id);
        // The first observation always fires, which is what a browser does and
        // what layout code depends on for its initial measurement.
        if (previous && previous.width === width && previous.height === height) continue;
        this._seen.set(target._id, { width, height });
        entries.push({
          target,
          contentRect: { x: 0, y: 0, width, height, top: 0, left: 0, right: width, bottom: height },
          borderBoxSize: [{ inlineSize: width, blockSize: height }],
          contentBoxSize: [{ inlineSize: width, blockSize: height }],
        });
      }
      if (entries.length) deliverTo(this, entries);
    }
  }

  function deliverTo(observer, entries) {
    try {
      observer._callback(entries, observer);
    } catch (error) {
      console.error("observer callback threw: " + error);
    }
  }

  // Called by the host after layout, once per settle round.
  globalThis.__h5iRunLayoutObservers = function () {
    if (intersectionObservers.length === 0 && resizeObservers.length === 0) return 0;
    const view = api.viewport();
    let ran = 0;
    for (const observer of intersectionObservers.slice()) { observer._check(view); ran++; }
    for (const observer of resizeObservers.slice()) { observer._check(); ran++; }
    return ran;
  };

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
    // Observing the *document* is what a framework does to watch a whole page —
    // Vite's module-preload polyfill opens with exactly this — and `document`
    // is not a `Node` here, so it has no `contains`. Calling it threw "not a
    // callable function" from inside this engine, on every page that mutated
    // the DOM after registering such an observer.
    const inScope = target.nodeType === 9
      ? record.target.isConnected
      : options.subtree
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

  function recordAttribute(target, attributeName, oldValue) {
    if (observers.length === 0) return;
    record({
      type: "attributes", target, addedNodes: [], removedNodes: [],
      attributeName, oldValue,
    });
  }

  function childListRecord(target, added, removed) {
    // Built only if something will read it: the arrays and the object are pure
    // waste on a page with no observer, and every insertion made one.
    if (observers.length === 0) return;
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

  // ── traversal ────────────────────────────────────────────────────────────
  //
  // `whatToShow` is a bitmask over node types, where the bit is `1 << (type-1)`:
  // element 1, text 4, comment 128. That arithmetic is the whole filter, plus
  // the caller's own function.

  const NodeFilter = {
    SHOW_ALL: 0xffffffff,
    SHOW_ELEMENT: 1,
    SHOW_TEXT: 4,
    SHOW_COMMENT: 128,
    FILTER_ACCEPT: 1,
    FILTER_REJECT: 2,
    FILTER_SKIP: 3,
  };

  function accepts(node, whatToShow, filter) {
    if (!node) return false;
    if (!(whatToShow & (1 << (node.nodeType - 1)))) return false;
    if (!filter) return true;
    const verdict = typeof filter === "function" ? filter(node) : filter.acceptNode(node);
    return verdict === NodeFilter.FILTER_ACCEPT;
  }

  /// Shared by both traversal objects: the subtree rooted at `root`, filtered.
  function traversable(root, whatToShow, filter) {
    const out = [];
    const visit = (id) => {
      const node = wrap(id);
      if (accepts(node, whatToShow, filter)) out.push(node);
      for (const kid of api.children(id)) visit(kid);
    };
    visit(root._id);
    return out;
  }

  class NodeIterator {
    constructor(root, whatToShow, filter) {
      this.root = root;
      this.whatToShow = whatToShow;
      this.filter = filter;
      this.referenceNode = root;
      this._at = -1;
    }
    _list() { return traversable(this.root, this.whatToShow, this.filter); }
    nextNode() {
      const list = this._list();
      this._at += 1;
      if (this._at >= list.length) { this._at = list.length; return null; }
      this.referenceNode = list[this._at];
      return this.referenceNode;
    }
    previousNode() {
      const list = this._list();
      this._at -= 1;
      if (this._at < 0) { this._at = -1; return null; }
      this.referenceNode = list[this._at];
      return this.referenceNode;
    }
    detach() {}
  }

  class TreeWalker {
    constructor(root, whatToShow, filter) {
      this.root = root;
      this.whatToShow = whatToShow;
      this.filter = filter;
      this.currentNode = root;
    }
    _list() { return traversable(this.root, this.whatToShow, this.filter); }
    _step(by) {
      const list = this._list();
      const at = list.findIndex((n) => n._id === this.currentNode._id);
      const next = list[(at < 0 ? (by > 0 ? -1 : list.length) : at) + by];
      if (!next) return null;
      this.currentNode = next;
      return next;
    }
    nextNode() { return this._step(1); }
    previousNode() { return this._step(-1); }
    parentNode() {
      for (let n = this.currentNode.parentNode; n; n = n.parentNode) {
        if (accepts(n, this.whatToShow, this.filter)) { this.currentNode = n; return n; }
        if (n._id === this.root._id) break;
      }
      return null;
    }
    firstChild() {
      for (const kid of this.currentNode.childNodes) {
        if (accepts(kid, this.whatToShow, this.filter)) { this.currentNode = kid; return kid; }
      }
      return null;
    }
    nextSibling() {
      for (let n = this.currentNode.nextSibling; n; n = n.nextSibling) {
        if (accepts(n, this.whatToShow, this.filter)) { this.currentNode = n; return n; }
      }
      return null;
    }
    previousSibling() {
      for (let n = this.currentNode.previousSibling; n; n = n.previousSibling) {
        if (accepts(n, this.whatToShow, this.filter)) { this.currentNode = n; return n; }
      }
      return null;
    }
  }

  /// The parts of `Range` pages actually use.
  ///
  /// Two of them, really: `createContextualFragment`, which is how a library
  /// turns a string of markup into nodes, and `getBoundingClientRect`, which is
  /// how it measures text. The rest of the interface is a selection model this
  /// engine has no use for, and anything reached for beyond what is here
  /// reports itself rather than silently doing nothing.
  class Range {
    constructor() {
      this.startContainer = null;
      this.endContainer = null;
      this.collapsed = true;
    }
    selectNode(node) { this.selectNodeContents(node); }
    selectNodeContents(node) {
      this.startContainer = node;
      this.endContainer = node;
      this.collapsed = false;
    }
    setStart(node, _offset) { this.startContainer = node; this.collapsed = false; }
    setEnd(node, _offset) { this.endContainer = node; this.collapsed = false; }
    collapse(toStart) {
      this.collapsed = true;
      if (toStart) this.endContainer = this.startContainer;
      else this.startContainer = this.endContainer;
    }
    get commonAncestorContainer() { return this.startContainer; }
    getBoundingClientRect() {
      return this.startContainer && this.startContainer.getBoundingClientRect
        ? this.startContainer.getBoundingClientRect()
        : { x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
    }
    getClientRects() { return [this.getBoundingClientRect()]; }
    createContextualFragment(html) {
      const host = document.createElement("div");
      host.innerHTML = String(html);
      const fragment = new DocumentFragment();
      for (const kid of host.childNodes) fragment.appendChild(kid);
      return fragment;
    }
    deleteContents() {
      if (this.startContainer) for (const kid of this.startContainer.childNodes) kid.remove();
    }
    cloneRange() {
      const copy = new Range();
      copy.startContainer = this.startContainer;
      copy.endContainer = this.endContainer;
      copy.collapsed = this.collapsed;
      return copy;
    }
    detach() {}
    toString() {
      return this.startContainer ? this.startContainer.textContent : "";
    }
  }

  /// `new DOMParser().parseFromString(html, "text/html")`.
  ///
  /// How a library turns a string of markup into something it can query —
  /// sanitizers, template engines and lit all do it. What comes back is a
  /// parsed subtree presented as a document, not a second document: this engine
  /// has one tree, so the result shares its arena. That is enough for reading
  /// and querying, which is all this is used for, and no script inside the
  /// string runs — which is the property a sanitizer is relying on anyway.
  class DOMParser {
    parseFromString(markup, type) {
      const kind = String(type || "text/html").toLowerCase();
      if (kind !== "text/html" && kind !== "application/xhtml+xml") {
        api.unsupported(`DOMParser.parseFromString(${kind})`);
      }
      const root = document.createElement("html");
      const head = document.createElement("head");
      const body = document.createElement("body");
      root.appendChild(head);
      root.appendChild(body);
      body.innerHTML = String(markup);

      return observed({
        documentElement: root,
        body,
        // A real parsed document always has a head, even for a fragment of
        // markup that contains none. Returning null here was enough to take
        // preactjs.com's markup component down with a null dereference, and the
        // page then re-rendered everything it had already server-rendered.
        head,
        nodeType: 9,
        contentType: kind,
        get title() {
          const found = body.querySelector("title");
          return found ? found.textContent : "";
        },
        querySelector: (sel) => root.querySelector(sel),
        querySelectorAll: (sel) => root.querySelectorAll(sel),
        getElementById: (id) => root.querySelector("#" + String(id)),
        getElementsByTagName: (tag) => root.getElementsByTagName(tag),
        getElementsByClassName: (cls) => root.getElementsByClassName(cls),
        createDocumentFragment: () => new DocumentFragment(),
        createComment: (text) => document.createComment(text),
        createElement: (tag) => document.createElement(tag),
        createTextNode: (text) => document.createTextNode(text),
        importNode: (node, deep) => node.cloneNode(deep),
        adoptNode: (node) => node,
      }, "parsed document");
    }
  }

  const documentImpl = {
    get documentElement() { return wrap(api.root()); },
    get body() { return wrap(api.body()); },
    get head() { return wrap(api.query("head", 0)); },
    createElement(tag) { return wrap(api.createElement(String(tag))); },
    // SVG and MathML arrive through this, and every framework that draws an
    // icon calls it. The namespace is dropped because this engine models one:
    // the element is created under its local name, which is what the renderer
    // can do something with.
    createElementNS(_namespace, tag) { return wrap(api.createElement(String(tag))); },
    createRange() { return observed(new Range(), "Range"); },
    // The pre-constructor way of making an event, still emitted by older
    // libraries and by anything compiled for old targets. The event is inert
    // until `initEvent` names it, which is exactly how the legacy API works.
    createEvent(kind) {
      const event = new Event("", {});
      event.initEvent = (type, bubbles, cancelable) => {
        event.type = String(type);
        event.bubbles = !!bubbles;
        event.cancelable = !!cancelable;
      };
      void kind;
      return event;
    },
    elementFromPoint(x, y) { return wrap(api.elementFromPoint(Number(x), Number(y))); },
    elementsFromPoint(x, y) {
      const found = wrap(api.elementFromPoint(Number(x), Number(y)));
      // The ancestors of the hit, topmost first, which is what the plural form
      // returns and what a library walking for a scroll container wants.
      const out = [];
      for (let n = found; n; n = n.parentNode) if (n.nodeType === 1) out.push(n);
      return collection(out);
    },
    createTextNode(text) { return wrap(api.createText(String(text))); },
    createDocumentFragment() { return new DocumentFragment(); },
    createComment(text) {
      const id = api.createComment(String(text));
      comments.add(id);
      return wrap(id);
    },
    // One document means importing is cloning. Saying that plainly is better
    // than a stub that returns nothing and leaves the caller inserting null.
    importNode(node, deep) { return node.cloneNode(deep); },
    adoptNode(node) { return node; },
    createNodeIterator(root, whatToShow, filter) {
      return new NodeIterator(root, whatToShow === undefined ? NodeFilter.SHOW_ALL : whatToShow, filter);
    },
    createTreeWalker(root, whatToShow, filter) {
      return new TreeWalker(root, whatToShow === undefined ? NodeFilter.SHOW_ALL : whatToShow, filter);
    },
    // Real API in its own right: `document.contains(node)` is how code asks
    // whether something is still on the page.
    contains(node) {
      return !!node && node.isConnected === true;
    },
    getElementsByName(name) {
      return api.queryAll(`[name="${String(name).replace(/"/g, '\\"')}"]`, 0).map(wrap);
    },
    querySelector(sel) { return wrap(api.query(String(sel), 0)); },
    querySelectorAll(sel) { return collection(api.queryAll(String(sel), 0).map(wrap)); },
    getElementById(id) { return wrap(api.query("#" + String(id), 0)); },
    getElementsByTagName(tag) { return collection(api.queryAll(String(tag), 0).map(wrap), "HTMLCollection"); },
    getElementsByClassName(cls) { return collection(api.queryAll("." + String(cls), 0).map(wrap), "HTMLCollection"); },
    addEventListener(type, handler, options) {
      const root = wrap(api.root());
      if (root) root.addEventListener(type, handler, options);
    },
    removeEventListener(type, handler) {
      const root = wrap(api.root());
      if (root) root.removeEventListener(type, handler);
    },
    // Non-HttpOnly cookies only, exactly as a browser exposes them. The
    // withholding is the point: a session credential is almost always HttpOnly,
    // and anything script can read it can write into the DOM, where the agent
    // reads it.
    get cookie() { return api.readCookies(); },
    set cookie(value) { api.writeCookie(String(value)); },
    get readyState() { return "complete"; },

    // A document is node type 9 and its child is the root element. Scripts that
    // walk upward from a node and stop at the document depend on both.
    get nodeType() { return 9; },
    get nodeName() { return "#document"; },
    get childNodes() { const root = wrap(api.root()); return root ? [root] : []; },
    get defaultView() { return globalThis; },
    get location() { return location; },
    get URL() { return currentAddress; },
    get documentURI() { return currentAddress; },
    // What relative URLs on this page resolve against — the `<base href>` if
    // the page set one, and the address otherwise.
    get baseURI() {
      const base = wrap(api.query("base[href]", 0));
      if (!base) return currentAddress;
      const parts = api.parseUrl(api.getAttr(base._id, "href") || "", currentAddress);
      return parts ? parts.href : currentAddress;
    },
    // This engine parses HTML and nothing else, so there is one honest answer.
    contentType: "text/html",

    // Empty, and true: this engine sends no `Referer`, so a page told anything
    // else would be told a lie about a request it can check.
    get referrer() { return ""; },

    get title() {
      const el = wrap(api.query("title", 0));
      return el ? el.textContent : "";
    },
    set title(value) {
      let el = wrap(api.query("title", 0));
      if (!el) {
        const head = wrap(api.query("head", 0));
        if (!head) return;
        el = document.createElement("title");
        head.appendChild(el);
      }
      el.textContent = String(value);
    },

    // Set by the host around each classic script, null inside a module or a
    // later callback — the same rule a browser follows.
    get currentScript() {
      const id = globalThis.__h5iCurrentScript;
      return id === null || id === undefined ? null : wrap(id);
    },

    get forms() { return api.queryAll("form", 0).map(wrap); },
    get images() { return api.queryAll("img", 0).map(wrap); },
    get scripts() { return api.queryAll("script", 0).map(wrap); },
    // Only anchors that actually have an href, which is what the collection is
    // defined to hold — a named anchor is not a link.
    get links() { return api.queryAll("a[href], area[href]", 0).map(wrap); },

    // Nothing is focused until something is: this engine has no focus ring, and
    // the body is what a browser reports in that state.
    // A Document has no `namespaceURI` and its `ownerDocument` is null — both
    // are true of a real browser. Defined rather than absent so the reporting
    // proxy does not name them as gaps: something no engine has is not
    // something this engine is missing.
    namespaceURI: undefined,
    ownerDocument: null,
    get implementation() {
      return observed({
        hasFeature: () => true,
        // The same shape `DOMParser` produces, which is what this is for: a
        // detached document to build markup in. It shares this engine's one
        // tree, so it is a subtree presented as a document rather than a second
        // one — enough for building and querying, which is all it is used for.
        createHTMLDocument: (title) =>
          new DOMParser().parseFromString(
            `<title>${String(title ?? "")}</title>`,
            "text/html",
          ),
        // A second document is genuinely out of reach here — there is one tree,
        // and it is the page. A page using this to parse HTML off to the side
        // gets a named refusal instead of a silently broken document.
      }, "document.implementation");
    },

    // The famous one. Legacy code uses `document.all` to detect old IE, and the
    // detection works because it is the only object in JavaScript that is
    // falsy while being an object. That cannot be reproduced here — Boa has no
    // `[[IsHTMLDDA]]` — so this returns the collection and *not* the falsiness,
    // which is the honest half: a page feature-detecting with it will take the
    // "modern browser" branch, which is the correct one for this engine.
    get all() { return collection(api.queryAll("*", 0).map(wrap), "HTMLCollection"); },

    get activeElement() { return wrap(api.body()); },
    get hidden() { return false; },
    get visibilityState() { return "visible"; },
  };

  // Same rule for `document`: a page reading `document.activeElement` or
  // `document.fonts` should produce a named gap, not a silent undefined.
  const document = observed(documentImpl, "document");

  const console = {
    log: (...a) => api.log("log", a.map(render).join(" ")),
    info: (...a) => api.log("info", a.map(render).join(" ")),
    warn: (...a) => api.log("warn", a.map(render).join(" ")),
    error: (...a) => api.log("error", a.map(render).join(" ")),
    debug: (...a) => api.log("debug", a.map(render).join(" ")),
  };

  // ── base64 and the legacy escapes ────────────────────────────────────────
  //
  // Named by the corpus once ReferenceErrors could name themselves. Small
  // enough that a stub reporting them as missing would cost more than the
  // implementation, and a page encoding a data: URI or a basic-auth header
  // fails outright without them.
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

  function btoa(input) {
    const text = String(input);
    let out = "";
    for (let i = 0; i < text.length; i += 3) {
      const a = text.charCodeAt(i);
      const b = text.charCodeAt(i + 1);
      const c = text.charCodeAt(i + 2);
      // Byte-oriented by definition: btoa on a code point above 255 throws in
      // a browser rather than mangling it, and a page that catches that is
      // entitled to the same answer here.
      if (a > 255 || (b === b && b > 255) || (c === c && c > 255)) {
        throw new TypeError("btoa: the string contains characters outside of Latin1");
      }
      const triple = (a << 16) | ((b || 0) << 8) | (c || 0);
      out += B64[(triple >> 18) & 63] + B64[(triple >> 12) & 63]
        + (Number.isNaN(b) ? "=" : B64[(triple >> 6) & 63])
        + (Number.isNaN(c) ? "=" : B64[triple & 63]);
    }
    return out;
  }

  function atob(input) {
    const text = String(input).replace(/[ \t\n\f\r]/g, "").replace(/=+$/, "");
    let out = "";
    let bits = 0;
    let held = 0;
    for (const ch of text) {
      const value = B64.indexOf(ch);
      if (value < 0) throw new TypeError("atob: the string is not valid base64");
      held = (held << 6) | value;
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out += String.fromCharCode((held >> bits) & 255);
      }
    }
    return out;
  }

  function escape(input) {
    return String(input).replace(/[^A-Za-z0-9@*_+\-./]/g, (ch) => {
      const code = ch.charCodeAt(0);
      return code < 256
        ? "%" + code.toString(16).toUpperCase().padStart(2, "0")
        : "%u" + code.toString(16).toUpperCase().padStart(4, "0");
    });
  }

  function unescape(input) {
    return String(input).replace(/%u([0-9a-fA-F]{4})|%([0-9a-fA-F]{2})/g, (_m, wide, byte) =>
      String.fromCharCode(parseInt(wide || byte, 16)));
  }

  // Defined rather than assigned, and the distinction is not cosmetic:
  // `Object.assign` invokes a getter and copies the value it returns, so a
  // scroll offset written that way freezes at whatever it was when the prelude
  // ran. Anything on the global object that changes over the life of the page
  // belongs here.
  //
  // These are also a gap the reporting proxy could never have found — nothing
  // wraps the global object, so `window.innerWidth` was simply undefined, and a
  // layout that measures instead of asking `matchMedia` got NaN out of its own
  // arithmetic.
  function defineLive(properties) {
    for (const [name, get] of Object.entries(properties)) {
      Object.defineProperty(globalThis, name, { get, configurable: true, enumerable: true });
    }
  }

  function render(v) {
    if (typeof v === "string") return v;
    if (v === null || v === undefined) return String(v);

    // An Error has no enumerable own properties, so `JSON.stringify` renders it
    // as `{}`. A page logging its own failures then fills the console with
    // hundreds of lines that say nothing — remix.run produced 1487 of them —
    // and the one thing an agent needed, the message, was the part thrown away.
    if (v instanceof Error || (typeof v?.message === "string" && typeof v?.name === "string")) {
      return v.stack ? `${v.name}: ${v.message}\n${v.stack}` : `${v.name}: ${v.message}`;
    }
    if (typeof v === "function") return `[function ${v.name || "anonymous"}]`;
    if (v instanceof Node) return v.outerHTML ?? String(v);

    try {
      const text = JSON.stringify(v);
      // `{}` for an object that plainly has contents means the contents were
      // not enumerable; say what it is rather than showing an empty shape.
      if (text === "{}" ) {
        const name = v.constructor?.name;
        return name && name !== "Object" ? `[${name}]` : String(v);
      }
      return text ?? String(v);
    } catch (_) {
      return String(v);
    }
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
    timers.set(id, { fn, due: clock + Math.max(0, delay | 0), args, every: null });
    return id;
  }
  function setInterval(fn, delay, ...args) {
    const id = nextTimer++;
    const every = Math.max(1, delay | 0);
    timers.set(id, { fn, due: clock + every, args, every });
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
      if (timer.every === null) timers.delete(id);
      else timer.due = clock + timer.every;
      try { timer.fn(...timer.args); } catch (error) {
        console.error("timer threw: " + error);
      }
      ran++;
    }
    return ran;
  };

  // Only one-shot timers count as work outstanding. An interval is perpetual by
  // definition, so waiting for the queue to drain would mean a page with a
  // polling loop — a clock, a carousel, an autosave — could never be described
  // as settled, and every snapshot of it would carry a "still busy" note that
  // told an agent nothing. Intervals still fire while the clock advances; they
  // just do not hold the page open.
  globalThis.__h5iPendingTimers = function () {
    let pending = 0;
    for (const timer of timers.values()) if (timer.every === null) pending++;
    return pending;
  };

  defineLive({
    innerWidth: () => api.viewport().width,
    innerHeight: () => api.viewport().height,
    outerWidth: () => api.viewport().width,
    outerHeight: () => api.viewport().height,
    scrollX: () => document.documentElement.scrollLeft,
    scrollY: () => document.documentElement.scrollTop,
    pageXOffset: () => document.documentElement.scrollLeft,
    pageYOffset: () => document.documentElement.scrollTop,
  });

  // How the host reaches a node it knows only by id, to fire a real event at
  // it. Exposed rather than reimplemented on the Rust side so a synthetic
  // click takes exactly the path a page's own `.click()` takes.
  globalThis.__h5iWrapById = wrap;

  // ── the rest of the window ───────────────────────────────────────────────

  // Every part, from the engine's own parser rather than from string surgery —
  // `pathname` came back undefined, and client-side routing is written against
  // exactly these. A second parser written in JavaScript would disagree with
  // the one that actually fetched the page about precisely the cases that
  // matter.
  // The address the *page* is at, which starts as the document URL and moves
  // with `history.pushState`. Held here rather than by writing to
  // `globalThis.__h5iUrl`, which the host defines read-only on purpose: that one
  // is the URL the broker actually fetched, and an engine whose record of what
  // it requested could be edited by the page would be worth nothing.
  let currentAddress = globalThis.__h5iUrl;

  function locationParts() {
    return api.parseUrl(String(currentAddress), "") || {};
  }
  const location = {
    get href() { return currentAddress; },
    get protocol() { return locationParts().protocol ?? ""; },
    get host() { return locationParts().host ?? ""; },
    get hostname() { return locationParts().hostname ?? ""; },
    get port() { return locationParts().port ?? ""; },
    get pathname() { return locationParts().pathname ?? ""; },
    get search() { return locationParts().search ?? ""; },
    get hash() { return locationParts().hash ?? ""; },
    get origin() { return locationParts().origin ?? ""; },
    toString() { return currentAddress; },
    assign(u) { api.unsupported("location.assign"); void u; },
    replace(u) { api.unsupported("location.replace"); void u; },
    reload() { api.unsupported("location.reload"); },
  };

  // Client-side routing goes through this, so a stub meant an SPA changed
  // nothing when it navigated. In memory, current entry plus a short list: the
  // page's own router reads `state` and listens for `popstate`, and both work.
  const entries = [{ state: null, url: globalThis.__h5iUrl }];
  let entryAt = 0;

  /// Resolve a pushed URL against the current one, the way a link would be.
  /// A router pushes `/page/2`, and storing that raw leaves an address no
  /// parser can answer questions about.
  function resolveEntry(url) {
    if (url === undefined || url === null || url === "") return entries[entryAt].url;
    const parts = api.parseUrl(String(url), String(currentAddress));
    return parts ? parts.href : String(url);
  }
  const history = {
    get length() { return entries.length; },
    get state() { return entries[entryAt].state ?? null; },
    pushState(state, _title, url) {
      const next = resolveEntry(url);
      entries.length = entryAt + 1;
      entries.push({ state: state ?? null, url: next });
      entryAt = entries.length - 1;
      // The address has to move with the entry, or `location.pathname` keeps
      // answering about the page the router already left — and a router that
      // reads its own route back gets the wrong one.
      currentAddress = next;
    },
    replaceState(state, _title, url) {
      const next = resolveEntry(url);
      entries[entryAt] = { state: state ?? null, url: next };
      currentAddress = next;
    },
    go(delta) {
      const next = entryAt + (delta | 0);
      if (next < 0 || next >= entries.length) return;
      entryAt = next;
      currentAddress = entries[entryAt].url;
      const event = new Event("popstate", { bubbles: false });
      event.state = entries[entryAt].state;
      dispatch(wrap(api.root()), event);
    },
    back() { history.go(-1); },
    forward() { history.go(1); },
  };

  // `now()` returns the *virtual* clock, deliberately: everything else in this
  // engine measures a page's own timeline rather than the wall, and a page that
  // computed a duration from a real clock would get a number about how loaded
  // this machine was.
  const performanceEntries = [];
  const performanceMarks = new Map();
  const performance = {
    now: () => clock,
    timeOrigin: 0,
    mark(name, options) {
      const at = options && typeof options.startTime === "number" ? options.startTime : clock;
      performanceMarks.set(String(name), at);
      const entry = { name: String(name), entryType: "mark", startTime: at, duration: 0 };
      performanceEntries.push(entry);
      return entry;
    },
    measure(name, startOrOptions, endMark) {
      const startName = typeof startOrOptions === "object" && startOrOptions !== null
        ? startOrOptions.start
        : startOrOptions;
      const start = performanceMarks.get(String(startName)) ?? 0;
      const end = endMark === undefined ? clock : (performanceMarks.get(String(endMark)) ?? clock);
      const entry = {
        name: String(name),
        entryType: "measure",
        startTime: start,
        duration: Math.max(0, end - start),
      };
      performanceEntries.push(entry);
      return entry;
    },
    getEntries() { return performanceEntries.slice(); },
    getEntriesByName(name, type) {
      return performanceEntries.filter(
        (e) => e.name === String(name) && (type === undefined || e.entryType === type),
      );
    },
    getEntriesByType(type) {
      return performanceEntries.filter((e) => e.entryType === String(type));
    },
    clearMarks(name) {
      if (name === undefined) performanceMarks.clear();
      else performanceMarks.delete(String(name));
    },
    clearMeasures() {},
    clearResourceTimings() {},
  };

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
  /// A `Blob` that actually holds its bytes.
  ///
  /// Pages build one to hand to `URL.createObjectURL`, to read back as text, or
  /// to measure. A stub would satisfy the constructor and then lie about `size`,
  /// which is the shape of bug this engine keeps having to remove.
  class Blob {
    constructor(parts, options) {
      // Bytes, not characters: `size` is a byte count, and a blob of "café" is
      // five bytes rather than four. Getting that wrong is the whole reason to
      // store the encoded form.
      const encoder = new TextEncoder();
      const chunks = [];
      for (const part of parts ?? []) {
        if (part instanceof Blob) chunks.push(...part._bytes);
        else if (part instanceof Uint8Array) chunks.push(...part);
        else if (part && part.buffer) chunks.push(...new Uint8Array(part.buffer));
        else chunks.push(...encoder.encode(String(part)));
      }
      this._bytes = chunks;
      this.type = String((options && options.type) || "");
    }
    get size() { return this._bytes.length; }
    text() { return Promise.resolve(new TextDecoder().decode(new Uint8Array(this._bytes))); }
    arrayBuffer() { return Promise.resolve(new Uint8Array(this._bytes).buffer); }
    bytes() { return Promise.resolve(new Uint8Array(this._bytes)); }
    slice(start, end, type) {
      const cut = new Blob([], { type: type ?? this.type });
      cut._bytes = this._bytes.slice(start, end);
      return cut;
    }
  }

  class File extends Blob {
    constructor(parts, name, options) {
      super(parts, options);
      this.name = String(name);
      this.lastModified = 0;
    }
  }

  /// The error type the platform throws, as distinct from a plain `Error`.
  ///
  /// Libraries construct it (`new DOMException('aborted', 'AbortError')`) and
  /// branch on `.name`, and an abort path that cannot build its own error
  /// throws a `ReferenceError` instead — which is how excalidraw's bundle died
  /// before rendering anything.
  class DOMException extends Error {
    constructor(message, name) {
      super(String(message ?? ""));
      this.name = String(name ?? "Error");
    }
    // The legacy numeric codes, which older code still compares against.
    get code() {
      return {
        IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4,
        InvalidCharacterError: 5, NoModificationAllowedError: 7, NotFoundError: 8,
        NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12,
        InvalidModificationError: 13, NamespaceError: 14, InvalidAccessError: 15,
        SecurityError: 18, NetworkError: 19, AbortError: 20, TimeoutError: 23,
        DataCloneError: 25,
      }[this.name] ?? 0;
    }
  }

  // ── text encoding, randomness, cloning, and the old request object ───────

  // UTF-8, written out rather than approximated. `escape`/`unescape` round
  // trips and `charCodeAt` truncation both get the common cases right and the
  // rest wrong, and "wrong only for non-Latin text" is the failure mode this
  // engine is least able to notice.
  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(input) {
      const text = String(input === undefined ? "" : input);
      const out = [];
      for (let i = 0; i < text.length; i++) {
        let code = text.codePointAt(i);
        if (code > 0xffff) i++; // a surrogate pair is one code point
        if (code < 0x80) {
          out.push(code);
        } else if (code < 0x800) {
          out.push(0xc0 | (code >> 6), 0x80 | (code & 63));
        } else if (code < 0x10000) {
          out.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 63), 0x80 | (code & 63));
        } else {
          out.push(
            0xf0 | (code >> 18),
            0x80 | ((code >> 12) & 63),
            0x80 | ((code >> 6) & 63),
            0x80 | (code & 63),
          );
        }
      }
      return new Uint8Array(out);
    }
  }

  class TextDecoder {
    constructor(label) { this._label = String(label || "utf-8").toLowerCase(); }
    get encoding() { return "utf-8"; }
    decode(input) {
      if (input === undefined || input === null) return "";
      // Anything byte-shaped: a typed array, an ArrayBuffer, or a plain array.
      const bytes = input instanceof Uint8Array
        ? input
        : new Uint8Array(input.buffer ? input.buffer : input);
      let out = "";
      for (let i = 0; i < bytes.length; ) {
        const byte = bytes[i];
        let code;
        let width;
        if (byte < 0x80) { code = byte; width = 1; }
        else if ((byte & 0xe0) === 0xc0) { code = byte & 31; width = 2; }
        else if ((byte & 0xf0) === 0xe0) { code = byte & 15; width = 3; }
        else if ((byte & 0xf8) === 0xf0) { code = byte & 7; width = 4; }
        else { out += "�"; i += 1; continue; }

        if (i + width > bytes.length) { out += "�"; break; }
        for (let k = 1; k < width; k++) {
          const cont = bytes[i + k];
          if ((cont & 0xc0) !== 0x80) { code = -1; break; }
          code = (code << 6) | (cont & 63);
        }
        // A truncated or overlong sequence becomes the replacement character,
        // which is what a decoder is specified to do rather than throwing.
        out += code < 0 ? "�" : String.fromCodePoint(code);
        i += width;
      }
      return out;
    }
  }

  const crypto = {
    getRandomValues(target) {
      if (!target || typeof target.length !== "number") {
        throw new TypeError("getRandomValues expects a typed array");
      }
      // Per element, not per byte: the caller's array decides the width.
      const width = target.BYTES_PER_ELEMENT || 1;
      const bytes = api.randomBytes(target.length * width);
      for (let i = 0; i < target.length; i++) {
        let value = 0;
        for (let b = 0; b < width; b++) value = value * 256 + bytes[i * width + b];
        target[i] = value;
      }
      return target;
    },
    randomUUID() {
      const bytes = api.randomBytes(16);
      // Version 4, variant 1 — the two fields a v4 UUID is defined by.
      bytes[6] = (bytes[6] & 0x0f) | 0x40;
      bytes[8] = (bytes[8] & 0x3f) | 0x80;
      const hex = bytes.map((b) => b.toString(16).padStart(2, "0"));
      return [
        hex.slice(0, 4).join(""),
        hex.slice(4, 6).join(""),
        hex.slice(6, 8).join(""),
        hex.slice(8, 10).join(""),
        hex.slice(10, 16).join(""),
      ].join("-");
    },
  };

  // A real deep clone. The JSON round trip this replaces silently dropped
  // `undefined`, turned a `Date` into a string, lost `Map` and `Set` entirely,
  // and threw on a cycle — every one of which reads as the page's own bug.
  function structuredClone(value, seen) {
    seen = seen || new Map();
    if (value === null || typeof value !== "object") return value;
    if (seen.has(value)) return seen.get(value);

    if (value instanceof Date) return new Date(value.getTime());
    if (value instanceof RegExp) return new RegExp(value.source, value.flags);
    if (Array.isArray(value)) {
      const out = [];
      seen.set(value, out);
      for (const item of value) out.push(structuredClone(item, seen));
      return out;
    }
    if (value instanceof Map) {
      const out = new Map();
      seen.set(value, out);
      for (const [k, v] of value) out.set(structuredClone(k, seen), structuredClone(v, seen));
      return out;
    }
    if (value instanceof Set) {
      const out = new Set();
      seen.set(value, out);
      for (const item of value) out.add(structuredClone(item, seen));
      return out;
    }
    // A node is not transferable, and pretending otherwise would hand the page
    // a detached copy that silently does nothing.
    if (value instanceof Node) {
      throw new TypeError("structuredClone cannot clone a DOM node");
    }
    const out = {};
    seen.set(value, out);
    for (const [k, v] of Object.entries(value)) out[k] = structuredClone(v, seen);
    return out;
  }

  // The old request object, over the same queue `fetch` uses — so an XHR is
  // policy-checked and receipted identically, and overlaps with everything else
  // in flight. Libraries that predate `fetch` are still everywhere.
  class XMLHttpRequest {
    constructor() {
      this.readyState = 0;
      this.status = 0;
      this.statusText = "";
      this.responseText = "";
      this.response = "";
      this.responseType = "";
      this.onreadystatechange = null;
      this.onload = null;
      this.onerror = null;
      this._method = "GET";
      this._url = "";
      this._headers = new Headers();
      this._responseHeaders = new Headers();
    }
    open(method, url, async) {
      // Synchronous XHR would have to block the one thread that owns the realm,
      // which would deadlock the loop that answers the request. Named rather
      // than silently upgraded to async, because a page relying on the return
      // value would read an empty response as an empty server.
      if (async === false) api.unsupported("XMLHttpRequest (synchronous)");
      this._method = String(method || "GET").toUpperCase();
      this._url = String(url);
      this._transition(1);
    }
    setRequestHeader(name, value) { this._headers.append(name, value); }
    getAllResponseHeaders() {
      let out = "";
      for (const [name, value] of this._responseHeaders) out += `${name}: ${value}\r\n`;
      return out;
    }
    getResponseHeader(name) { return this._responseHeaders.get(name); }
    abort() { this._aborted = true; this._transition(4); }
    _transition(state) {
      this.readyState = state;
      if (typeof this.onreadystatechange === "function") {
        try { this.onreadystatechange(); } catch (e) { console.error(`XHR onreadystatechange threw: ${e}`); }
      }
    }
    send(body) {
      fetch(this._url, { method: this._method, body, headers: this._headers })
        .then((response) => response.text().then((text) => ({ response, text })))
        .then(({ response, text }) => {
          if (this._aborted) return;
          this.status = response.status;
          this.statusText = response.statusText;
          this._responseHeaders = response.headers;
          this.responseText = text;
          this.response = this.responseType === "json" ? JSON.parse(text) : text;
          this._transition(4);
          if (typeof this.onload === "function") this.onload();
        })
        .catch((error) => {
          if (this._aborted) return;
          this.status = 0;
          this._transition(4);
          if (typeof this.onerror === "function") this.onerror(error);
          else console.error(`XMLHttpRequest failed: ${error}`);
        });
    }
  }

  Object.assign(globalThis, {
    addEventListener, removeEventListener, dispatchEvent,
    window,
    document,
    console,
    // Same reporting rule as `document`: a method missing from one of these was
    // invisible, because only the document and its nodes were watched. A module
    // failing with "not a callable function" and naming nothing is the failure
    // §8.3 exists to prevent, and these are where the remaining ones hid.
    location: observed(location, "location"),
    history: observed(history, "history"),
    performance: observed(performance, "performance"),
    setTimeout, clearTimeout,
    setInterval, clearInterval: clearTimeout,
    requestAnimationFrame: (fn) => setTimeout(() => fn(clock), 16),
    cancelAnimationFrame: clearTimeout,
    Node, Element, Text, Event,
    alert: () => api.unsupported("alert"),
    matchMedia,
    URL, URLSearchParams,
    queueMicrotask: (fn) => { Promise.resolve().then(fn); },
    structuredClone: (value) => structuredClone(value),
    requestIdleCallback: (fn) => setTimeout(() => fn({ didTimeout: false, timeRemaining: () => 0 }), 1),
    cancelIdleCallback: clearTimeout,
    navigator: observed({
      // From the host, not a second copy: a page that branches on the agent
      // server-side and again in script must see the same string both times,
      // or it renders for one engine and scripts for another.
      userAgent: api.userAgent(),
      // Every browser answers "Netscape" here, and `appVersion` is the agent
      // string with its product token removed. Both are derived from the one
      // constant rather than written again, so they cannot drift from it.
      appName: "Netscape",
      appVersion: api.userAgent().replace(/^Mozilla\//, ""),
      appCodeName: "Mozilla",
      product: "Gecko",
      vendor: "",
      platform: "", language: "en-US", languages: ["en-US"],
      onLine: true, cookieEnabled: false, maxTouchPoints: 0,
      hardwareConcurrency: 1,
      // False, and true: this is not a driven browser in the WebDriver sense.
      // A page fingerprinting for automation gets the same answer a person's
      // browser gives, because the answer is not about who is asking.
      webdriver: false,
      // `userAgentData` and `scheduling` are deliberately *not* declared here.
      // Writing `userAgentData: undefined` would make `'userAgentData' in
      // navigator` answer true, which is the same lie the `missingApi` stubs
      // told: a page checking before using would take the branch for an API
      // that is not there. Left absent, they behave as they do in Firefox, and
      // the reporting proxy still names them.
    }, "navigator"),
    // Named rather than absent. A page reaching for these gets a message that
    // says which API it wanted, and the name reaches the snapshot.
    // `self` is `window` under another name, and worker-shaped code reaches for
    // it first. It has to be the same object, not a copy, or a page that stores
    // state on one and reads it from the other loses it.
    get self() { return globalThis; },

    devicePixelRatio: 1,
    scrollTo(x, y) {
      const top = typeof x === "object" && x !== null ? x.top : y;
      api.setScrollTop(api.root(), Number(top) || 0);
    },
    scroll(x, y) { globalThis.scrollTo(x, y); },
    scrollBy(x, y) {
      const by = typeof x === "object" && x !== null ? x.top : y;
      api.setScrollTop(api.root(), document.documentElement.scrollTop + (Number(by) || 0));
    },
    btoa, atob, escape, unescape,

    // Boa defines neither. `reportError` is how a library hands an error to the
    // page's own handler rather than swallowing it, and this engine's console
    // is exactly where that should land.
    reportError: (error) => api.log("error", `reported: ${render(error)}`),
    // Nothing here is ever collected during a page's life, so a registry that
    // never fires its callback is the honest shape rather than a refusal: a
    // page registering one is not asking for anything it will notice missing.
    FinalizationRegistry: class FinalizationRegistry {
      constructor(callback) { this._callback = callback; }
      register() {}
      unregister() { return false; }
    },

    // The constructors, exposed for `instanceof` — which is how library code
    // asks "is this a node?" before deciding what to do with it. `HTMLElement`
    // is `Element` here because this engine has one element class; the check
    // that matters is the one pages actually write.
    Node, Element, Text, Comment, CharacterData, DocumentFragment, DOMTokenList, Range,
    EventTarget, DOMParser,
    HTMLElement: Element,
    // The per-tag constructors, which pages use two ways: `instanceof
    // HTMLAnchorElement` to ask what they are holding, and `extends
    // HTMLButtonElement` to build on one. Every one is `Element` because this
    // engine has a single element class, so `instanceof` answers "is this an
    // element" rather than "is this a button" — a coarser answer than a browser
    // gives, and a far better one than `ReferenceError`, which is what these
    // were and which took whole bundles down with them.
    ...Object.fromEntries(
      [
        "Anchor", "Area", "Audio", "Base", "Body", "BR", "Button", "Canvas", "Data",
        "DataList", "Details", "Dialog", "Div", "DList", "Embed", "FieldSet", "Form",
        "Head", "Heading", "HR", "Html", "IFrame", "Image", "Input", "Label", "Legend",
        "LI", "Link", "Map", "Media", "Menu", "Meta", "Meter", "Mod", "Object", "OList",
        "OptGroup", "Option", "Output", "Paragraph", "Param", "Picture", "Pre",
        "Progress", "Quote", "Script", "Select", "Slot", "Source", "Span", "Style",
        "Table", "TableCaption", "TableCell", "TableCol", "TableRow", "TableSection",
        "Template", "TextArea", "Time", "Title", "Track", "UList", "Unknown", "Video",
      ].map((name) => [`HTML${name}Element`, Element]),
    ),
    SVGElement: Element,
    CharacterData: Text,
    customElements, NodeFilter, NodeIterator, TreeWalker,

    crypto: observed(crypto, "crypto"),
    TextEncoder, TextDecoder, XMLHttpRequest, Blob, File, DOMException,
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
    IntersectionObserver, ResizeObserver,
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

    // Handed to the host and answered later. The whole point of the ticket is
    // that two calls to `fetch` overlap: the old binding did the round trip
    // inline, so a page that fanned out ten requests paid for them in series
    // and every SPA waterfall was ours rather than the site's.
    const id = api.fetchStart(request.url, request.method, body);
    return new Promise((resolve, reject) => {
      pendingFetches.set(id, { resolve, reject, request, signal });
    });
  }
  globalThis.fetch = fetch;

  const pendingFetches = new Map();

  function responseFrom(res, request) {
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
    return response;
  }

  // Driven by the settle loop, so a page's promises resolve as the network
  // answers rather than at some arbitrary later point. Returns how much is
  // still owed, which is what tells `settle` there is real work outstanding.
  globalThis.__h5iDrainFetches = function () {
    for (const [id, res] of api.fetchDrain()) {
      const waiting = pendingFetches.get(id);
      if (!waiting) continue;
      pendingFetches.delete(id);
      if (waiting.signal && waiting.signal.aborted) {
        waiting.reject(waiting.signal.reason ?? new Error("aborted"));
      } else if (res.error) {
        waiting.reject(new Error(res.error));
      } else {
        waiting.resolve(responseFrom(res, waiting.request));
      }
    }
    return api.fetchPending();
  };

  // Everything still owed an answer when the page ran out of budget. Rejecting
  // is the honest end: a promise left pending forever is a page that looks like
  // it is still working when nothing is.
  globalThis.__h5iAbandonFetches = function (why) {
    for (const [, waiting] of pendingFetches) waiting.reject(new Error(why));
    pendingFetches.clear();
  };

  // In memory and nowhere else. A disposable box has no business writing a
  // page's storage to a filesystem, and "restart the session" is a complete
  // clear — the same rule the cookie jar follows.
  function makeStorage() {
    const map = new Map();
    const api = {
      getItem(k) { const v = map.get(String(k)); return v === undefined ? null : v; },
      setItem(k, v) { map.set(String(k), String(v)); },
      removeItem(k) { map.delete(String(k)); },
      clear() { map.clear(); },
      key(i) { return [...map.keys()][i] ?? null; },
      get length() { return map.size; },
    };

    // `storage.theme` is not a property that might be missing — it *is* the
    // Storage API for a key, and it reads and writes the same map `getItem`
    // does. Watching this object with the reporting proxy therefore turned
    // every key any page ever read into a "missing API": the document corpus
    // listed `localStorage.currentTheme` and `sessionStorage.sveltekit:scroll`
    // as gaps in this engine, which would have buried the real ones.
    //
    // So it is a proxy that implements the named-property access instead of
    // reporting it. The methods win over keys, as they do in a browser — a page
    // storing something under "getItem" gets it back through `getItem("getItem")`.
    return new Proxy(api, {
      get(target, key) {
        if (typeof key === "symbol" || key in target) return Reflect.get(target, key);
        const value = map.get(String(key));
        return value === undefined ? undefined : value;
      },
      set(target, key, value) {
        if (typeof key === "symbol" || key in target) return Reflect.set(target, key, value);
        map.set(String(key), String(value));
        return true;
      },
      has(target, key) {
        return key in target || map.has(String(key));
      },
      deleteProperty(target, key) {
        if (key in target) return Reflect.deleteProperty(target, key);
        map.delete(String(key));
        return true;
      },
      ownKeys() { return [...map.keys()]; },
      getOwnPropertyDescriptor(target, key) {
        if (key in target) return Reflect.getOwnPropertyDescriptor(target, key);
        if (!map.has(String(key))) return undefined;
        return { value: map.get(String(key)), writable: true, enumerable: true, configurable: true };
      },
    });
  }
})();
