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
    // Re-entrant construction — a custom element's constructor asking the
    // document for itself — gets a plain wrapper rather than recursing forever.
    if (constructing.has(id)) return observed(new Element(id), "Element");


    let raw;
    let label;
    if (comments.has(id)) { raw = new Comment(id); label = "Comment"; }
    else if (api.isElement(id)) { raw = constructElement(id); label = "Element"; }
    else { raw = new Text(id); label = "Text"; }

    // Labelled by what the node actually is. Calling a text node "Element"
    // reported `Element.tagName` as missing when what happened was a page
    // reading `tagName` off a text node, where no engine has one.
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
    const visit = (n) => {
      if (!n || n.nodeType !== 1) return;
      if (isCustom(n)) found.push(n);
      for (const kid of n.children) visit(kid);
    };
    visit(node);
    return found;
  }

  function notifyConnection(node) {
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
          return Reflect.get(object, property, receiver);
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

        api.unsupported(`${label}.${String(property)}`);
        return undefined;
      },
    });
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
    constructor(id) {
      // `undefined` means "the element currently being upgraded". A custom
      // element's constructor runs as `super()` with no arguments — the class
      // never sees the node it is being attached to — so the id arrives out of
      // band, exactly as the construction stack works in a real engine.
      this._id = id === undefined ? upgrading : id;
    }

    get ownerDocument() { return document; }
    get isConnected() {
      const root = api.root();
      for (let n = this._id; n !== null && n !== undefined; n = api.parent(n)) {
        if (n === root) return true;
      }
      return false;
    }
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

    get nodeType() { return api.isElement(this._id) ? 1 : 3; }
    get parentNode() { return wrap(api.parent(this._id)); }
    get parentElement() { return this.parentNode; }
    get childNodes() { return api.children(this._id).map(wrap); }
    get firstChild() { return this.childNodes[0] || null; }
    get lastChild() { const c = this.childNodes; return c[c.length - 1] || null; }

    // Text for a text node, null for an element — the distinction is the whole
    // reason the property exists, and code that walks a tree branches on it.
    get nodeValue() { return this.nodeType === 3 ? api.getText(this._id) : null; }
    set nodeValue(value) {
      if (this.nodeType === 3) this.textContent = value;
    }

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
      notifyConnection(child);
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
      notifyConnection(child);
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
    querySelector() { api.unsupported("DocumentFragment.querySelector"); return null; }
  }

  class Text extends Node {
    get data() { return this.textContent; }
    set data(v) { this.textContent = v; }
  }

  // A real comment node, not a text node wearing a hat: a marker that showed up
  // in `textContent` would appear in the outline an agent reads.
  class Comment extends Node {
    get nodeType() { return 8; }
    get nodeName() { return "#comment"; }
    get data() { return api.getText(this._id); }
    get nodeValue() { return api.getText(this._id); }
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
      fireAttributeChanged(this, String(name).toLowerCase(), previous, String(value));
    }
    removeAttribute(name) {
      const previous = api.getAttr(this._id, String(name));
      api.removeAttr(this._id, String(name));
      record({
        type: "attributes", target: this, addedNodes: [], removedNodes: [],
        attributeName: String(name).toLowerCase(), oldValue: previous,
      });
      fireAttributeChanged(this, String(name).toLowerCase(), previous, null);
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
      const parts = api.parseUrl(String(raw), globalThis.__h5iUrl);
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
      const parts = api.parseUrl(String(raw), globalThis.__h5iUrl);
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
    get scrollHeight() { return (api.scrollMetrics(this._id) || [0, 0, 0])[2]; }
    get scrollWidth() { return (api.scrollMetrics(this._id) || [0, 0, 0, 0])[3]; }

    get lang() { return api.getAttr(this._id, "lang") || ""; }
    set lang(v) { this.setAttribute("lang", v); }
    get title() { return api.getAttr(this._id, "title") || ""; }
    set title(v) { this.setAttribute("title", v); }
    get alt() { return api.getAttr(this._id, "alt") || ""; }
    set alt(v) { this.setAttribute("alt", v); }

    // Bring the element into view for a screenshot or a live viewer. The
    // outline an agent reads covers the whole document either way, so this
    // changes what a *human* watching sees and nothing about what is readable.
    scrollIntoView() { api.scrollToNode(this._id); }

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
    getElementsByTagName(tag) { return api.queryAll(String(tag), this._id).map(wrap); }
    getElementsByClassName(cls) { return api.queryAll("." + String(cls), this._id).map(wrap); }

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
    // Not the bounding rect: for `documentElement` and `body` this is the
    // *viewport*, not the element's own height, and the bottom-of-page check
    // every page writes compares the two.
    get clientWidth() { return (api.scrollMetrics(this._id) || [0, 0, 0, 0, 0, 0])[5]; }
    get clientHeight() { return (api.scrollMetrics(this._id) || [0, 0, 0, 0, 0])[4]; }
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

  const documentImpl = {
    get documentElement() { return wrap(api.root()); },
    get body() { return wrap(api.body()); },
    get head() { return wrap(api.query("head", 0)); },
    createElement(tag) { return wrap(api.createElement(String(tag))); },
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
    getElementsByName(name) {
      return api.queryAll(`[name="${String(name).replace(/"/g, '\\"')}"]`, 0).map(wrap);
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
    get URL() { return globalThis.__h5iUrl; },
    get documentURI() { return globalThis.__h5iUrl; },

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
      return {
        hasFeature: () => true,
        // A second document is genuinely out of reach here — there is one tree,
        // and it is the page. A page using this to parse HTML off to the side
        // gets a named refusal instead of a silently broken document.
        createHTMLDocument: missingApi("document.implementation.createHTMLDocument"),
        createDocument: missingApi("document.implementation.createDocument"),
        createDocumentType: missingApi("document.implementation.createDocumentType"),
      };
    },

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
    setInterval, clearInterval: clearTimeout,
    requestAnimationFrame: (fn) => setTimeout(() => fn(clock), 16),
    cancelAnimationFrame: clearTimeout,
    Node, Element, Text, Event,
    alert: () => api.unsupported("alert"),
    matchMedia,
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

    // The constructors, exposed for `instanceof` — which is how library code
    // asks "is this a node?" before deciding what to do with it. `HTMLElement`
    // is `Element` here because this engine has one element class; the check
    // that matters is the one pages actually write.
    Node, Element, Text, Comment, DocumentFragment,
    HTMLElement: Element,
    customElements, NodeFilter, NodeIterator, TreeWalker,

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
