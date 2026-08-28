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

  /// Tag name to the interface that tag gets, filled in once the
  /// interfaces below exist. A Map declared here rather than beside them
  /// because `constructElement` is defined above and would otherwise read
  /// a `const` still in its temporal dead zone.
  const TAG_CLASSES = new Map();

  /// Which node has focus, or null for none.
  ///
  /// Held here rather than on the tree because focus is a property of the
  /// *document's* view of itself, not of a node, and two nodes must not be able
  /// to believe they both have it.
  let focusedId = null;

  /// An error with the line it came from, for the callbacks that swallow one.
  ///
  /// Every `catch` that reports and carries on — a listener, a timer, an
  /// observer — is by definition detached from the code that scheduled it, so
  /// the message is all the reader gets and a message without a location sends
  /// them looking through the whole page. These are exactly the errors that can
  /// least afford to be anonymous, and they were the ones reporting the least.
  function withStack(error) {
    return String(error) + (error && error.stack ? "\n" + error.stack : "");
  }

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
    const tag = api.tagName(id).toLowerCase();
    const definition = definitions.get(tag);
    if (!definition) return new (TAG_CLASSES.get(tag) ?? Element)(id);

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
      console.error(`custom element connectedCallback threw: ${withStack(error)}`);
    }
  }

  function fireDisconnected(node) {
    if (!connected.has(node._id)) return;
    connected.delete(node._id);
    try {
      if (typeof node.disconnectedCallback === "function") node.disconnectedCallback();
    } catch (error) {
      console.error(`custom element disconnectedCallback threw: ${withStack(error)}`);
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
      console.error(`custom element attributeChangedCallback threw: ${withStack(error)}`);
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
          console.error(`a ${event.type} listener threw: ${withStack(error)}`);
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
    get childNodes() {
      // A `<template>`'s children belong to its `content` fragment, not to the
      // element: `tp.childNodes.length` is 0 in a browser however much markup
      // it holds. This engine keeps one node for both, so the element hides
      // them and `TemplateContent` shows them — which is the division the spec
      // describes anyway. Without it a walker that recurses into a template
      // reads markup the page has not rendered and may never render.
      if (this.tagName === "TEMPLATE") return [];
      return api.children(this._id).map(wrap);
    }
    /// The single most-asked-for thing this engine did not have.
    ///
    /// WPT called it 3,944 times across the corpus, more than twice anything
    /// else on the list, and it is one line. It went missing because nothing in
    /// four hand-picked corpora used it and everything in the DOM test suite
    /// does — which is the argument for running a conformance suite in one
    /// sentence.
    hasChildNodes() { return api.children(this._id).length > 0; }

    /// Same type, same name, same attributes, same children — not the same node.
    isEqualNode(other) {
      if (!other) return false;
      if (this.nodeType !== other.nodeType) return false;
      if (this.nodeType === 3 || this.nodeType === 8) return this.data === other.data;
      if (this.tagName !== other.tagName) return false;
      const mine = api.attrNames(this._id) || [];
      const theirs = api.attrNames(other._id) || [];
      if (mine.length !== theirs.length) return false;
      for (const name of mine) {
        if (api.getAttr(this._id, name) !== api.getAttr(other._id, name)) return false;
      }
      const a = this.childNodes, b = other.childNodes;
      if (a.length !== b.length) return false;
      for (let i = 0; i < a.length; i++) if (!a[i].isEqualNode(b[i])) return false;
      return true;
    }
    isSameNode(other) { return !!other && other._id === this._id; }

    /// Merge adjacent text nodes and drop empty ones.
    normalize() {
      const kids = this.childNodes;
      let previous = null;
      for (const kid of kids) {
        if (kid.nodeType === 3) {
          if (kid.data === "") { kid.remove(); continue; }
          if (previous) { previous.data += kid.data; kid.remove(); continue; }
          previous = kid;
        } else {
          previous = null;
          if (kid.nodeType === 1) kid.normalize();
        }
      }
    }

    /// Where `other` sits relative to this node, as the spec's bit field.
    compareDocumentPosition(other) {
      if (!other) return 1;
      if (other._id === this._id) return 0;
      const DISCONNECTED = 1, PRECEDING = 2, FOLLOWING = 4, CONTAINS = 8, CONTAINED = 16;
      const ancestors = (node) => { const out = []; for (let n = node; n; n = n.parentNode) out.push(n); return out; };
      const mine = ancestors(this), theirs = ancestors(other);
      if (theirs.some((n) => n._id === this._id)) return FOLLOWING | CONTAINED;
      if (mine.some((n) => n._id === other._id)) return PRECEDING | CONTAINS;
      // Nearest common ancestor, then compare the branches under it.
      const common = mine.find((a) => theirs.some((b) => b._id === a._id));
      if (!common) return DISCONNECTED | PRECEDING;
      const branchOf = (chain) => chain[chain.findIndex((n) => n._id === common._id) - 1];
      const a = branchOf(mine), b = branchOf(theirs);
      const kids = common.childNodes;
      let seenA = -1, seenB = -1;
      for (let i = 0; i < kids.length; i++) {
        if (a && kids[i]._id === a._id) seenA = i;
        if (b && kids[i]._id === b._id) seenB = i;
      }
      return seenA < seenB ? FOLLOWING : PRECEDING;
    }
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
      // Nullable by spec: `el.textContent = null` empties the element rather
      // than writing the four characters "null".
      api.setText(this._id, value === null || value === undefined ? "" : String(value));
      if (observers.length === 0) return;
      record({
        type: "characterData", target: this, addedNodes: [], removedNodes: [],
        attributeName: null, oldValue: null,
      });
      childListRecord(this, [], []);
    }

    /// The text as *rendered*, which is what separates it from `textContent`.
    ///
    /// Two differences, and both are the reason pages reach for this one:
    /// content in a `display: none` subtree is not in it, and block boundaries
    /// become line breaks. A page that reads `innerText` to find out what the
    /// user can see gets a different and better answer than `textContent`,
    /// which would hand back the contents of every hidden menu on the page.
    ///
    /// Walked natively. As a JavaScript walk this cost 142ms on a 6,000-node
    /// page against `textContent`'s 6ms, because every level built an array of
    /// wrapped nodes and every read paid a proxy trap.
    get innerText() { return api.innerText(this._id); }
    set innerText(value) { this.textContent = String(value); }

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
      // The spec's pre-insert step, and not a formality: without it the anchor
      // reaches blitz, which inserts relative to the anchor's parent and
      // unwraps it. A caller that passes a node from somewhere else is asking
      // for a NotFoundError and was getting a dead process.
      if (anchor.parentNode !== this) {
        throw new DOMException(
          "insertBefore: the reference node is not a child of this node",
          "NotFoundError",
        );
      }
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
      const once = !!(options && options.once);
      // The same handler registered twice for the same type and phase is one
      // listener, as in a browser. Without this a page that re-runs its own
      // setup accumulates duplicates and every event fires N times.
      const already = listeners.some(
        (l) => l.id === this._id && l.type === String(type)
          && l.handler === handler && l.capture === capture,
      );
      if (already) return;
      listeners.push({ id: this._id, type: String(type), handler, capture, once });
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
      // `el.setAttribute("onclick", ...)` is the same handler the parser would
      // have compiled, arriving by another road.
      if (HANDLER_ATTR_SET.has(String(name).toLowerCase())) {
        const lowered = String(name).toLowerCase();
        const installed = this.__h5iInline ?? (this.__h5iInline = {});
        installed[lowered] = String(value);
        installInlineHandler(this, lowered, String(value));
      }
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
    /// Always null, and that is the answer rather than a gap: this engine
    /// parses HTML and nothing else (`document.contentType` says so), and every
    /// element in an HTML document is in the HTML namespace with no prefix.
    /// It was being *reported* as missing, which is the reverse of the mistake
    /// the reporting proxy exists to catch — naming as absent something no
    /// browser would answer differently.
    get prefix() { return null; }

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

    get options() { return this.querySelectorAll("option"); }

    // Real serialisation. Returning textContent here silently stripped every
    // tag, so `el.innerHTML = el.innerHTML` destroyed the subtree.
    get innerHTML() { return api.innerHtml(this._id); }
    set innerHTML(html) {
      api.setInnerHtml(this._id, String(html));
      // Markup written after load carries handlers too, and the lifecycle
      // sweep has already been and gone by the time a page does this.
      globalThis.__h5iInstallInlineHandlers(this);
    }

    /// `innerHTML`, plus the one thing `innerHTML` is specified *not* to do:
    /// turn `<template shadowrootmode>` into a real shadow root.
    ///
    /// That difference is the entire reason the method exists, and it is how a
    /// server-rendered component ships its shadow DOM as markup — so an engine
    /// that aliased this to `innerHTML` would leave every declarative component
    /// as an inert `<template>` that renders nothing.
    ///
    /// "Unsafe" is the spec's word for "does not sanitise", which is the same
    /// contract `innerHTML` already has here. It is not a new hazard: page
    /// markup reaching an agent is fenced as untrusted either way.
    setHTMLUnsafe(html) {
      api.setInnerHtml(this._id, String(html));
      adoptDeclarativeShadowRoots(this);
      globalThis.__h5iInstallInlineHandlers(this);
    }
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
    get style() { return new StyleDeclaration(inlineStyleSource(this)); }
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

    querySelector(sel) { return wrap(api.query(checkSelector(sel), this._id)); }
    querySelectorAll(sel) { return collection(api.queryAll(checkSelector(sel), this._id).map(wrap)); }
    getElementsByTagName(tag) { return collection(api.queryAll(String(tag), this._id).map(wrap), "HTMLCollection"); }
    getElementsByClassName(cls) { return collection(api.queryAll("." + String(cls), this._id).map(wrap), "HTMLCollection"); }

    matches(sel) { return api.matchesSelector(this._id, checkSelector(sel)); }
    closest(sel) {
      for (let n = this; n; n = n.parentNode) {
        if (n.nodeType === 1 && n.matches(sel)) return n;
      }
      return null;
    }

    /// The placement rule the three `insertAdjacent*` methods share.
    ///
    /// They differ only in what they are handed — parsed markup, a text node,
    /// an element — so the four positions are worked out once here. Returns
    /// whether anything was inserted: `beforebegin` and `afterend` on a node
    /// with no parent are a no-op by spec rather than an error, and
    /// `insertAdjacentElement` has to return null for exactly that case.
    _insertAdjacent(position, nodes) {
      const where = String(position).toLowerCase();
      if (where === "beforeend") {
        for (const node of nodes) this.appendChild(node);
        return true;
      }
      if (where === "afterbegin") {
        const first = this.firstChild;
        for (const node of nodes) first ? this.insertBefore(node, first) : this.appendChild(node);
        return true;
      }
      if (where === "beforebegin" || where === "afterend") {
        const parent = this.parentNode;
        if (!parent) return false;
        if (where === "beforebegin") {
          for (const node of nodes) parent.insertBefore(node, this);
          return true;
        }
        const next = this.nextSibling;
        for (const node of nodes) next ? parent.insertBefore(node, next) : parent.appendChild(node);
        return true;
      }
      // A DOMException rather than the TypeError this used to throw: the spec
      // names this one, and a caller catching by type should find what the spec
      // told it to expect.
      throw new DOMException(
        "not one of beforebegin, afterbegin, beforeend, afterend: " + position,
        "SyntaxError",
      );
    }
    insertAdjacentHTML(position, html) {
      const host = document.createElement("div");
      api.setInnerHtml(host._id, String(html));
      this._insertAdjacent(position, [...host.childNodes]);
      host.remove();
    }
    insertAdjacentText(position, text) {
      this._insertAdjacent(position, [document.createTextNode(String(text))]);
    }
    insertAdjacentElement(position, element) {
      return this._insertAdjacent(position, [element]) ? element : null;
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
    /// Move focus here, and fire what a browser fires.
    ///
    /// Both were empty, so `document.activeElement` never moved: a page that
    /// focused a field and then checked which field was focused got the wrong
    /// answer, and a form that advances focus as it validates got no signal at
    /// all. `focusin`/`focusout` bubble and `focus`/`blur` do not, which is the
    /// difference delegation depends on.
    focus() {
      if (focusedId === this._id) return;
      const previous = focusedId === null ? null : wrap(focusedId);
      focusedId = this._id;
      if (previous) {
        previous.dispatchEvent(new Event("blur", { bubbles: false }));
        previous.dispatchEvent(new Event("focusout", { bubbles: true }));
      }
      this.dispatchEvent(new Event("focus", { bubbles: false }));
      this.dispatchEvent(new Event("focusin", { bubbles: true }));
    }
    blur() {
      if (focusedId !== this._id) return;
      focusedId = null;
      this.dispatchEvent(new Event("blur", { bubbles: false }));
      this.dispatchEvent(new Event("focusout", { bubbles: true }));
    }

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
    /// Position relative to `offsetParent`, which for this engine is the page.
    ///
    /// A full implementation walks up for the nearest positioned ancestor and
    /// subtracts its border box. That is a real difference on a positioned
    /// subtree, and it is written down here rather than left to be discovered:
    /// what these return is the offset from the document, which is what
    /// `offsetParent` being the body means.
    get offsetTop() { return this.getBoundingClientRect().top; }
    get offsetLeft() { return this.getBoundingClientRect().left; }
    get offsetParent() {
      // Null for an element that is not rendered, which is the one case code
      // actually branches on.
      const display = api.computedStyle(this._id, "display") || "";
      if (display === "none" || !this.isConnected) return null;
      return wrap(api.query("body", 0));
    }
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
    setHTMLUnsafe(html) {
      api.setInnerHtml(this._id, String(html));
      adoptDeclarativeShadowRoots(this);
      this._project();
    }
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
    // The one place a template's children *are* visible. `Element` hides them
    // for the element itself, per the rule below, and this is the fragment
    // they officially belong to.
    get childNodes() { return api.children(this._id).map(wrap); }
    get children() { return collection(this.childNodes.filter((n) => n.nodeType === 1), "HTMLCollection"); }
  }

  // `el.onclick = fn` is the other way to bind a handler, and plenty of
  // generated code still uses it. Defined rather than enumerated by hand so the
  // property and `addEventListener` cannot disagree about what is registered.
  // Attributes that are nothing but a string on the element. Defined from a
  // table rather than written out twice, so a getter can never exist without
  // its setter — which is the bug this table was written to end: in a module,
  // which is strict, assigning to a getter-only property *throws*, and a
  // classic-script test cannot see it because sloppy mode swallows it.
  /// The content attributes every element reflects, because they are global.
  ///
  /// It used to hold twelve more, and they were not global: `htmlFor`, `rel`,
  /// `target`, `charset`, `crossOrigin` and the rest belong to particular
  /// interfaces, and defining them here put them on *every* element. A page
  /// asking `"htmlFor" in element` — which is how the platform is feature
  /// detected, and what WPT's reflection helper gates on — was told yes for
  /// `<div>`, `<button>`, and everything else.
  ///
  /// That is not a cosmetic wrong answer. The helper takes it as licence to
  /// test a property the element should not have, so one file went from 209
  /// subtests passing to 330 failing: the engine had claimed a surface it does
  /// not implement, and was then measured against it.
  ///
  /// The interface table below is the right owner for the rest, and already
  /// declared all but three of them. `dir`, `slot` and `accessKey` stay because
  /// they really do belong to every element.
  const REFLECTED_ATTRIBUTES = {
    dir: "dir",
    slot: "slot",
    accessKey: "accesskey",
  };  for (const [property, attribute] of Object.entries(REFLECTED_ATTRIBUTES)) {
    Object.defineProperty(Element.prototype, property, {
      configurable: true,
      get() { return api.getAttr(this._id, attribute) || ""; },
      set(value) { this.setAttribute(attribute, value); },
    });
  }

  /// Reflect an IDL property onto a content attribute, with the *type* the
  /// spec gives it.
  ///
  /// The type is not decoration, and this mechanism exists because passing the
  /// string through looked like it worked. `dir` is an enumerated attribute:
  /// its IDL getter answers `""` for anything that is not one of its keywords,
  /// so `el.setAttribute("dir", "5%")` reads back as `""` in a browser and read
  /// back as `"5%"` here. WPT sets every reflected attribute to sixty-odd
  /// hostile values and checks exactly that, which is how an engine can score
  /// zero on an attribute it believed it supported.
  ///
  /// One definition per shape rather than a hand-written getter and setter per
  /// attribute, because there are a great many of them and the interesting part
  /// is the conversion, not the plumbing.
  function reflect(proto, idl, content, type = "string", options = {}) {
    const parseInteger = (raw) => {
      // The spec's rules for parsing integers, which are not `Number()`:
      // leading whitespace is skipped, trailing garbage ends the number, and
      // anything else is a failure rather than a NaN to paper over.
      const match = /^[ \t\n\f\r]*([+-]?[0-9]+)/.exec(raw ?? "");
      if (!match) return null;
      const value = Number(match[1]);
      return Number.isSafeInteger(value) ? value : null;
    };
    const get = {
      string() { return api.getAttr(this._id, content) ?? ""; },
      // Nullable, unlike a plain DOMString: the ARIA properties report `null`
      // for an attribute that is absent rather than an empty string, and a test
      // that distinguishes the two is testing something real.
      nullable() { return api.getAttr(this._id, content); },
      bool() { return api.getAttr(this._id, content) !== null; },
      long() {
        const value = parseInteger(api.getAttr(this._id, content));
        return value === null ? (options.default ?? 0) : value;
      },
      ulong() {
        const value = parseInteger(api.getAttr(this._id, content));
        if (value === null || value < 0) return options.default ?? 0;
        return value;
      },
      enumerated() {
        const raw = api.getAttr(this._id, content);
        // `in` rather than `??`, because `null` is a real missing-value default
        // — `crossOrigin` reports null for an absent attribute — and `??` would
        // quietly turn that into "".
        if (raw === null) return "missing" in options ? options.missing : "";
        const lower = String(raw).toLowerCase();
        // Aliases first: a keyword can have more than one spelling that maps to
        // the same state, and the empty string is the one that matters —
        // `<div contenteditable>` is the "true" state, so an implementation
        // that only matched the literal keywords reported "inherit" for the
        // most common way anyone writes it.
        if (options.aliases && lower in options.aliases) return options.aliases[lower];
        const found = options.keywords.find((word) => word.toLowerCase() === lower);
        if (found !== undefined) return found;
        return "invalid" in options ? options.invalid : "";
      },
      url() {
        const raw = api.getAttr(this._id, content);
        if (raw === null) return "";
        // `currentAddress`, matching `_resolved` above: only Document carries a
        // `baseURI`, and resolving against `undefined` would hand back the raw
        // attribute for every relative URL while looking like it resolved.
        const parts = api.parseUrl(String(raw), currentAddress);
        // An unparseable URL reflects as the literal attribute, which is what a
        // browser does and is more useful than an empty string when debugging.
        return parts ? parts.href : String(raw);
      },
    }[type];
    const set = type === "bool"
      ? function (on) {
        if (on) this.setAttribute(content, "");
        else this.removeAttribute(content);
      }
      : type === "long" || type === "ulong"
        ? function (value) {
          const number = Number(value);
          let written = Number.isFinite(number) ? Math.trunc(number) : 0;
          // An unsigned reflection cannot hold a negative, and writing one
          // anyway left `td.colSpan = -3` reading back as 1 while
          // `getAttribute("colspan")` said "-3" — the property and the
          // attribute disagreeing about the same element.
          if (type === "ulong" && written < 0) written = options.default ?? 0;
          this.setAttribute(content, String(written));
        }
        : function (value) {
          // `null` on a nullable reflection removes the attribute; everywhere
          // else it stringifies, so `el.dir = null` really does write "null".
          if (value === null && type === "nullable") this.removeAttribute(content);
          else this.setAttribute(content, String(value));
        };
    Object.defineProperty(proto, idl, { configurable: true, get, set });
  }

  // The attributes every HTML element carries. `hidden` and `tabIndex` were
  // the only two of these that existed, hand-written, before WPT was pointed
  // at the engine.
  reflect(Element.prototype, "hidden", "hidden", "bool");
  reflect(Element.prototype, "autofocus", "autofocus", "bool");
  // `tabIndex` has no single default: an element the user can reach with the
  // keyboard reports 0, everything else -1. Answering -1 for a link or a button
  // tells a page nothing is focusable, which is the opposite of true.
  const FOCUSABLE_BY_DEFAULT = new Set([
    "A", "AREA", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "IFRAME", "OBJECT",
    "SUMMARY", "AUDIO", "VIDEO",
  ]);
  Object.defineProperty(Element.prototype, "tabIndex", {
    configurable: true,
    get() {
      const raw = api.getAttr(this._id, "tabindex");
      if (raw !== null) {
        const match = /^[ \t\n\f\r]*([+-]?[0-9]+)/.exec(raw);
        if (match) return Number(match[1]);
      }
      if (!FOCUSABLE_BY_DEFAULT.has(this.tagName)) return -1;
      // A link is only focusable if it actually links somewhere.
      if ((this.tagName === "A" || this.tagName === "AREA")
        && api.getAttr(this._id, "href") === null) return -1;
      return 0;
    },
    set(value) { this.setAttribute("tabindex", String(Math.trunc(Number(value)) || 0)); },
  });
  reflect(Element.prototype, "accessKey", "accesskey");
  reflect(Element.prototype, "slot", "slot");
  reflect(Element.prototype, "nonce", "nonce");
  reflect(Element.prototype, "dir", "dir", "enumerated", { keywords: ["ltr", "rtl", "auto"] });
  reflect(Element.prototype, "contentEditable", "contenteditable", "enumerated", {
    keywords: ["true", "false", "plaintext-only"],
    aliases: { "": "true" },
    missing: "inherit",
    invalid: "inherit",
  });
  reflect(Element.prototype, "autocapitalize", "autocapitalize", "enumerated", {
    keywords: ["none", "off", "on", "sentences", "words", "characters"],
  });
  reflect(Element.prototype, "inputMode", "inputmode", "enumerated", {
    keywords: ["none", "text", "tel", "url", "email", "numeric", "decimal", "search"],
  });
  reflect(Element.prototype, "enterKeyHint", "enterkeyhint", "enumerated", {
    keywords: ["enter", "done", "go", "next", "previous", "search", "send"],
  });
  reflect(Element.prototype, "popover", "popover", "enumerated", {
    keywords: ["auto", "manual"], invalid: "manual",
  });

  // ARIA, which reflects mechanically: every one of these is `aria-` followed
  // by the rest of the name lowercased, with no word separator —
  // `ariaHasPopup` is `aria-haspopup`, not `aria-has-popup`. Written as a list
  // rather than as forty pairs because the mapping has no exceptions.
  for (const name of [
    "ariaAtomic", "ariaAutoComplete", "ariaBrailleLabel", "ariaBrailleRoleDescription",
    "ariaBusy", "ariaChecked", "ariaColCount", "ariaColIndex", "ariaColIndexText",
    "ariaColSpan", "ariaCurrent", "ariaDescription", "ariaDisabled", "ariaExpanded",
    "ariaHasPopup", "ariaHidden", "ariaInvalid", "ariaKeyShortcuts", "ariaLabel",
    "ariaLevel", "ariaLive", "ariaModal", "ariaMultiLine", "ariaMultiSelectable",
    "ariaOrientation", "ariaPlaceholder", "ariaPosInSet", "ariaPressed", "ariaReadOnly",
    "ariaRelevant", "ariaRequired", "ariaRoleDescription", "ariaRowCount", "ariaRowIndex",
    "ariaRowIndexText", "ariaRowSpan", "ariaSelected", "ariaSetSize", "ariaSort",
    "ariaValueMax", "ariaValueMin", "ariaValueNow", "ariaValueText",
  ]) {
    reflect(Element.prototype, name, "aria-" + name.slice(4).toLowerCase(), "nullable");
  }
  reflect(Element.prototype, "role", "role", "nullable");

  // The ARIA properties that are *enumerated* rather than free strings. Their
  // getters answer a canonical keyword and reject anything else, exactly as
  // `dir` does — declaring them as plain strings passed every hostile value
  // straight back and cost roughly six hundred subtests in one file.
  //
  // `missing: null` on purpose: an absent ARIA attribute reflects as null, and
  // an invalid one as the empty string. Three states, not two.
  for (const [name, keywords] of [
    ["ariaAutoComplete", ["inline", "list", "both", "none"]],
    ["ariaChecked", ["true", "false", "mixed", "undefined"]],
    ["ariaCurrent", ["page", "step", "location", "date", "time", "true", "false"]],
    ["ariaDisabled", ["true", "false"]],
    ["ariaExpanded", ["true", "false", "undefined"]],
    ["ariaHasPopup", ["false", "true", "menu", "listbox", "tree", "grid", "dialog"]],
    ["ariaHidden", ["true", "false", "undefined"]],
    ["ariaInvalid", ["grammar", "false", "spelling", "true"]],
    ["ariaLive", ["assertive", "off", "polite"]],
    ["ariaModal", ["true", "false"]],
    ["ariaMultiLine", ["true", "false"]],
    ["ariaMultiSelectable", ["true", "false"]],
    ["ariaOrientation", ["horizontal", "vertical", "undefined"]],
    ["ariaPressed", ["true", "false", "mixed", "undefined"]],
    ["ariaReadOnly", ["true", "false"]],
    ["ariaRequired", ["true", "false"]],
    ["ariaSelected", ["true", "false", "undefined"]],
    ["ariaSort", ["ascending", "descending", "none", "other"]],
    ["ariaAtomic", ["true", "false"]],
    ["ariaBusy", ["true", "false"]],
  ]) {
    reflect(Element.prototype, name, "aria-" + name.slice(4).toLowerCase(),
      "enumerated", { keywords, missing: null, invalid: "" });
  }

  // ── per-tag interfaces ───────────────────────────────────────────────────
  //
  // A browser has HTMLAnchorElement, HTMLTableCellElement and eighty more, and
  // the split is not cosmetic. `colSpan` belongs to <td> and <th>; `span` to
  // <col> and <colgroup>; `scrollAmount` to <marquee> and nothing else. Hanging
  // all of them on one Element would make `"colSpan" in div` true, which is the
  // same lie the removed `missingApi` stubs told: feature detection asks before
  // it uses, and gets sent down a branch a real browser never takes.
  //
  // Each entry is [idl, content, type, options] and reads as the spec's
  // reflection table does. Names Element already defines — href, src, name,
  // type, disabled, value, checked, selected — are deliberately absent: those
  // carry behaviour beyond reflection, and `defaultChecked`, `defaultSelected`
  // and `defaultValue` are the spec's names for the reflecting half.
  const REFLECTIONS = {
    html: ["HTMLHtmlElement", [["version", "version"]]],
    head: ["HTMLHeadElement", []],
    title: ["HTMLTitleElement", []],
    base: ["HTMLBaseElement", [["target", "target"]]],
    link: ["HTMLLinkElement", [
      ["rel", "rel"], ["media", "media"], ["hreflang", "hreflang"],
      ["integrity", "integrity"], ["imageSrcset", "imagesrcset"],
      ["imageSizes", "imagesizes"], ["charset", "charset"], ["rev", "rev"],
      ["target", "target"],
      ["as", "as", "enumerated", { keywords: [
        "fetch", "audio", "audioworklet", "document", "embed", "font", "frame",
        "iframe", "image", "json", "manifest", "object", "paintworklet",
        "report", "script", "serviceworker", "sharedworker", "style", "track",
        "video", "webidentity", "worker", "xslt"] }],
      ["crossOrigin", "crossorigin", "enumerated", {
        keywords: ["anonymous", "use-credentials"],
        missing: null, invalid: "anonymous" }],
      ["referrerPolicy", "referrerpolicy", "enumerated", { keywords: [
        "", "no-referrer", "no-referrer-when-downgrade", "same-origin",
        "origin", "strict-origin", "origin-when-cross-origin",
        "strict-origin-when-cross-origin", "unsafe-url"] }],
    ]],
    meta: ["HTMLMetaElement", [
      ["httpEquiv", "http-equiv"], ["media", "media"], ["scheme", "scheme"],
    ]],
    style: ["HTMLStyleElement", [["media", "media"]]],
    body: ["HTMLBodyElement", [
      ["link", "link"], ["vLink", "vlink"], ["aLink", "alink"],
      ["bgColor", "bgcolor"], ["background", "background"], ["text", "text"],
    ]],
    a: ["HTMLAnchorElement", [
      ["target", "target"], ["download", "download"], ["ping", "ping"],
      ["rel", "rel"], ["hreflang", "hreflang"], ["charset", "charset"],
      ["rev", "rev"], ["shape", "shape"], ["coords", "coords"],
      ["referrerPolicy", "referrerpolicy", "enumerated", { keywords: [
        "", "no-referrer", "no-referrer-when-downgrade", "same-origin",
        "origin", "strict-origin", "origin-when-cross-origin",
        "strict-origin-when-cross-origin", "unsafe-url"] }],
    ]],
    area: ["HTMLAreaElement", [
      ["coords", "coords"], ["download", "download"], ["ping", "ping"],
      ["rel", "rel"], ["shape", "shape"], ["target", "target"],
      ["noHref", "nohref", "bool"], ["referrerPolicy", "referrerpolicy"],
    ]],
    img: ["HTMLImageElement", [
      ["srcset", "srcset"], ["sizes", "sizes"], ["useMap", "usemap"],
      ["isMap", "ismap", "bool"], ["align", "align"], ["border", "border"],
      ["lowsrc", "lowsrc", "url"], ["longDesc", "longdesc", "url"],
      ["width", "width", "ulong"], ["height", "height", "ulong"],
      ["hspace", "hspace", "ulong"], ["vspace", "vspace", "ulong"],
      ["decoding", "decoding"], ["loading", "loading"],
      ["crossOrigin", "crossorigin"], ["referrerPolicy", "referrerpolicy"],
    ]],
    embed: ["HTMLEmbedElement", [
      ["width", "width"], ["height", "height"], ["align", "align"],
    ]],
    object: ["HTMLObjectElement", [
      ["data", "data", "url"], ["useMap", "usemap"], ["align", "align"],
      ["archive", "archive"], ["code", "code"], ["declare", "declare", "bool"],
      ["standby", "standby"], ["codeBase", "codebase", "url"],
      ["codeType", "codetype"], ["border", "border"],
      ["width", "width"], ["height", "height"],
      ["hspace", "hspace", "ulong"], ["vspace", "vspace", "ulong"],
    ]],
    param: ["HTMLParamElement", [["valueType", "valuetype"]]],
    video: ["HTMLVideoElement", [
      ["poster", "poster", "url"], ["preload", "preload"],
      ["autoplay", "autoplay", "bool"], ["loop", "loop", "bool"],
      ["controls", "controls", "bool"], ["defaultMuted", "muted", "bool"],
      ["crossOrigin", "crossorigin"],
      ["playsInline", "playsinline", "bool"],
      ["width", "width", "ulong"], ["height", "height", "ulong"],
    ]],
    audio: ["HTMLAudioElement", [
      ["preload", "preload"], ["autoplay", "autoplay", "bool"],
      ["loop", "loop", "bool"], ["controls", "controls", "bool"],
      ["defaultMuted", "muted", "bool"], ["crossOrigin", "crossorigin"],
    ]],
    source: ["HTMLSourceElement", [
      ["srcset", "srcset"], ["sizes", "sizes"], ["media", "media"],
      ["width", "width", "ulong"], ["height", "height", "ulong"],
    ]],
    track: ["HTMLTrackElement", [
      ["srclang", "srclang"], ["label", "label"], ["default", "default", "bool"],
      ["kind", "kind", "enumerated", {
        keywords: ["subtitles", "captions", "descriptions", "chapters", "metadata"],
        missing: "subtitles", invalid: "metadata" }],
    ]],
    map: ["HTMLMapElement", []],
    form: ["HTMLFormElement", [
      ["acceptCharset", "accept-charset"], ["action", "action", "url"],
      ["autocomplete", "autocomplete"], ["enctype", "enctype"],
      ["encoding", "enctype"], ["method", "method"],
      ["noValidate", "novalidate", "bool"], ["target", "target"], ["rel", "rel"],
    ]],
    label: ["HTMLLabelElement", [["htmlFor", "for"]]],
    input: ["HTMLInputElement", [
      ["accept", "accept"], ["autocomplete", "autocomplete"],
      ["defaultChecked", "checked", "bool"], ["dirName", "dirname"],
      ["formAction", "formaction", "url"], ["formEnctype", "formenctype"],
      ["formMethod", "formmethod"], ["formTarget", "formtarget"],
      ["formNoValidate", "formnovalidate", "bool"],
      ["max", "max"], ["min", "min"], ["pattern", "pattern"],
      ["placeholder", "placeholder"], ["step", "step"], ["useMap", "usemap"],
      ["align", "align"], ["defaultValue", "value"],
      ["multiple", "multiple", "bool"], ["required", "required", "bool"],
      ["readOnly", "readonly", "bool"],
      ["maxLength", "maxlength", "long", { default: -1 }],
      ["minLength", "minlength", "long", { default: -1 }],
      ["size", "size", "ulong", { default: 20 }],
      ["width", "width", "ulong"], ["height", "height", "ulong"],
    ]],
    button: ["HTMLButtonElement", [
      ["formAction", "formaction", "url"], ["formEnctype", "formenctype"],
      ["formMethod", "formmethod"], ["formTarget", "formtarget"],
      ["formNoValidate", "formnovalidate", "bool"],
    ]],
    select: ["HTMLSelectElement", [
      ["autocomplete", "autocomplete"], ["multiple", "multiple", "bool"],
      ["required", "required", "bool"], ["size", "size", "ulong"],
    ]],
    optgroup: ["HTMLOptGroupElement", [["label", "label"]]],
    option: ["HTMLOptionElement", [
      ["label", "label"], ["defaultSelected", "selected", "bool"],
    ]],
    textarea: ["HTMLTextAreaElement", [
      ["autocomplete", "autocomplete"], ["dirName", "dirname"],
      ["placeholder", "placeholder"], ["wrap", "wrap"],
      ["required", "required", "bool"], ["readOnly", "readonly", "bool"],
      ["maxLength", "maxlength", "long", { default: -1 }],
      ["minLength", "minlength", "long", { default: -1 }],
      ["cols", "cols", "ulong", { default: 20 }],
      ["rows", "rows", "ulong", { default: 2 }],
    ]],
    output: ["HTMLOutputElement", [["htmlFor", "for"]]],
    fieldset: ["HTMLFieldSetElement", []],
    legend: ["HTMLLegendElement", [["align", "align"]]],
    table: ["HTMLTableElement", [
      ["align", "align"], ["border", "border"], ["frame", "frame"],
      ["rules", "rules"], ["summary", "summary"], ["width", "width"],
      ["bgColor", "bgcolor"], ["cellPadding", "cellpadding"],
      ["cellSpacing", "cellspacing"],
    ]],
    caption: ["HTMLTableCaptionElement", [["align", "align"]]],
    col: ["HTMLTableColElement", [
      ["span", "span", "ulong", { default: 1 }], ["align", "align"],
      ["ch", "char"], ["chOff", "charoff"], ["vAlign", "valign"],
      ["width", "width"],
    ]],
    tr: ["HTMLTableRowElement", [
      ["align", "align"], ["ch", "char"], ["chOff", "charoff"],
      ["vAlign", "valign"], ["bgColor", "bgcolor"],
    ]],
    td: ["HTMLTableCellElement", [
      ["colSpan", "colspan", "ulong", { default: 1 }],
      ["rowSpan", "rowspan", "ulong", { default: 1 }],
      ["headers", "headers"], ["abbr", "abbr"], ["scope", "scope"],
      ["align", "align"], ["axis", "axis"], ["height", "height"],
      ["width", "width"], ["ch", "char"], ["chOff", "charoff"],
      ["noWrap", "nowrap", "bool"], ["vAlign", "valign"], ["bgColor", "bgcolor"],
    ]],
    ol: ["HTMLOListElement", [
      ["reversed", "reversed", "bool"], ["compact", "compact", "bool"],
      ["start", "start", "long", { default: 1 }],
    ]],
    ul: ["HTMLUListElement", [["compact", "compact", "bool"]]],
    li: ["HTMLLIElement", [["value", "value", "long"]]],
    dl: ["HTMLDListElement", [["compact", "compact", "bool"]]],
    blockquote: ["HTMLQuoteElement", [["cite", "cite", "url"]]],
    ins: ["HTMLModElement", [["cite", "cite", "url"], ["dateTime", "datetime"]]],
    script: ["HTMLScriptElement", [
      ["noModule", "nomodule", "bool"], ["async", "async", "bool"],
      ["defer", "defer", "bool"], ["integrity", "integrity"],
      ["charset", "charset"], ["event", "event"], ["htmlFor", "for"],
      ["crossOrigin", "crossorigin"], ["referrerPolicy", "referrerpolicy"],
    ]],
    marquee: ["HTMLMarqueeElement", [
      ["behavior", "behavior"], ["bgColor", "bgcolor"],
      ["direction", "direction"], ["height", "height"], ["width", "width"],
      ["hspace", "hspace", "ulong"], ["vspace", "vspace", "ulong"],
      ["trueSpeed", "truespeed", "bool"],
      ["scrollAmount", "scrollamount", "ulong", { default: 6 }],
      ["scrollDelay", "scrolldelay", "ulong", { default: 85 }],
      ["loop", "loop", "long", { default: -1 }],
    ]],
    applet: ["HTMLAppletElement", [
      ["align", "align"], ["archive", "archive"], ["code", "code"],
      ["codeBase", "codebase", "url"], ["height", "height"],
      ["object", "object"], ["width", "width"],
      ["hspace", "hspace", "ulong"], ["vspace", "vspace", "ulong"],
    ]],
    frame: ["HTMLFrameElement", [
      ["scrolling", "scrolling"], ["frameBorder", "frameborder"],
      ["longDesc", "longdesc", "url"], ["noResize", "noresize", "bool"],
      ["marginHeight", "marginheight"], ["marginWidth", "marginwidth"],
    ]],
    frameset: ["HTMLFrameSetElement", [["cols", "cols"], ["rows", "rows"]]],
    font: ["HTMLFontElement", [
      ["color", "color"], ["face", "face"], ["size", "size"],
    ]],
    dir: ["HTMLDirectoryElement", [["compact", "compact", "bool"]]],
    hr: ["HTMLHRElement", [
      ["align", "align"], ["color", "color"], ["size", "size"],
      ["width", "width"], ["noShade", "noshade", "bool"],
    ]],
    pre: ["HTMLPreElement", [["width", "width", "long"]]],
    details: ["HTMLDetailsElement", [["open", "open", "bool"]]],
    dialog: ["HTMLDialogElement", [["open", "open", "bool"]]],
    slot: ["HTMLSlotElement", []],
    canvas: ["HTMLCanvasElement", [
      ["width", "width", "ulong", { default: 300 }],
      ["height", "height", "ulong", { default: 150 }],
    ]],
    time: ["HTMLTimeElement", [["dateTime", "datetime"]]],
    data: ["HTMLDataElement", []],
    div: ["HTMLDivElement", [["align", "align"]]],
    h1: ["HTMLHeadingElement", [["align", "align"]]],
    tbody: ["HTMLTableSectionElement", [
      ["align", "align"], ["ch", "char"], ["chOff", "charoff"],
      ["vAlign", "valign"],
    ]],
    p: ["HTMLParagraphElement", [["align", "align"]]],
    span: ["HTMLSpanElement", []],
    br: ["HTMLBRElement", [["clear", "clear"]]],
    menu: ["HTMLMenuElement", [["compact", "compact", "bool"]]],
  };

  // Tags that share one interface with another tag, rather than repeating it.
  //
  // Only where the spec genuinely gives two tags one interface. <h1> is a
  // heading and <tbody> is a table section, so both got their own above rather
  // than being pointed at <p> and <tr>: `h1 instanceof HTMLParagraphElement`
  // would be false in every browser and true here, which is the kind of
  // almost-right this engine keeps having to remove.
  const SHARED = {
    colgroup: "col", th: "td", q: "blockquote", del: "ins",
    thead: "tbody", tfoot: "tbody",
    h2: "h1", h3: "h1", h4: "h1", h5: "h1", h6: "h1",
  };

  {
    const interfaces = {};
    for (const [tag, [name, attributes]] of Object.entries(REFLECTIONS)) {
      const Interface = { [name]: class extends Element {} }[name];
      for (const [idl, content, type, options] of attributes) {
        reflect(Interface.prototype, idl, content, type ?? "string", options ?? {});
      }
      interfaces[name] = Interface;
      TAG_CLASSES.set(tag, Interface);
    }
    // <th> is a table cell and <del> is a mod: same interface, both tags.
    for (const [tag, like] of Object.entries(SHARED)) {
      if (TAG_CLASSES.has(like)) TAG_CLASSES.set(tag, TAG_CLASSES.get(like));
    }
    // `instanceof HTMLAnchorElement` is something pages genuinely write, and
    // an engine with the interfaces but no names for them fails it anyway.
    Object.assign(globalThis, interfaces);
  }

  // ── the properties that are not plain reflections ────────────────────────
  //
  // These nine sat on Element until WPT probed them, which meant
  // `"checked" in document.createElement("div")` was true and
  // `document.createElement("div").type` was `"text"`. That is the
  // `missingApi` lie at property scale: feature detection asks before it uses,
  // and every one of these is something code branches on.
  //
  // Each is installed on exactly the interfaces the spec gives it. The ones
  // that *are* plain reflections are listed here too, so the whole set of
  // "which tags have this" is in one place rather than split between two
  // mechanisms.
  {
    const on = (tags, name, descriptor) => {
      for (const tag of tags) {
        const Interface = TAG_CLASSES.get(tag);
        if (!Interface) continue;
        Object.defineProperty(Interface.prototype, name, {
          configurable: true, ...descriptor,
        });
      }
    };
    const reflectOn = (tags, idl, content, type, options) => {
      for (const tag of tags) {
        const Interface = TAG_CLASSES.get(tag);
        if (Interface) reflect(Interface.prototype, idl, content, type ?? "string", options ?? {});
      }
    };

    // `href` and `src` are *resolved*, which is the difference between the
    // property and `getAttribute`. A page comparing `link.href` to
    // `location.href`, or reading `script.src` to find its own origin, gets the
    // absolute URL a browser would give it rather than the raw `../x` in the
    // markup.
    on(["a", "area", "link", "base"], "href", {
      get() { return this._resolved("href"); },
      set(v) { this.setAttribute("href", v); },
    });
    on(["img", "script", "embed", "source", "track", "audio", "video", "input"], "src", {
      get() { return this._resolved("src"); },
      set(v) { this.setAttribute("src", v); },
    });

    // The current value of a form control, which is not the `value` attribute:
    // a typed-in `<input>` and a `<select>` both answer from state, and
    // `<option>` falls back to its own text. `defaultValue` is the spec's name
    // for the attribute half and is reflected in the table above.
    on(["input", "option", "select", "textarea"], "value", {
      get() {
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
        // The editor is the truth when there is one and it holds something:
        // typing updates it and leaves the `value` attribute at whatever the
        // HTML said.
        const edited = api.getValue(this._id);
        if (edited && edited.trim()) return edited;

        // A blank editor on a `<textarea>` is the case worth handling. A
        // textarea's default value is its text content, and blitz lays one out
        // with an editor holding a single space rather than that content — so a
        // page whose comment box arrives filled in read back as blank, which is
        // a filled form reported as empty. Whitespace-only counts as unseeded
        // for that reason: the space is blitz's, not the page's.
        //
        // The limitation this leaves is small and stated: a textarea a user
        // has explicitly *cleared* also has an empty editor, and reports its
        // original text rather than "". Wrong in that one case, right in the
        // far commoner one, and it fails toward showing content that exists
        // rather than hiding it.
        if (tag === "TEXTAREA" && this._value === undefined) {
          const written = this.textContent;
          if (written) return written;
        }
        if (edited !== null && edited !== undefined) return edited;
        // There is no editor — a detached control, or a `<textarea>`, which
        // blitz lays out as text rather than as an input. Falling back to the
        // markup is what a browser reports, and answering "" instead made a
        // filled-in comment box look empty to the agent reading it.
        if (this._value !== undefined) return this._value;
        if (tag === "TEXTAREA") return this.textContent;
        return api.getAttr(this._id, "value") ?? "";
      },
      set(v) {
        const text = String(v);
        // Remembered on this side when the write had nowhere to land, so a
        // page that builds a control and fills it in can read back what it
        // wrote. A page that sets `.value` from script does not get
        // input/change: the spec fires those for *user* edits, and a framework
        // that re-rendered on its own write would loop. `Page::type_into` is
        // the user path.
        const landed = api.setValue(this._id, text);
        if (!landed) {
          this._value = text;
          if (this.tagName === "TEXTAREA") this.textContent = text;
        } else {
          delete this._value;
        }
      },
    });

    // `checked` is state, not the attribute. The attribute is the *default*,
    // which is why it is reflected separately as `defaultChecked` — writing to
    // it here made `getAttribute("checked")` change under a page that never
    // touched the markup, and made a box the user unticked look ticked still.
    on(["input"], "checked", {
      get() {
        if (this._checked !== undefined) return this._checked;
        return api.getAttr(this._id, "checked") !== null;
      },
      set(on_) { this._checked = !!on_; },
    });
    on(["option"], "selected", {
      get() { return api.getAttr(this._id, "selected") !== null; },
      set(on_) {
        if (on_) api.setAttr(this._id, "selected", "");
        else api.removeAttr(this._id, "selected");
      },
    });

    // `<input>` is the one element whose missing `type` is not the empty
    // string: an input with no type attribute is a text input, and code reads
    // `input.type` to decide how to treat it.
    on(["input"], "type", {
      get() { return (api.getAttr(this._id, "type") || "text").toLowerCase(); },
      set(v) { this.setAttribute("type", v); },
    });
    reflectOn(["a", "link", "script", "style", "embed", "object", "source",
               "param", "ol", "ul", "li", "button"], "type", "type");

    reflectOn(["img", "area", "input", "applet"], "alt", "alt");
    reflectOn(["input", "button", "select", "optgroup", "option", "textarea",
               "fieldset", "link", "style"], "disabled", "disabled", "bool");
    reflectOn(["form", "input", "select", "textarea", "button", "output",
               "fieldset", "object", "param", "map", "meta", "a", "img",
               "embed", "frame", "applet", "slot"], "name", "name");
    reflectOn(["button", "param", "data"], "value", "value");

    // Canvas 2D, drawn for real. See `src/canvas.rs`.
    //
    // This used to answer `null`, on the argument that a context object which
    // accepts `fillRect` and paints nothing is a lie the page cannot detect —
    // which was right, and is exactly what both reference engines ship. The
    // answer was never "fake it better"; it was that this engine owns a
    // rasteriser (`blitz-paint` over `vello_cpu`) and can therefore do the
    // thing itself, which is cheaper here than in an engine with no pixels at
    // all.
    //
    // The honesty rule survives intact and moves down one level. Every call
    // below routes through `api.canvasOp`, which returns **false** for an
    // operation that is not built, and a false answer is reported through the
    // same `unsupported()` channel as any other missing Web API. So a page
    // that calls `fillText` still gets its name in front of the agent —
    // `note: this page used Web APIs this engine does not have
    // (CanvasRenderingContext2D.fillText x12)` — rather than a blank canvas
    // with no explanation.
    //
    // Not built, and reported by name when asked for: text, gradients,
    // patterns, shadows, `drawImage`, `clip`, and the `ImageData` operations.
    class CanvasRenderingContext2D {
      constructor(canvas) {
        this.canvas = canvas;
        this._node = canvas._id;
        // Mirrored on the JS side because the spec requires reading them back,
        // and a getter that asked Rust for each would be a round trip per
        // property read in a draw loop.
        this._fillStyle = "#000000";
        this._strokeStyle = "#000000";
        this._lineWidth = 1;
        this._globalAlpha = 1;
        this._lineCap = "butt";
        this._lineJoin = "miter";
      }

      // Every drawing call, under one door. `false` back means the engine does
      // not have this one, and saying so is the whole point.
      _op(name, args) {
        if (api.canvasOp(this._node, name, args || []) === false) {
          api.unsupported(`CanvasRenderingContext2D.${name}`);
          return false;
        }
        return true;
      }

      get fillStyle() { return this._fillStyle; }
      set fillStyle(value) {
        // A colour this engine cannot parse is *reported*, and the previous
        // one stands — which is the spec's rule for an invalid value and also
        // keeps a gradient object from being read as if it were a colour.
        if (this._op("fillStyle", [String(value)])) this._fillStyle = String(value);
      }

      get strokeStyle() { return this._strokeStyle; }
      set strokeStyle(value) {
        if (this._op("strokeStyle", [String(value)])) this._strokeStyle = String(value);
      }

      get lineWidth() { return this._lineWidth; }
      set lineWidth(value) {
        this._lineWidth = Number(value);
        this._op("lineWidth", [Number(value)]);
      }

      get globalAlpha() { return this._globalAlpha; }
      set globalAlpha(value) {
        this._globalAlpha = Number(value);
        this._op("globalAlpha", [Number(value)]);
      }

      get lineCap() { return this._lineCap; }
      set lineCap(value) {
        this._lineCap = String(value);
        this._op("lineCap", [String(value)]);
      }

      get lineJoin() { return this._lineJoin; }
      set lineJoin(value) {
        this._lineJoin = String(value);
        this._op("lineJoin", [String(value)]);
      }

      save() { this._op("save", []); }
      restore() { this._op("restore", []); }

      translate(x, y) { this._op("translate", [+x, +y]); }
      scale(x, y) { this._op("scale", [+x, +y]); }
      rotate(a) { this._op("rotate", [+a]); }
      transform(a, b, c, d, e, f) { this._op("transform", [+a, +b, +c, +d, +e, +f]); }
      setTransform(a, b, c, d, e, f) { this._op("setTransform", [+a, +b, +c, +d, +e, +f]); }
      resetTransform() { this._op("resetTransform", []); }

      beginPath() { this._op("beginPath", []); }
      closePath() { this._op("closePath", []); }
      moveTo(x, y) { this._op("moveTo", [+x, +y]); }
      lineTo(x, y) { this._op("lineTo", [+x, +y]); }
      quadraticCurveTo(cx, cy, x, y) { this._op("quadraticCurveTo", [+cx, +cy, +x, +y]); }
      bezierCurveTo(a, b, c, d, e, f) { this._op("bezierCurveTo", [+a, +b, +c, +d, +e, +f]); }
      rect(x, y, w, h) { this._op("rect", [+x, +y, +w, +h]); }
      arc(x, y, r, s, e, ccw) { this._op("arc", [+x, +y, +r, +s, +e, ccw ? 1 : 0]); }

      fill(rule) { this._op("fill", rule ? [String(rule)] : []); }
      stroke() { this._op("stroke", []); }
      fillRect(x, y, w, h) { this._op("fillRect", [+x, +y, +w, +h]); }
      strokeRect(x, y, w, h) { this._op("strokeRect", [+x, +y, +w, +h]); }
      clearRect(x, y, w, h) { this._op("clearRect", [+x, +y, +w, +h]); }
    }

    // The operations this engine does not have, present and reporting.
    //
    // Absent would be worse here than anywhere else in the prelude, and it is
    // the one place the usual rule inverts. Canvas drawing is *incremental*: a
    // page issues thirty calls in a row, and if the fourth throws
    // "fillText is not a function" the remaining twenty-six never run, so a
    // page that would have rendered most of its chart renders none of it — and
    // the agent gets a blank canvas plus a stack trace about the wrong thing.
    //
    // Present-and-reporting keeps the rest of the drawing, and puts the real
    // answer where an agent will read it: `note: this page used Web APIs this
    // engine does not have (CanvasRenderingContext2D.fillText x12)`. That is
    // the routing signal §B8.4 exists for, and it is only available because
    // `api.canvasOp` answers `false` for what it does not know rather than
    // quietly returning.
    //
    // These are *not* silent stubs. A silent stub returns `undefined` and says
    // nothing; every one of these names itself the first time it is called.
    for (const name of [
      "fillText", "strokeText", "drawImage", "clip", "setLineDash",
      "ellipse", "arcTo", "roundRect", "putImageData", "createImageData",
    ]) {
      CanvasRenderingContext2D.prototype[name] = function (...args) {
        this._op(name, args.filter((a) => typeof a === "number"));
      };
    }

    // The value-returning half. Reported the same way, and answering `null`
    // rather than a plausible number: a `measureText` that claims a width this
    // engine never measured is the wrong-answer-that-looks-right this whole
    // engine is built to refuse, and a page that lays out against it would be
    // laying out against fiction.
    for (const name of [
      "measureText", "createLinearGradient", "createRadialGradient",
      "createConicGradient", "createPattern", "getImageData", "getLineDash",
      "isPointInPath", "isPointInStroke",
    ]) {
      CanvasRenderingContext2D.prototype[name] = function () {
        api.unsupported(`CanvasRenderingContext2D.${name}`);
        return null;
      };
    }

    // Properties that configure something unbuilt. Settable, so assignment
    // does not throw and the surrounding code runs, and named on the way past.
    for (const name of [
      "font", "textAlign", "textBaseline", "shadowBlur", "shadowColor",
      "shadowOffsetX", "shadowOffsetY", "globalCompositeOperation",
      "imageSmoothingEnabled", "filter", "miterLimit", "direction",
    ]) {
      Object.defineProperty(CanvasRenderingContext2D.prototype, name, {
        configurable: true,
        get() { return this[`_${name}`]; },
        set(value) {
          this[`_${name}`] = value;
          api.unsupported(`CanvasRenderingContext2D.${name}`);
        },
      });
    }
    globalThis.CanvasRenderingContext2D = CanvasRenderingContext2D;

    on(["canvas"], "getContext", {
      value: function (kind) {
        const wanted = String(kind).toLowerCase();
        if (wanted !== "2d") {
          // WebGL and the rest are genuinely absent, and `null` is what a
          // browser returns for a context it cannot provide — so a page's own
          // fallback branch runs, which is the behaviour the previous comment
          // here was right about and which still applies to everything but 2D.
          api.unsupported(`canvas.getContext(${String(kind)})`);
          return null;
        }
        if (!this._context2d) {
          // The surface is created at the element's current size, which is
          // what `width`/`height` reflect.
          const w = this.width || 300;
          const h = this.height || 150;
          // No reset: the surface, if one exists, is the page's and must
          // survive a second `getContext` call.
          api.canvasSize(this._id, w, h, false);
          this._context2d = new CanvasRenderingContext2D(this);
        }
        return this._context2d;
      },
      writable: true,
    });

    // `canvas.width = canvas.width` is the idiomatic erase, so the setters go
    // through to the surface rather than only to the attribute.
    for (const side of ["width", "height"]) {
      on(["canvas"], side, {
        get() {
          const raw = this.getAttribute(side);
          const parsed = raw === null ? null : parseInt(raw, 10);
          return Number.isFinite(parsed) ? parsed : (side === "width" ? 300 : 150);
        },
        set(value) {
          const next = Math.max(0, parseInt(value, 10) || 0);
          this.setAttribute(side, String(next));
          if (this._context2d) {
            // Always a reset, even when the number did not change: that is
            // what makes `canvas.width = canvas.width` the erase every page
            // uses it as.
            api.canvasSize(this._id, this.width, this.height, true);
          }
        },
      });
    }

    on(["canvas"], "toDataURL", {
      value: function (type) {
        if (type && String(type).toLowerCase() !== "image/png") {
          // Named rather than silently answering a PNG under a JPEG's name,
          // which would be a plausible wrong answer of exactly the kind this
          // engine refuses.
          api.unsupported(`canvas.toDataURL(${String(type)})`);
        }
        const url = api.canvasPng(this._id);
        // A canvas nobody drew on has no surface; a 1x1 transparent PNG is
        // what a browser gives back, and inventing one here would be less
        // honest than saying the canvas is empty.
        return url === null ? "data:," : url;
      },
      writable: true,
    });

    // The sheet an element owns. Only `<style>` and `<link>` have one, and a
    // `<link>` that is not a stylesheet has none — `img.sheet` being undefined
    // is the point of putting it here rather than on Element.
    // Forms and tables, which an agent reads more than almost anything else.
    //
    // `form.elements`, `table.rows`, `tr.cells` and `td.cellIndex` were all
    // absent, so a page that walks its own form or table — and a great deal of
    // page script does — got `undefined` and stopped.
    on(["form"], "elements", {
      get() {
        return collection(
          this.querySelectorAll("input, select, textarea, button, fieldset, output")
            .filter((el) => (api.getAttr(el._id, "type") || "").toLowerCase() !== "image"),
          "HTMLFormControlsCollection",
        );
      },
    });
    on(["form"], "length", { get() { return this.elements.length; } });
    // A form's default method is `get`, not the empty string: code branches on
    // it, and "" is not one of the branches.
    on(["form"], "method", {
      get() {
        const raw = (api.getAttr(this._id, "method") || "").toLowerCase();
        return raw === "post" || raw === "dialog" ? raw : "get";
      },
      set(value) { this.setAttribute("method", String(value)); },
    });

    // The form a control belongs to: its `form` attribute if it names one,
    // otherwise the form it sits inside.
    on(["input", "select", "textarea", "button", "fieldset", "output", "label"], "form", {
      get() {
        const named = api.getAttr(this._id, "form");
        if (named) return wrap(api.query("#" + cssEscapeIdent(named), 0));
        for (let n = this.parentNode; n; n = n.parentNode) {
          if (n.nodeType === 1 && n.tagName === "FORM") return n;
        }
        return null;
      },
    });

    // `<option>.text` is its text with whitespace collapsed, which is what a
    // `<select>` actually shows.
    on(["option"], "text", {
      get() { return (this.textContent || "").replace(/\s+/g, " ").trim(); },
      set(value) { this.textContent = String(value); },
    });
    on(["option"], "index", {
      get() {
        const owner = this.parentNode;
        if (!owner) return 0;
        return owner.querySelectorAll("option").findIndex((o) => o._id === this._id);
      },
    });
    on(["select"], "selectedIndex", {
      get() {
        const options = this.querySelectorAll("option");
        const at = options.findIndex((o) => o.selected);
        // A `<select>` with nothing marked selected shows its first option.
        return at >= 0 ? at : (options.length ? 0 : -1);
      },
      set(index) {
        const options = this.querySelectorAll("option");
        options.forEach((o, i) => { o.selected = i === Number(index); });
      },
    });
    on(["select"], "options", {
      get() { return collection(this.querySelectorAll("option"), "HTMLOptionsCollection"); },
    });

    // Tables. `rows` spans the sections in document order, which is what the
    // spec says and what a reader expects.
    on(["table"], "rows", {
      get() { return collection(this.querySelectorAll("tr"), "HTMLCollection"); },
    });
    on(["table"], "tBodies", {
      get() { return collection(this.querySelectorAll("tbody"), "HTMLCollection"); },
    });
    on(["table"], "tHead", { get() { return this.querySelector("thead"); } });
    on(["table"], "tFoot", { get() { return this.querySelector("tfoot"); } });
    on(["table"], "caption", { get() { return this.querySelector("caption"); } });
    on(["tr"], "cells", {
      get() { return collection(this.querySelectorAll("td, th"), "HTMLCollection"); },
    });
    on(["tr"], "rowIndex", {
      get() {
        for (let n = this.parentNode; n; n = n.parentNode) {
          if (n.nodeType === 1 && n.tagName === "TABLE") {
            return n.querySelectorAll("tr").findIndex((r) => r._id === this._id);
          }
        }
        return -1;
      },
    });
    on(["td"], "cellIndex", {
      get() {
        const row = this.parentNode;
        if (!row || row.tagName !== "TR") return -1;
        return row.querySelectorAll("td, th").findIndex((c) => c._id === this._id);
      },
    });

    on(["style", "link"], "sheet", {
      get() {
        if (this.tagName === "LINK") {
          const rel = (api.getAttr(this._id, "rel") || "").toLowerCase();
          if (!rel.split(/\s+/).includes("stylesheet")) return null;
        }
        return CSSStyleSheet.forElement(this);
      },
    });
  }

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

  /// The window's own `on*` properties.
  ///
  /// `window.onload = fn` stored the function and never called it. The
  /// accessors above are installed on `Element.prototype`, and the window is
  /// not an element, so the assignment landed on an ordinary expando: it read
  /// back correctly, which is why nothing looked wrong, and it never ran.
  ///
  /// These delegate to the window's own `addEventListener`, so a page that
  /// mixes the two forms gets one handler per assignment and no double-fire.
  const WINDOW_HANDLER_EVENTS = [
    "load", "unload", "beforeunload", "error", "message", "messageerror",
    "hashchange", "popstate", "pagehide", "pageshow", "resize", "scroll",
    "storage", "offline", "online", "languagechange", "rejectionhandled",
    "unhandledrejection", "afterprint", "beforeprint",
  ];
  for (const type of WINDOW_HANDLER_EVENTS) {
    const slot = `__on_window_${type}`;
    Object.defineProperty(globalThis, `on${type}`, {
      configurable: true,
      get() { return globalThis[slot] ?? null; },
      set(handler) {
        if (globalThis[slot]) removeEventListener(type, globalThis[slot]);
        globalThis[slot] = typeof handler === "function" ? handler : null;
        if (globalThis[slot]) addEventListener(type, globalThis[slot]);
      },
    });
  }

  /// Event-handler *content attributes*: `<body onload="run()">`.
  ///
  /// These never ran either, and for a different reason than the window
  /// properties: the `on*` accessors are IDL attributes, reached when *script*
  /// assigns them, and markup does not go through script. Nothing was
  /// compiling an attribute into a handler at all, so a page whose entire
  /// behaviour hangs off `<body onload>` loaded, did nothing, and looked idle
  /// — which is exactly how it was scored.
  ///
  /// The compiled function is the spec's shape: the attribute value is a
  /// function *body* rather than an expression, it takes `event`, and it is
  /// called with the element as `this`.
  ///
  /// The attribute names are enumerated rather than discovered because they
  /// have to become a selector. CSS can match an attribute's value by prefix
  /// but not its *name*, so "every element with some `on*` attribute" is not a
  /// selector — and the alternative, walking every element and asking for its
  /// attribute names, is a call into the tree per element on documents that
  /// run to tens of thousands of them.
  const HANDLER_ATTRS = [
    ...HANDLER_EVENTS, ...WINDOW_HANDLER_EVENTS,
    "beforeinput", "select", "reset", "invalid", "toggle", "cancel", "close",
    "copy", "cut", "paste", "drag", "dragend", "dragenter", "dragleave",
    "dragover", "dragstart", "drop", "animationstart", "animationiteration",
    "transitionrun", "transitionstart", "transitioncancel", "pointermove",
    "pointerover", "pointerout", "pointerenter", "pointerleave", "pointercancel",
    "mouseenter", "mouseleave", "focusin", "focusout", "readystatechange",
  ];
  const HANDLER_ATTR_SET = new Set(HANDLER_ATTRS.map((type) => `on${type}`));
  const HANDLER_ATTR_SELECTOR = HANDLER_ATTRS.map((type) => `[on${type}]`).join(",");

  /// The handlers `<body>` and `<frameset>` do not keep for themselves.
  ///
  /// The spec forwards this set to the window, and the difference is not
  /// cosmetic: `load` is fired *at* the window, so a `<body onload>` installed
  /// on the body element would sit through the one event it exists for.
  const BODY_FORWARDED = new Set([
    "blur", "error", "focus", "load", "resize", "scroll", "afterprint",
    "beforeprint", "beforeunload", "hashchange", "languagechange", "message",
    "messageerror", "offline", "online", "pagehide", "pageshow", "popstate",
    "rejectionhandled", "storage", "unhandledrejection", "unload",
  ]);

  function installInlineHandler(element, name, source) {
    const type = name.slice(2);
    let compiled;
    try {
      compiled = new Function("event", source);
    } catch (error) {
      // A handler that does not parse is the page's bug, not this engine's, and
      // a browser reports it and carries on rather than taking the document
      // down with it.
      console.error(`inline ${name} did not compile: ${error}`);
      return;
    }
    const handler = function (event) { return compiled.call(element, event); };
    const tag = element.tagName;
    if ((tag === "BODY" || tag === "FRAMESET") && BODY_FORWARDED.has(type)) {
      globalThis[`on${type}`] = handler;
      return;
    }
    if (`on${type}` in element) element[`on${type}`] = handler;
    else element.addEventListener(type, handler);
  }

  /// Compile the inline handlers under `within`, or under the whole document.
  ///
  /// Idempotent by remembering the source it last compiled per attribute, so
  /// the sweep can run again after markup arrives without stacking a second
  /// copy of every handler on the elements it already saw.
  globalThis.__h5iInstallInlineHandlers = function (within) {
    const scope = within && within._id ? within._id : 0;
    for (const id of api.queryAll(HANDLER_ATTR_SELECTOR, scope)) {
      const element = wrap(id);
      if (!element) continue;
      const installed = element.__h5iInline ?? (element.__h5iInline = {});
      for (const name of api.attrNames(id) ?? []) {
        const lowered = String(name).toLowerCase();
        if (!HANDLER_ATTR_SET.has(lowered)) continue;
        const source = api.getAttr(id, name);
        if (source == null || installed[lowered] === source) continue;
        installed[lowered] = source;
        installInlineHandler(element, lowered, source);
      }
    }
  };

  /// Turn every `<template shadowrootmode>` inside `within` into a shadow root.
  ///
  /// Order matters and is the fiddly part: `attachShadow` takes the host's
  /// light children out of the way first, so the template has to be removed
  /// *before* the root is attached, or the template itself would be filed as
  /// light content of the component it was supposed to become.
  function adoptDeclarativeShadowRoots(within) {
    const templates = api
      .queryAll("template[shadowrootmode]", within._id)
      .map(wrap)
      .filter(Boolean);
    for (const template of templates) {
      const host = template.parentNode;
      if (!host || host._shadow || host.nodeType !== 1) continue;
      const mode = (api.getAttr(template._id, "shadowrootmode") || "open").toLowerCase();
      const content = [...template.childNodes];
      for (const node of content) detachFromParent(node);
      template.remove();
      const root = host.attachShadow({ mode: mode === "closed" ? "closed" : "open" });
      for (const node of content) root.appendChild(node);
    }
  }

  /// Refuse a selector a browser would refuse, rather than answering "nothing".
  ///
  /// `querySelector("!!!")` throws `SyntaxError` in a browser and returned
  /// `null` here — the same answer as "no such element", so a page with a typo
  /// took its not-found branch and never learned why.
  /// The selectors this engine cannot parse but which are not the page's fault.
  ///
  /// `:has()` is the whole list today. Stylo's servo selector parser answers
  /// `parse_has() -> false` and it is hardcoded, not a preference, so `:has()`
  /// has never parsed here — in a stylesheet or in `querySelector`. That is a
  /// missing feature, and the throw below is right either way: an unsupported
  /// pseudo-class makes a selector invalid, and a browser without `:has()`
  /// throws too.
  ///
  /// What was wrong was the sentence. "`.x:has(.y)` is not a valid selector"
  /// is false — it is a valid selector, and it is 2026. Someone reading that
  /// goes looking for a typo they will not find, which is the most expensive
  /// thing a diagnostic can do. Naming it as unsupported also files it through
  /// `api.unsupported`, so it appears in the counted gaps beside every other
  /// feature this engine does not have rather than hiding inside a parse error.
  const UNSUPPORTED_SELECTOR = /(^|[^\\w-])::?has\s*\(/i;

  function checkSelector(selector) {
    const text = String(selector);
    if (!api.validSelector(text)) {
      if (UNSUPPORTED_SELECTOR.test(text)) {
        api.unsupported("selector :has()");
        throw new DOMException(
          `${text} uses :has(), which this engine does not support yet. ` +
            "It is a valid selector; the engine is the limitation.",
          "SyntaxError",
        );
      }
      throw new DOMException(`${text} is not a valid selector`, "SyntaxError");
    }
    return text;
  }

  function camelToDash(name) {
    return name.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
  }

  // Inline style, backed by the element's own `style` attribute rather than by
  // a parallel object, so what script sets is what the cascade sees and what a
  // later `getAttribute("style")` returns. One source of truth, same rule the
  // DOM follows.
  class StyleDeclaration {
    /// `source` is a get/set pair for the declaration *text*.
    ///
    /// An element's inline style reads and writes its `style` attribute; a rule
    /// inside a stylesheet reads and writes its own body. One parser and one
    /// serialiser for both, rather than a second copy that could disagree with
    /// this one about what `color:red;;` means.
    constructor(source) { this._source = source; }

    _read() {
      const raw = this._source.get();
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
      // Trailing semicolon, as a browser serialises it: `color: red;`. Pages do
      // compare `getAttribute("style")` against a literal.
      const text = [...map.entries()].map(([k, v]) => `${k}: ${v};`).join(" ");
      this._source.set(text);
    }
    get length() { return this._read().size; }
    item(index) { return [...this._read().keys()][index] ?? ""; }

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
    get cssText() { return this._source.get(); }
    set cssText(text) { this._source.set(String(text)); }
  }

  /// The backing an element's inline style uses: its own `style` attribute, so
  /// what script sets is what the cascade sees and what `getAttribute("style")`
  /// returns.
  function inlineStyleSource(node) {
    return {
      get: () => api.getAttr(node._id, "style") || "",
      // Always written, never removed: emptying a style declaration leaves an
      // empty `style` attribute in a browser, and `getAttribute("style")`
      // answers "" rather than null.
      set: (text) => api.setAttr(node._id, "style", text),
    };
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
  StyleDeclaration = function (source) {
    return new Proxy(new RawStyleDeclaration(source), styleHandler);
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
        // Removed *before* the call, not after: a handler that throws, or that
        // dispatches the same event again, must still not run twice. `once`
        // was being ignored entirely, so a page relying on it double-handled
        // and nothing said so.
        if (l.once) {
          const at = listeners.indexOf(l);
          if (at >= 0) listeners.splice(at, 1);
        }
        try {
          if (typeof l.handler === "function") l.handler.call(node, event);
          else if (l.handler && typeof l.handler.handleEvent === "function") {
            l.handler.handleEvent(event);
          }
        } catch (error) {
          console.error("listener for " + event.type + " threw: " + withStack(error));
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
      // The two that decide what happens at an origin boundary. Defaults are
      // the spec's — `cors` and `same-origin` — so a page that says nothing
      // gets what a browser gives it: the request may cross, and it does not
      // take the session with it.
      this.mode = i.mode || (input instanceof Request ? input.mode : "cors");
      this.credentials =
        i.credentials || (input instanceof Request ? input.credentials : "same-origin");
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
      console.error("observer callback threw: " + withStack(error));
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
        console.error("MutationObserver callback threw: " + withStack(error));
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
  /// Where a node sits among its siblings.
  function nodeIndex(node) {
    const parent = node && node.parentNode;
    if (!parent) return 0;
    const kids = parent.childNodes;
    for (let i = 0; i < kids.length; i++) if (kids[i]._id === node._id) return i;
    return 0;
  }

  function sameNode(a, b) {
    return !!a && !!b && a._id !== undefined && a._id === b._id;
  }

  /// Every node at or under `node`, in document order.
  function flattenTree(node, out = []) {
    if (!node) return out;
    out.push(node);
    if (node.nodeType === 1 || node.nodeType === 9 || node.nodeType === 11) {
      for (const kid of node.childNodes) flattenTree(kid, out);
    }
    return out;
  }

  /// A range over the document, with the offsets a real one has.
  ///
  /// The version this replaces stored two containers, ignored every offset it
  /// was given, and answered `toString()` with the start container's entire
  /// `textContent`. That is fine until something depends on it — and Selection
  /// and `execCommand` depend on nothing else, because a selection *is* a pair
  /// of boundary points.
  ///
  /// Boundary points are compared by flattening the common ancestor in document
  /// order rather than by a general position comparison. That is enough for what
  /// runs through here and it is honest about its shape: ranges whose ends sit
  /// in unrelated trees are not ordered by this, and nothing asks them to be.
  class Range {
    constructor() {
      this.startContainer = document;
      this.startOffset = 0;
      this.endContainer = document;
      this.endOffset = 0;
    }
    get collapsed() {
      return sameNode(this.startContainer, this.endContainer)
        ? this.startOffset === this.endOffset
        : this.startContainer === this.endContainer && this.startOffset === this.endOffset;
    }
    setStart(node, offset) { this.startContainer = node; this.startOffset = Number(offset) || 0; }
    setEnd(node, offset) { this.endContainer = node; this.endOffset = Number(offset) || 0; }
    setStartBefore(node) { this.setStart(node.parentNode, nodeIndex(node)); }
    setStartAfter(node) { this.setStart(node.parentNode, nodeIndex(node) + 1); }
    setEndBefore(node) { this.setEnd(node.parentNode, nodeIndex(node)); }
    setEndAfter(node) { this.setEnd(node.parentNode, nodeIndex(node) + 1); }
    selectNode(node) { this.setStartBefore(node); this.setEndAfter(node); }
    selectNodeContents(node) {
      this.setStart(node, 0);
      this.setEnd(node, node.nodeType === 3 ? node.data.length : node.childNodes.length);
    }
    collapse(toStart) {
      if (toStart) { this.endContainer = this.startContainer; this.endOffset = this.startOffset; }
      else { this.startContainer = this.endContainer; this.startOffset = this.endOffset; }
    }
    cloneRange() {
      const copy = new Range();
      copy.startContainer = this.startContainer; copy.startOffset = this.startOffset;
      copy.endContainer = this.endContainer; copy.endOffset = this.endOffset;
      return copy;
    }
    detach() {}

    get commonAncestorContainer() {
      const ancestors = [];
      for (let n = this.startContainer; n; n = n.parentNode) ancestors.push(n);
      for (let n = this.endContainer; n; n = n.parentNode) {
        if (ancestors.some((a) => sameNode(a, n) || a === n)) return n;
      }
      return document;
    }

    /// The nodes this range covers, with any partial text pieces resolved.
    ///
    /// Returns entries of `{ node, text, whole }`: `whole` marks a node that is
    /// entirely inside the range and can simply be removed, which is what makes
    /// `deleteContents` and `toString` share one traversal instead of
    /// disagreeing about what "inside" means.
    _pieces() {
      const flat = flattenTree(this.commonAncestorContainer);
      const at = (node) => flat.findIndex((n) => sameNode(n, node) || n === node);

      // A boundary point is either *inside* a text node, or *between* two
      // children of an element. Resolving both to a position in the flattened
      // list is the whole trick: without it, `selectNodeContents(p)` — whose
      // boundaries are both the element `p` — covered no text node at all and
      // the selection read as empty.
      const resolve = (container, offset) => {
        if (container.nodeType === 3) return { index: at(container), textOffset: offset };
        const here = at(container);
        if (here < 0) return { index: -1, textOffset: null };
        const kids = container.childNodes;
        if (offset < kids.length) return { index: at(kids[offset]), textOffset: null };
        // Past the last child: the position just after this whole subtree.
        return { index: here + flattenTree(container).length, textOffset: null };
      };

      const from = resolve(this.startContainer, this.startOffset);
      const to = resolve(this.endContainer, this.endOffset);
      if (from.index < 0 || to.index < 0) return [];

      // An element end boundary sits *before* the node at its index; a text one
      // sits inside it.
      const last = to.textOffset === null ? to.index - 1 : to.index;
      const out = [];
      for (let i = from.index; i <= last; i++) {
        const node = flat[i];
        if (!node || node.nodeType !== 3) continue;
        const begin = i === from.index && from.textOffset !== null ? from.textOffset : 0;
        const finish = i === to.index && to.textOffset !== null ? to.textOffset : node.data.length;
        if (finish <= begin) continue;
        out.push({
          node,
          text: node.data.slice(begin, finish),
          begin,
          finish,
          whole: begin === 0 && finish >= node.data.length,
        });
      }
      return out;
    }

    toString() { return this._pieces().map((piece) => piece.text).join(""); }

    deleteContents() {
      // Reversed, so removing a node cannot shift the offsets of the pieces not
      // yet handled. Sliced by offset rather than by matching the text, because
      // `split(text).join("")` deleted every *other* occurrence of the same
      // string in the same node too.
      for (const piece of this._pieces().reverse()) {
        if (piece.whole) piece.node.remove();
        else piece.node.data = piece.node.data.slice(0, piece.begin) + piece.node.data.slice(piece.finish);
      }
      this.collapse(true);
    }

    /// Put a node at the start boundary.
    insertNode(node) {
      const container = this.startContainer;
      if (container.nodeType === 3) {
        // Split the text so the node lands exactly where the boundary is,
        // rather than before or after the whole run.
        const after = container.splitText
          ? container.splitText(this.startOffset)
          : null;
        const parent = container.parentNode;
        if (!parent) return node;
        if (after) parent.insertBefore(node, after);
        else parent.appendChild(node);
        return node;
      }
      const kids = container.childNodes;
      if (this.startOffset < kids.length) container.insertBefore(node, kids[this.startOffset]);
      else container.appendChild(node);
      return node;
    }

    surroundContents(wrapper) {
      const text = this.toString();
      this.deleteContents();
      wrapper.textContent = text;
      this.insertNode(wrapper);
      return wrapper;
    }

    extractContents() {
      const fragment = new DocumentFragment();
      const text = this.toString();
      this.deleteContents();
      if (text) fragment.appendChild(document.createTextNode(text));
      return fragment;
    }
    cloneContents() {
      const fragment = new DocumentFragment();
      const text = this.toString();
      if (text) fragment.appendChild(document.createTextNode(text));
      return fragment;
    }

    getBoundingClientRect() {
      const anchor = this.startContainer && this.startContainer.nodeType === 3
        ? this.startContainer.parentNode
        : this.startContainer;
      return anchor && anchor.getBoundingClientRect
        ? anchor.getBoundingClientRect()
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
  }

  /// The document's selection: one range, which is all a browser gives you.
  ///
  /// Agents need this to act on text — "select this paragraph, replace it" is
  /// the shape of a great deal of real work — and `execCommand` below is
  /// defined entirely in terms of it.
  ///
  /// Multiple ranges are not supported and say so by reporting `rangeCount`
  /// honestly: only Gecko ever implemented them, and code that asks for range
  /// two is code written against a browser this is not.
  class Selection {
    constructor() { this._range = null; this._direction = "forward"; }

    get rangeCount() { return this._range ? 1 : 0; }
    get isCollapsed() { return !this._range || this._range.collapsed; }
    get type() {
      if (!this._range) return "None";
      return this._range.collapsed ? "Caret" : "Range";
    }
    get anchorNode() { return this._range ? this._range.startContainer : null; }
    get anchorOffset() { return this._range ? this._range.startOffset : 0; }
    get focusNode() { return this._range ? this._range.endContainer : null; }
    get focusOffset() { return this._range ? this._range.endOffset : 0; }

    getRangeAt(index) {
      if (Number(index) !== 0 || !this._range) {
        throw new DOMException(`there is no range ${index}`, "IndexSizeError");
      }
      return this._range;
    }
    addRange(range) { if (!this._range) this._range = range; }
    removeRange(range) { if (this._range === range) this._range = null; }
    removeAllRanges() { this._range = null; }
    empty() { this.removeAllRanges(); }

    collapse(node, offset) {
      if (node === null || node === undefined) return this.removeAllRanges();
      const range = new Range();
      range.setStart(node, offset || 0);
      range.collapse(true);
      this._range = range;
    }
    setPosition(node, offset) { this.collapse(node, offset); }
    collapseToStart() {
      if (!this._range) throw new DOMException("nothing is selected", "InvalidStateError");
      this._range.collapse(true);
    }
    collapseToEnd() {
      if (!this._range) throw new DOMException("nothing is selected", "InvalidStateError");
      this._range.collapse(false);
    }
    extend(node, offset) {
      if (!this._range) throw new DOMException("nothing is selected", "InvalidStateError");
      this._range.setEnd(node, offset || 0);
    }
    setBaseAndExtent(anchor, anchorOffset, focus, focusOffset) {
      const range = new Range();
      range.setStart(anchor, anchorOffset);
      range.setEnd(focus, focusOffset);
      this._range = range;
    }
    selectAllChildren(node) {
      const range = new Range();
      range.selectNodeContents(node);
      this._range = range;
    }
    deleteFromDocument() { if (this._range) this._range.deleteContents(); }
    containsNode(node, partly) {
      if (!this._range || !node) return false;
      const covered = flattenTree(this._range.commonAncestorContainer);
      const inside = covered.some((n) => sameNode(n, node));
      if (!inside) return false;
      if (partly) return true;
      const text = this._range.toString();
      return text.includes(node.textContent || "");
    }
    toString() { return this._range ? this._range.toString() : ""; }
  }

  const selection = new Selection();
  function getSelection() { return observed(selection, "Selection"); }

  /// `document.execCommand`, for the commands this engine can actually carry out.
  ///
  /// Deprecated, never converged across browsers, and still the only way a page
  /// edits a `contenteditable` region — which is what an agent driving a rich
  /// text editor has to go through. So: a small set, done properly, and
  /// everything else answers **false** from both `execCommand` and
  /// `queryCommandSupported` rather than returning true and doing nothing. A
  /// command that reports success without acting is the failure this engine
  /// keeps removing.
  const COMMANDS = {
    bold: (sel) => wrapSelection(sel, "b"),
    italic: (sel) => wrapSelection(sel, "i"),
    underline: (sel) => wrapSelection(sel, "u"),
    strikethrough: (sel) => wrapSelection(sel, "s"),
    subscript: (sel) => wrapSelection(sel, "sub"),
    superscript: (sel) => wrapSelection(sel, "sup"),

    inserttext: (sel, value) => replaceSelection(sel, document.createTextNode(String(value ?? ""))),
    inserthtml: (sel, value) => {
      const host = document.createElement("div");
      host.innerHTML = String(value ?? "");
      const fragment = new DocumentFragment();
      for (const kid of [...host.childNodes]) fragment.appendChild(kid);
      return replaceSelection(sel, fragment);
    },
    insertlinebreak: (sel) => replaceSelection(sel, document.createElement("br")),
    insertparagraph: (sel) => replaceSelection(sel, document.createElement("p")),

    delete: (sel) => { if (!sel.rangeCount) return false; sel.getRangeAt(0).deleteContents(); return true; },
    forwarddelete: (sel) => COMMANDS.delete(sel),

    selectall: (sel) => {
      const body = wrap(api.body());
      if (!body) return false;
      sel.selectAllChildren(body);
      return true;
    },

    createlink: (sel, value) => {
      if (!sel.rangeCount || sel.isCollapsed) return false;
      const link = document.createElement("a");
      link.setAttribute("href", String(value ?? ""));
      return wrapSelectionWith(sel, link);
    },
    unlink: (sel) => {
      if (!sel.rangeCount) return false;
      const range = sel.getRangeAt(0);
      let found = false;
      for (let n = range.startContainer; n; n = n.parentNode) {
        if (n.nodeType === 1 && n.tagName === "A") {
          const text = document.createTextNode(n.textContent);
          n.parentNode.insertBefore(text, n);
          n.remove();
          found = true;
          break;
        }
      }
      return found;
    },

    formatblock: (sel, value) => {
      const tag = String(value ?? "").replace(/[<>]/g, "").toLowerCase();
      if (!tag) return false;
      return wrapSelectionWith(sel, document.createElement(tag));
    },
  };

  function replaceSelection(sel, node) {
    if (!sel.rangeCount) return false;
    const range = sel.getRangeAt(0);
    range.deleteContents();
    range.insertNode(node);
    return true;
  }

  function wrapSelectionWith(sel, wrapper) {
    if (!sel.rangeCount) return false;
    const range = sel.getRangeAt(0);
    if (range.collapsed) return true;
    range.surroundContents(wrapper);
    return true;
  }

  function wrapSelection(sel, tag) {
    return wrapSelectionWith(sel, document.createElement(tag));
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

  let adoptedSheets = [];

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
    // `document.write`, emulated where it can be and refused where it cannot.
    //
    // A browser inserts the markup at the parser's position, which is right
    // after the script doing the writing. This engine parses the whole document
    // before running anything, so "the parser's position" does not exist — but
    // `currentScript` does, and inserting after it is the same place for the one
    // use that is deliberate: an inline script emitting markup in situ, which is
    // what caniuse.com does with `<style>.static-only{display:none}</style>`.
    //
    // Called with no script running — from a timer, a promise, a module — a
    // browser would implicitly `document.open()` and **wipe the page**. That is
    // not emulated. Doing so would destroy a document over a call that in a
    // browser would have happened during parsing and been harmless; the
    // difference is this engine's script timing, not the page's intent. It is
    // refused by name instead.
    write(...parts) {
      const markup = parts.join("");
      const id = globalThis.__h5iCurrentScript;
      const script = id === null || id === undefined ? null : wrap(id);
      if (!script || !script.parentNode) {
        api.unsupported("document.write (after parsing)");
        return;
      }
      const host = document.createElement("div");
      host.innerHTML = markup;
      const parent = script.parentNode;
      const next = script.nextSibling;
      for (const kid of host.childNodes) {
        if (next) parent.insertBefore(kid, next);
        else parent.appendChild(kid);
      }
    },
    writeln(...parts) { this.write(...parts, "\n"); },
    // `open` and `close` exist so a page that brackets its writes does not throw
    // on the bracket. Neither replaces the document, for the reason above.
    open() { return document; },
    close() {},

    createRange() { return observed(new Range(), "Range"); },
    getSelection() { return getSelection(); },

    /// The commands this engine carries out, and no others.
    ///
    /// `queryCommandSupported` answers from the same table `execCommand` acts
    /// on, so the two can never disagree — a page that asks first and acts
    /// second gets one consistent story.
    execCommand(name, _showUI, value) {
      const key = String(name ?? "").toLowerCase();
      const command = COMMANDS[key];
      if (!command) {
        api.unsupported(`document.execCommand(${key})`);
        return false;
      }
      try {
        return !!command(selection, value);
      } catch (error) {
        console.error(`execCommand(${key}) threw: ${withStack(error)}`);
        return false;
      }
    },
    queryCommandSupported(name) {
      return Object.prototype.hasOwnProperty.call(COMMANDS, String(name ?? "").toLowerCase());
    },
    queryCommandEnabled(name) {
      return this.queryCommandSupported(name) && selection.rangeCount > 0;
    },
    /// Always false, and honest about why: this engine keeps no record of the
    /// formatting around the caret, so "is the selection bold" is a question it
    /// cannot answer. Returning a guess would be worse than returning false —
    /// an editor toolbar would light up at random.
    queryCommandState(name) {
      api.unsupported(`document.queryCommandState(${String(name ?? "")})`);
      return false;
    },
    queryCommandValue(name) {
      api.unsupported(`document.queryCommandValue(${String(name ?? "")})`);
      return "";
    },
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
    querySelector(sel) { return wrap(api.query(checkSelector(sel), 0)); },
    querySelectorAll(sel) { return collection(api.queryAll(checkSelector(sel), 0).map(wrap)); },
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
    get readyState() { return documentReadyState; },

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
    /// What this document was decoded as. All three names are the same value
    /// and all three are in use: `characterSet` is current, `charset` is the
    /// legacy alias, and `inputEncoding` is the one the DOM spec kept.
    get characterSet() { return api.documentEncoding(); },
    get charset() { return api.documentEncoding(); },
    get inputEncoding() { return api.documentEncoding(); },
    // Adopting a sheet applies it. Assignment replaces the set, as in a browser.
    /// What scrolls when the document scrolls. In standards mode that is
    /// `<html>`, and code reads it to avoid the quirks-mode `<body>` split.
    get scrollingElement() { return wrap(api.root()); },
    /// Every `<style>` and `<link rel=stylesheet>` the document has, as sheets.
    get styleSheets() { return styleSheetList(); },
    get adoptedStyleSheets() { return adoptedSheets.slice(); },
    set adoptedStyleSheets(sheets) {
      adoptedSheets = Array.from(sheets || []);
      for (const sheet of adoptedSheets) if (sheet && sheet._apply) sheet._apply();
    },

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

    /// What has focus. The body when nothing does, as in a browser — never
    /// null, which is what code branching on it expects.
    get activeElement() {
      if (focusedId !== null) {
        const focused = wrap(focusedId);
        if (focused && focused.isConnected) return focused;
        focusedId = null;
      }
      return wrap(api.body());
    },
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

  // How long a chain of timers that arm each other goes on holding the page
  // open. A loop that re-arms itself from inside its own callback — an
  // animation frame, a poller, a progress tick — is not on its way to
  // finishing, so counting it as outstanding work means the page can never be
  // described as anything but busy, and every read of it rides the settle
  // budget to the end before saying so.
  //
  // Past this depth the timers still fire; they stop *counting*. The page is
  // then reported as running periodic work rather than as unfinished, which is
  // a different fact and the one an agent can act on.
  const NESTING_LIMIT = 10;
  let timerDepth = 0;

  function setTimeout(fn, delay, ...args) {
    const id = nextTimer++;
    timers.set(id, {
      fn, due: clock + Math.max(0, delay | 0), args, every: null, depth: timerDepth + 1,
    });
    return id;
  }
  function setInterval(fn, delay, ...args) {
    const id = nextTimer++;
    const every = Math.max(1, delay | 0);
    timers.set(id, { fn, due: clock + every, args, every, depth: timerDepth + 1 });
    return id;
  }
  function clearTimeout(id) { timers.delete(id); }

  // A timer is *blocking* while the page still owes it: one-shot, and not so
  // deep in a self-arming chain that it has stopped converging.
  function timerBlocks(timer) {
    return timer.every === null && timer.depth < NESTING_LIMIT;
  }

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
      // Anything this callback arms inherits its depth, which is what makes a
      // self-arming chain measurable at all. Restored in `finally` so a timer
      // that throws does not leave every later one counted as nested.
      const outer = timerDepth;
      timerDepth = timer.depth;
      try { timer.fn(...timer.args); } catch (error) {
        console.error("timer threw: " + withStack(error));
      } finally {
        timerDepth = outer;
      }
      ran++;
    }
    return ran;
  };

  // Only *converging* timers count as work outstanding. An interval is
  // perpetual by definition, so waiting for the queue to drain would mean a
  // page with a polling loop — a clock, a carousel, an autosave — could never
  // be described as settled, and every snapshot of it would carry a "still
  // busy" note that told an agent nothing.
  //
  // A one-shot that re-arms itself is the same thing wearing a different hat,
  // and it took `NESTING_LIMIT` to see that: `requestAnimationFrame` is a
  // `setTimeout` here, so an animation loop presented as a fresh one-shot every
  // frame and rode the whole settle budget before reporting "still busy". Both
  // kinds still fire while the clock advances; neither holds the page open.
  // ── websockets ─────────────────────────────────────────────────────────
  //
  // Real, or absent. The rule at the top of the "absent, not stubbed" section
  // applies here more than anywhere: `typeof WebSocket === "function"` answering
  // true for a stub cost three sites their entire bundle. This is a working
  // object over a real connection, or the identifier is not defined at all.
  //
  // Delivery is at settle-round boundaries rather than the instant a frame
  // lands, because the session has no pump at rest. See `socket_drain` in
  // dom_api.rs.
  const openSockets = new Map(); // id -> WebSocket

  class WebSocket extends EventTarget {
    constructor(url, protocols) {
      super();
      if (arguments.length === 0) {
        throw new TypeError("WebSocket requires a url");
      }
      this._url = String(url);
      this._protocols = protocols;
      this.readyState = WebSocket.CONNECTING;
      this.bufferedAmount = 0;
      this.extensions = "";
      this.protocol = "";
      this.binaryType = "blob";
      this.onopen = null;
      this.onmessage = null;
      this.onclose = null;
      this.onerror = null;
      // Throws if the policy refused it, which is what a page sees for any
      // other refused request too.
      this._id = api.socketOpen(this._url);
      openSockets.set(this._id, this);
    }

    get url() {
      return this._url;
    }

    send(data) {
      if (this.readyState === WebSocket.CONNECTING) {
        throw new DOMException_("still connecting", "InvalidStateError");
      }
      if (this.readyState !== WebSocket.OPEN) return;
      api.socketSend(this._id, typeof data === "string" ? data : String(data));
    }

    close(code, reason) {
      if (this.readyState === WebSocket.CLOSED) return;
      this.readyState = WebSocket.CLOSING;
      api.socketClose(this._id);
      openSockets.delete(this._id);
      this.readyState = WebSocket.CLOSED;
      const event = new Event("close");
      event.code = code === undefined ? 1000 : code;
      event.reason = reason === undefined ? "" : String(reason);
      event.wasClean = true;
      this._fire("close", event);
    }

    _fire(kind, event) {
      const handler = this["on" + kind];
      if (typeof handler === "function") {
        try {
          handler.call(this, event);
        } catch (error) {
          console.error("websocket " + kind + " handler threw: " + withStack(error));
        }
      }
      this.dispatchEvent(event);
    }
  }
  WebSocket.CONNECTING = 0;
  WebSocket.OPEN = 1;
  WebSocket.CLOSING = 2;
  WebSocket.CLOSED = 3;

  // A minimal DOMException stand-in, only where the spec names one.
  function DOMException_(message, name) {
    const error = new Error(message);
    error.name = name;
    return error;
  }

  // Collect what arrived and turn it into events. Returns how many were
  // delivered, so the settle loop knows whether the round did any work: a
  // socket that is merely *open* must not hold the page busy forever, and one
  // that delivered a message should get another round.
  // `EventSource`, over the same delivery mechanism. Real, or absent — the
  // rule above applies here too.
  const openStreams = new Map(); // id -> EventSource

  class EventSource extends EventTarget {
    constructor(url, init) {
      super();
      if (arguments.length === 0) {
        throw new TypeError("EventSource requires a url");
      }
      this._url = String(url);
      this.withCredentials = !!(init && init.withCredentials);
      this.readyState = EventSource.CONNECTING;
      this.onopen = null;
      this.onmessage = null;
      this.onerror = null;
      this._id = api.sseOpen(this._url);
      openStreams.set(this._id, this);
    }

    get url() {
      return this._url;
    }

    close() {
      if (this.readyState === EventSource.CLOSED) return;
      api.sseClose(this._id);
      openStreams.delete(this._id);
      this.readyState = EventSource.CLOSED;
    }

    _fire(kind, event) {
      const handler = this["on" + kind];
      if (typeof handler === "function") {
        try {
          handler.call(this, event);
        } catch (error) {
          console.error("eventsource " + kind + " handler threw: " + withStack(error));
        }
      }
      this.dispatchEvent(event);
    }
  }
  EventSource.CONNECTING = 0;
  EventSource.OPEN = 1;
  EventSource.CLOSED = 2;

  globalThis.WebSocket = WebSocket;
  globalThis.EventSource = EventSource;

  globalThis.__h5iDrainSockets = function () {
    let delivered = 0;
    for (const socket of Array.from(openSockets.values())) {
      const events = api.socketDrain(socket._id);
      for (const entry of events) {
        const kind = entry[0];
        const payload = entry[1];
        delivered++;
        if (kind === "open") {
          socket.readyState = WebSocket.OPEN;
          socket._fire("open", new Event("open"));
        } else if (kind === "message") {
          const event = new Event("message");
          event.data = payload;
          event.origin = socket._url;
          socket._fire("message", event);
        } else if (kind === "close") {
          socket.readyState = WebSocket.CLOSED;
          // Tell the engine too, not just this map. Dropping it only here left
          // the Rust side holding the connection for the life of the page: the
          // snapshot reported a phantom open socket forever, and every later
          // `wait_for` polled in real time for the whole network budget
          // because the engine still believed something might arrive.
          api.socketClose(socket._id);
          openSockets.delete(socket._id);
          const event = new Event("close");
          event.code = 1006;
          event.reason = payload;
          event.wasClean = false;
          socket._fire("close", event);
        } else if (kind === "error") {
          const event = new Event("error");
          event.message = payload;
          socket._fire("error", event);
        }
      }
    }

    for (const stream of Array.from(openStreams.values())) {
      const events = api.sseDrain(stream._id);
      for (const entry of events) {
        const kind = entry[0];
        const payload = entry[1];
        delivered++;
        if (kind === "open") {
          stream.readyState = EventSource.OPEN;
          stream._fire("open", new Event("open"));
        } else if (kind === "message") {
          // The name arrives as its own field. Reading it out of the payload
          // meant guessing, and a plain `data: one\ndata: two` was read as an
          // event named `one` carrying `two`, so `onmessage` never fired.
          const name = entry[2] || "message";
          const event = new Event(name);
          event.data = payload;
          event.origin = stream._url;
          stream._fire(name === "message" ? "message" : name, event);
        } else if (kind === "close") {
          stream.readyState = EventSource.CLOSED;
          api.sseClose(stream._id);
          openStreams.delete(stream._id);
          const event = new Event("error");
          event.message = payload;
          stream._fire("error", event);
        } else if (kind === "error") {
          const event = new Event("error");
          event.message = payload;
          stream._fire("error", event);
        }
      }
    }

    return delivered;
  };

  /// How many long-lived connections this page has open, for the engine's own
  /// reporting.
  globalThis.__h5iOpenSockets = function () {
    return openSockets.size + openStreams.size;
  };

  globalThis.__h5iPendingTimers = function () {
    let pending = 0;
    for (const timer of timers.values()) if (timerBlocks(timer)) pending++;
    return pending;
  };

  /// Timers that are armed but no longer hold the page open: intervals, and
  /// one-shots deep enough in a self-arming chain to have stopped converging.
  ///
  /// Reported rather than hidden. "Nothing is left to run" and "the only thing
  /// left is a loop that will never stop" are different answers, and a caller
  /// that waited for an element deserves to know which one it got.
  globalThis.__h5iPeriodicTimers = function () {
    let periodic = 0;
    for (const timer of timers.values()) if (!timerBlocks(timer)) periodic++;
    return periodic;
  };

  /// When the earliest waiting timer is due, or -1 if none is.
  ///
  /// The settle loop uses this to jump the virtual clock to the next thing that
  /// will actually happen, rather than stepping toward it 16ms at a time. A
  /// page that sets one ten-second timeout should cost one step, not six
  /// hundred and twenty-five, and stepping was not merely slow: it meant a
  /// timer due at the settle budget was never reached at all.
  globalThis.__h5iNextTimerDue = function () {
    let soonest = -1;
    for (const timer of timers.values()) {
      if (soonest < 0 || timer.due < soonest) soonest = timer.due;
    }
    return soonest;
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

  /// How far through loading the document says it is.
  ///
  /// This was the constant `"complete"` until WPT was pointed at the engine.
  /// A constant is the answer that makes the *common* idiom work — the one that
  /// reads `readyState === "loading"` and otherwise initialises immediately —
  /// so every page in §8's four corpora took the immediate branch and nothing
  /// looked wrong. What it hid is that the other branch never arrived, because
  /// no lifecycle event was ever fired at all (§11.5.2).
  let documentReadyState = "loading";

  /// Fire the document lifecycle: DOMContentLoaded, then load.
  ///
  /// Called once by the host after every script in the document has been
  /// evaluated, and before settling, so the callbacks these wake are settled
  /// along with everything else they start.
  ///
  /// Both are dispatched at the root element because that is where this engine
  /// puts `window` and `document` listeners alike, so a page that waits on
  /// either sees them. `load` deliberately does not bubble, matching a browser:
  /// a page that listens for `load` on a container to catch its images must not
  /// be told the document is one.
  /// Expose elements with an `id` as globals, the way a browser does.
  ///
  /// `<div id="target">` makes `window.target` that element. It is a legacy
  /// corner of the platform and it is *everywhere* in test suites and in older
  /// page script: "target is not defined" was the single largest cause of files
  /// this engine could not report on at all — 267 in `css` alone, plus `main`,
  /// `container`, `host` and a long tail. A ReferenceError on the first line
  /// stops a file before it can say anything, which is why one missing legacy
  /// behaviour cost more than most missing APIs.
  ///
  /// Getters rather than values, so the global follows the element if the page
  /// replaces it, and never a name `globalThis` already has: an element with
  /// `id="document"` must not be able to take `document` away from the page.
  ///
  /// The setter is not optional, and leaving it out was a quiet way to hand a
  /// page the wrong value. A getter-only accessor swallows every assignment in
  /// sloppy mode, so with `<div id="thing">` on the page, `var thing = [1,2,3]`
  /// left `thing` as the element and `Array.isArray(thing)` false — the page's
  /// own variable never took. A browser lets the page win: the named property
  /// lives behind the window in the prototype chain, so any assignment creates
  /// an own property that shadows it. Replacing the accessor with a plain data
  /// property on first write is the same behaviour with the machinery this
  /// engine has, and it is what the platform calls a [Replaceable] attribute.
  ///
  /// This also fixes a symptom that looked like something else entirely. A page
  /// that did `var el = document.getElementById("el"); el.remove();` and then
  /// read `el.parentNode` got a TypeError about converting null to an object —
  /// not because `parentNode` was broken on a detached node, but because `el`
  /// was still the *named access* getter, which stops resolving the moment the
  /// element leaves the document. The variable, not the property, was the thing
  /// that had gone.
  globalThis.__h5iInstallNamedAccess = function () {
    for (const id of api.queryAll("[id]", 0)) {
      const name = api.getAttr(id, "id");
      if (!name || !/^[A-Za-z_$][\w$]*$/.test(name)) continue;
      if (name in globalThis) continue;
      Object.defineProperty(globalThis, name, {
        configurable: true,
        enumerable: false,
        get() { return wrap(api.query("#" + cssEscapeIdent(name), 0)); },
        set(value) {
          // Enumerable, because what the page has just created is an ordinary
          // global and should behave like one from here on.
          Object.defineProperty(globalThis, name, {
            configurable: true,
            enumerable: true,
            writable: true,
            value,
          });
        },
      });
    }
  };

  /// Enough escaping to put an id back into a selector safely.
  function cssEscapeIdent(name) {
    return name.replace(/[^\w-]/g, (ch) => "\\" + ch);
  }

  globalThis.__h5iFireLifecycle = function () {
    const root = wrap(api.root());
    const at = (event) => { if (root) root.dispatchEvent(event); };

    // Again here: a script that ran during parsing may have added elements
    // with ids, and the handlers about to run will reach for them by name.
    globalThis.__h5iInstallNamedAccess();

    // Before the first event, not after: `<body onload>` is a handler for the
    // load event dispatched three lines below, so compiling it later would be
    // compiling it too late.
    globalThis.__h5iInstallInlineHandlers();

    documentReadyState = "interactive";
    at(new Event("readystatechange"));
    at(new Event("DOMContentLoaded", { bubbles: true }));

    documentReadyState = "complete";
    at(new Event("readystatechange"));
    at(new Event("load"));
    at(new Event("pageshow"));
  };

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
    /// Hash routing, which a great many single-page applications are built on.
    ///
    /// Assigning to a getter-only property is a silent no-op outside strict
    /// mode, so `location.hash = "/x"` did nothing at all and a hash router
    /// simply never navigated — no error, no route change, no explanation.
    ///
    /// A same-document fragment change moves the address, pushes a history
    /// entry and fires `hashchange`. It deliberately does *not* reload: that is
    /// what makes it a fragment change rather than a navigation, and it is the
    /// whole reason routers use it.
    set hash(value) {
      const wanted = String(value);
      const fragment = wanted.startsWith("#") ? wanted : "#" + wanted;
      const before = currentAddress;
      const parts = api.parseUrl(fragment, currentAddress);
      const next = parts ? parts.href : currentAddress;
      if (next === before) return;
      history.pushState(history.state, "", next);
      const event = new Event("hashchange", { bubbles: false });
      event.oldURL = before;
      event.newURL = next;
      dispatch(wrap(api.root()), event);
    },
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

  // A constructable stylesheet, backed by a real `<style>` element.
  //
  // Design systems build one, fill it with `replaceSync`, and adopt it — and a
  // page that cannot construct one throws before rendering anything. Backing it
  // with a `<style>` in the head means the rules actually reach Stylo, so
  // `display: none` still hides things from the outline, which is the part that
  // changes what an agent reads.
  //
  // `cssRules` is deliberately **not** defined. This engine does not model rules
  // individually, and answering an empty list for a sheet that plainly has rules
  // would be the confident wrong answer it keeps having to refuse — so it goes
  // unanswered, and reports itself.
  /// Split a stylesheet into its top-level rules.
  ///
  /// Brace matching that knows about strings and comments, which is all
  /// `cssRules` needs: where each rule starts and ends. **It is not a CSS
  /// parser and does not pretend to be one** — the cascade is Stylo's, this
  /// only reports the text back in the shape the CSSOM asks for. A declaration
  /// this splitter mis-slices would still be applied correctly to the page,
  /// because the page's styles never come through here.
  function splitRules(css) {
    const found = [];
    let depth = 0, start = 0, index = 0, quote = null;
    while (index < css.length) {
      const ch = css[index];
      if (quote) {
        if (ch === "\\") index++;
        else if (ch === quote) quote = null;
      } else if (ch === '"' || ch === "'") {
        quote = ch;
      } else if (ch === "/" && css[index + 1] === "*") {
        const end = css.indexOf("*/", index + 2);
        index = end < 0 ? css.length : end + 1;
      } else if (ch === "{") {
        depth++;
      } else if (ch === "}") {
        if (--depth <= 0) {
          found.push(css.slice(start, index + 1));
          start = index + 1;
          depth = 0;
        }
      } else if (depth === 0 && ch === ";") {
        // `@import`, `@charset` and friends: a rule with no block at all.
        found.push(css.slice(start, index + 1));
        start = index + 1;
      }
      index++;
    }
    if (start < css.length) found.push(css.slice(start));
    return found.map((text) => text.trim()).filter(Boolean);
  }

  /// Split one rule into its prelude and its body, or null if it has no body.
  function ruleParts(text) {
    let quote = null;
    for (let index = 0; index < text.length; index++) {
      const ch = text[index];
      if (quote) {
        if (ch === "\\") index++;
        else if (ch === quote) quote = null;
      } else if (ch === '"' || ch === "'") {
        quote = ch;
      } else if (ch === "{") {
        const close = text.lastIndexOf("}");
        return { prelude: text.slice(0, index).trim(), body: text.slice(index + 1, close) };
      }
    }
    return null;
  }

  const RULE_TYPES = {
    style: 1, import: 3, media: 4, "font-face": 5, page: 6, keyframes: 7,
    namespace: 10, supports: 12, "counter-style": 11, "font-feature-values": 14,
    layer: 0, container: 0, property: 0, scope: 0, starting: 0,
  };

  class CSSRule {
    constructor(text, sheet) { this._text = text; this._sheet = sheet; }
    get cssText() { return this._text; }
    /// Push this rule's new text back into the stylesheet it belongs to.
    ///
    /// Without this a mutation was a silent no-op: `cssRules` built a fresh
    /// object per access, so `sheet.cssRules[0].style.color = "blue"` wrote to
    /// a throwaway and the sheet still said red — success reported, nothing
    /// changed. CSS-in-JS libraries mutate rules, so this was the worst kind of
    /// wrong: quiet.
    _changed() { if (this._sheet) this._sheet._rewriteFromRules(); }
    get parentStyleSheet() { return this._sheet ?? null; }
    get parentRule() { return null; }
    get type() {
      const parts = ruleParts(this._text) ?? { prelude: this._text };
      if (!parts.prelude.startsWith("@")) return RULE_TYPES.style;
      const name = parts.prelude.slice(1).split(/[\s({]/)[0].toLowerCase();
      return RULE_TYPES[name] ?? 0;
    }
  }

  class CSSStyleRule extends CSSRule {
    get selectorText() { return (ruleParts(this._text)?.prelude ?? "").trim(); }
    set selectorText(value) {
      const parts = ruleParts(this._text);
      if (!parts) return;
      this._text = `${String(value)} {${parts.body}}`;
      this._changed();
    }
    get style() {
      const rule = this;
      return new StyleDeclaration({
        get: () => (ruleParts(rule._text)?.body ?? "").trim(),
        set: (text) => {
          const parts = ruleParts(rule._text);
          if (!parts) return;
          rule._text = `${parts.prelude} { ${text} }`;
          rule._changed();
        },
      });
    }
  }

  /// `@media`, `@supports` and the other rules that contain rules.
  class CSSGroupingRule extends CSSRule {
    get conditionText() { return (ruleParts(this._text)?.prelude ?? "").replace(/^@\w+\s*/, "").trim(); }
    get cssRules() {
      return splitRules(ruleParts(this._text)?.body ?? "").map((text) => makeRule(text, this._sheet));
    }
  }

  function makeRule(text, sheet) {
    const parts = ruleParts(text);
    if (!parts) return new CSSRule(text, sheet);
    if (!parts.prelude.startsWith("@")) return new CSSStyleRule(text, sheet);
    const name = parts.prelude.slice(1).split(/[\s({]/)[0].toLowerCase();
    if (name === "media" || name === "supports" || name === "container"
      || name === "layer" || name === "scope") {
      return new CSSGroupingRule(text, sheet);
    }
    return new CSSRule(text, sheet);
  }

  /// A stylesheet, either constructed by script or belonging to an element.
  ///
  /// Both directions matter and they are not the same object. A constructed
  /// sheet (`new CSSStyleSheet()`, for `adoptedStyleSheets`) *writes* a
  /// `<style>` element into the document. An element's own sheet — `<style>` or
  /// `<link rel=stylesheet>` — *reads* what is already there. Until WPT asked,
  /// only the first existed, so `document.styleSheets` and `el.sheet` were the
  /// two most-wanted CSSOM gaps on the list at 3,779 calls between them.
  class CSSStyleSheet {
    constructor(options) {
      this._text = "";
      this._element = null;
      this._owned = false;
      this.disabled = !!(options && options.disabled);
      if (options && options.media) this._media = String(options.media);
    }

    /// The sheet an element owns, cached on the element so two reads of
    /// `el.sheet` are the same object, as they are in a browser.
    static forElement(element) {
      if (element._sheet) return element._sheet;
      const sheet = new CSSStyleSheet();
      sheet._element = element;
      sheet._owned = true;
      element._sheet = sheet;
      return sheet;
    }

    get ownerNode() { return this._element; }
    get ownerRule() { return null; }
    get parentStyleSheet() { return null; }
    get type() { return "text/css"; }
    get title() {
      const raw = this._element ? api.getAttr(this._element._id, "title") : null;
      return raw || null;
    }
    get media() {
      if (this._media !== undefined) return this._media;
      return (this._element && api.getAttr(this._element._id, "media")) || "";
    }
    /// Null for a `<style>` and for a constructed sheet, as in a browser: only
    /// a sheet that came from a URL has one.
    get href() {
      if (!this._element || this._element.tagName !== "LINK") return null;
      return this._element.href || null;
    }

    _css() {
      if (!this._owned) return this._text;
      // A `<link>`'s bytes were fetched and parsed natively and never reach
      // script, so its rules are not readable here. Empty rather than wrong,
      // and the same answer a browser gives for a cross-origin sheet.
      if (this._element.tagName === "LINK") return "";
      return this._element.textContent || "";
    }

    /// The rules, cached against the text they were parsed from.
    ///
    /// Two reasons, and neither is only speed. A browser's `cssRules` hands
    /// back the same object every time, so a page that keeps a rule and mutates
    /// it later must keep hold of something real; and re-splitting on every
    /// index made a loop over the rules quadratic in the size of the sheet.
    get cssRules() {
      const css = this._css();
      if (this._rulesFor !== css) {
        this._rulesFor = css;
        this._rules = splitRules(css).map((text) => makeRule(text, this));
      }
      return this._rules;
    }
    get rules() { return this.cssRules; }

    /// Re-serialise the cached rules after one of them changed.
    ///
    /// `_rulesFor` is set to the text we just wrote, so the next read finds the
    /// cache warm and the caller keeps the rule object it is holding.
    _rewriteFromRules() {
      if (!this._rules) return;
      const text = this._rules.map((rule) => rule.cssText).join("\n");
      this._rulesFor = text;
      this._replaceAll(text);
    }

    replaceSync(text) {
      this._text = String(text);
      this._apply();
    }
    replace(text) {
      this.replaceSync(text);
      return Promise.resolve(this);
    }
    insertRule(rule, index) {
      const rules = splitRules(this._css());
      const at = index === undefined ? 0 : Math.min(Number(index) || 0, rules.length);
      rules.splice(at, 0, String(rule));
      this._replaceAll(rules.join("\n"));
      return at;
    }
    deleteRule(index) {
      const rules = splitRules(this._css());
      const at = Number(index) || 0;
      if (at < 0 || at >= rules.length) {
        throw new DOMException(`there is no rule ${at} to delete`, "IndexSizeError");
      }
      rules.splice(at, 1);
      this._replaceAll(rules.join("\n"));
    }
    _replaceAll(text) {
      if (this._owned) {
        // A `<link>`'s bytes were fetched and parsed natively and never reach
        // script, so there is nothing here to edit. Refused loudly rather than
        // quietly: `insertRule` used to answer 0 and change nothing, which is a
        // page believing it had installed a style.
        if (this._element.tagName === "LINK") {
          throw new DOMException(
            "this stylesheet came from a <link> and its rules are not editable here",
            "NoModificationAllowedError",
          );
        }
        this._element.textContent = text;
        return;
      }
      this._text = text;
      this._apply();
    }
    _apply() {
      if (!this._element) {
        const head = wrap(api.query("head", 0)) || document.body;
        if (!head) return;
        this._element = document.createElement("style");
        head.appendChild(this._element);
      }
      this._element.textContent = this._text;
    }
  }

  /// Every sheet the document owns, in document order.
  ///
  /// Indexed as well as iterable, because `document.styleSheets[0]` is how
  /// almost every caller reaches for it.
  function styleSheetList() {
    const sheets = api
      .queryAll("style, link[rel~=stylesheet i]", 0)
      .map(wrap)
      .filter(Boolean)
      .map((element) => CSSStyleSheet.forElement(element));
    const list = {
      get length() { return sheets.length; },
      item: (index) => sheets[index] ?? null,
      [Symbol.iterator]: () => sheets[Symbol.iterator](),
    };
    sheets.forEach((sheet, index) => { list[index] = sheet; });
    return list;
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
    /// Validates its label, and decodes as *that* label.
    ///
    /// Both used to be wrong rather than missing: every label was accepted and
    /// every one answered "utf-8", so a page asking whether an encoding was
    /// supported was told yes and a page decoding Shift-JIS got mojibake with
    /// no error. The label table and the decoders are `encoding_rs`'s, which is
    /// the same table the standard defines rather than a list of our own that
    /// would drift from it.
    constructor(label, options) {
      const wanted = label === undefined ? "utf-8" : String(label);
      const canonical = api.encodingFor(wanted);
      if (canonical === null || canonical === undefined) {
        throw new RangeError(`${wanted} is not a known encoding`);
      }
      // `replacement` exists only to be refused, and decoding as it is not a
      // thing a caller can ask for.
      this._encoding = canonical;
      this._fatal = !!(options && options.fatal);
      this._ignoreBOM = !!(options && options.ignoreBOM);
    }
    get encoding() { return this._encoding; }
    get fatal() { return this._fatal; }
    get ignoreBOM() { return this._ignoreBOM; }
    decode(input) {
      if (input === undefined || input === null) return "";
      // Anything byte-shaped: a typed array, an ArrayBuffer, or a plain array.
      const bytes = input instanceof Uint8Array
        ? input
        : new Uint8Array(input.buffer ? input.buffer : input);
      return api.decodeBytes(this._encoding, Array.from(bytes), this._fatal);
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

  /// The `CSS` namespace: `CSS.escape` and `CSS.supports`.
  ///
  /// `supports` is answered by actually handing the declaration to Stylo rather
  /// than by consulting a list, because a list is a second opinion about what
  /// this engine supports and the two would drift the moment Stylo moved. It
  /// also matters more than most answers: pages call `CSS.supports` in order to
  /// take a *different code path*, so a wrong answer does not degrade the page,
  /// it misroutes it.
  const CSS = observed({
    escape(value) {
      // The spec's algorithm, which is not `encodeURIComponent` and not a
      // regex over "special characters": the rules for a leading digit, a
      // leading hyphen-digit, and NULL are each different, and a selector
      // built from a wrong escape silently matches nothing.
      const text = String(value);
      let out = "";
      for (let index = 0; index < text.length; index++) {
        const code = text.charCodeAt(index);
        const ch = text[index];
        if (code === 0) { out += "\uFFFD"; continue; }
        if ((code >= 0x1 && code <= 0x1f) || code === 0x7f
          || (index === 0 && code >= 0x30 && code <= 0x39)
          || (index === 1 && code >= 0x30 && code <= 0x39 && text.charCodeAt(0) === 0x2d)) {
          out += "\\" + code.toString(16) + " ";
          continue;
        }
        if (index === 0 && code === 0x2d && text.length === 1) { out += "\\" + ch; continue; }
        if (code >= 0x80 || code === 0x2d || code === 0x5f
          || (code >= 0x30 && code <= 0x39) || (code >= 0x41 && code <= 0x5a)
          || (code >= 0x61 && code <= 0x7a)) {
          out += ch;
          continue;
        }
        out += "\\" + ch;
      }
      return out;
    },
    supports(propertyOrCondition, value) {
      if (value !== undefined) {
        return api.supportsCss(String(propertyOrCondition), String(value));
      }
      // The one-argument form takes a condition, `(display: grid)`. Only the
      // plain parenthesised declaration is answered; `and`/`or`/`not` are a
      // grammar rather than a declaration and are named rather than guessed.
      const text = String(propertyOrCondition).trim();
      const match = /^\(\s*([-\w]+)\s*:\s*([^]*?)\s*\)$/.exec(text);
      if (!match) {
        api.unsupported(`CSS.supports(${text.slice(0, 40)})`);
        return false;
      }
      return api.supportsCss(match[1], match[2]);
    },
  }, "CSS");

  Object.assign(globalThis, {
    CSS,
    addEventListener, removeEventListener, dispatchEvent,
    window,
    // The browsing context's view of itself. These are not stubs: §6 refuses
    // iframes and popups, so this document is always a top-level context with
    // no children, and every value below is what a real browser reports for
    // one. `self` in particular gates a great deal of library code — the whole
    // of testharness.js walks `w != w.parent` from `self` before it can run a
    // single assertion, so its absence read as "the engine cannot run WPT"
    // rather than as one missing binding.
    getSelection,
    Selection,
    self: window,
    parent: window,
    top: window,
    frames: window,
    length: 0,
    frameElement: null,
    opener: null,
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
    EventTarget, DOMParser, CSSStyleSheet, ShadowRoot,
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
          // `"color" in getComputedStyle(el)` asks `has`, not `get`, and
          // without this trap it fell through to the bare backing object and
          // answered **false for every property**. A computed style declares
          // every property the engine knows, so that is what it now reports.
          //
          // WPT's `test_computed_value` asserts this on its first line and it
          // is the standard helper for CSS parsing tests, so thousands of
          // subtests failed before comparing a single value — all of
          // `css-color` among them, where Stylo already supported every
          // feature under test.
          has(target, key) {
            if (typeof key !== "string") return Reflect.has(target, key);
            if (key in target) return true;
            return api.isCssProperty(camelToDash(key));
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
    // The origin story travels with the request. The host decides what may be
    // sent and what may be read from it; this side only reports what the page
    // asked for, because a page that could choose its own answer to those
    // questions would not be subject to a policy at all.
    const headerPairs = [];
    for (const [name, value] of request.headers) headerPairs.push([name, value]);
    const id = api.fetchStart(
      request.url, request.method, body,
      request.mode, request.credentials, headerPairs,
    );
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
      // What a page checks to find out it was handed an opaque response
      // rather than a failed one. Reported rather than left to be inferred
      // from an empty body with status 0, which reads as a network error.
      type: res.opaque ? "opaque" : "basic",
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
