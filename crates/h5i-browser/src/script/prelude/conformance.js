// The WebIDL member decoration, which only a conformance harness observes.
(function () {
  "use strict";

  const internals = globalThis.__h5iInternals;

  // ── WebIDL member polish ─────────────────────────────────────────────
  //
  // Two properties of every interface member that idlharness checks and class
  // syntax does not give: members are enumerable, and an accessor reached on
  // the prototype object itself throws TypeError instead of running against
  // nothing. The reflection tables already emit both; this pass brings the
  // members written as plain class accessors and methods up to the same
  // contract. Underscore names are internal machinery and stay out of
  // enumeration.
  //
  // The `get x`/`set x` naming below is *maintenance*, not one of the two:
  // Boa already names class accessors that way, and rebuilding the descriptor
  // here would lose the name if it were not put back.
  const polish = (Interface) => {
    const proto = Interface && Interface.prototype;
    if (!proto) return;
    for (const key of Object.getOwnPropertyNames(proto)) {
      if (key === "constructor" || key.startsWith("_")) continue;
      const desc = Object.getOwnPropertyDescriptor(proto, key);
      if (!desc.configurable) continue;
      desc.enumerable = true;
      if (desc.get) {
        const inner = desc.get;
        desc.get = function () {
          // The WebIDL brand check: the prototype itself and any object
          // that is not an instance of this interface both get the
          // TypeError — `desc.get.call({})` is idlharness's own probe.
          if (this === proto || !(this instanceof Interface)) {
            throw new TypeError(`Illegal invocation: ${key} needs an instance`);
          }
          return inner.call(this);
        };
        Object.defineProperty(desc.get, "name", { value: `get ${key}` });
      }
      if (desc.set) {
        const inner = desc.set;
        desc.set = function (value) {
          if (this === proto || !(this instanceof Interface)) {
            throw new TypeError(`Illegal invocation: ${key} needs an instance`);
          }
          return inner.call(this, value);
        };
        Object.defineProperty(desc.set, "name", { value: `set ${key}` });
      }
      Object.defineProperty(proto, key, desc);
    }
  };
  for (const Interface of [
    ...internals.polishTargets,
    ...new Set(internals.TAG_CLASSES.values()),
  ]) {
    polish(Interface);
  }

  // ── The singletons, mirrored onto their interface prototypes ─────────
  //
  // WebIDL puts an interface's members on its *prototype*; this engine's
  // singletons are object literals, so the members lived on the instance and
  // `Navigator.prototype` — the object WebIDL says owns `userAgent` — was
  // empty. That is §B22.3's finding in different clothes: the implementation
  // was real and the interface was a shell.
  //
  // `documentConstructor` already solved it for `Document` by mirroring the
  // live object's surface onto the prototype as forwarding members. This is
  // that, generalised — and it belongs in this tier rather than the core for
  // the same reason as everything else here: the instances keep their own
  // properties, so no page ever reaches these accessors, and mirroring them
  // eagerly cost 2 KiB of parse for a shape only idlharness inspects.
  //
  // Each forwarded member carries the brand guard WebIDL requires: reaching
  // `Navigator.prototype.userAgent` with the prototype as `this` throws
  // TypeError rather than answering for the singleton, which is the assertion
  // idlharness makes right after it finds the member.
  //
  // `Window` is deliberately **not** mirrored. It is a `[Global]` interface,
  // and WebIDL puts a [Global] interface's members on the global object as own
  // properties *instead of* on the prototype — idlharness asserts the
  // prototype must not carry them. Mirroring it was measured: 12 subtests lost
  // and none won.
  const mirror = (Interface, source, isInstance) => {
    if (typeof Interface !== "function" || !source) return;
    const proto = Interface.prototype;
    const label = Interface.name;
    for (const [key, d] of Object.entries(Object.getOwnPropertyDescriptors(source))) {
      if (key.startsWith("_") || key === "constructor") continue;
      if (Object.prototype.hasOwnProperty.call(proto, key)) continue;
      const guard = (that) => {
        if (!isInstance(that)) {
          throw new TypeError(`Illegal invocation: ${key} needs a ${label}`);
        }
      };
      const forwarded = { configurable: true, enumerable: true };
      if (d.get || d.set) {
        forwarded.get = function () { guard(this); return source[key]; };
        Object.defineProperty(forwarded.get, "name", { value: `get ${key}` });
        if (d.set) {
          forwarded.set = function (value) { guard(this); source[key] = value; };
          Object.defineProperty(forwarded.set, "name", { value: `set ${key}` });
        }
      } else if (typeof d.value === "function") {
        forwarded.writable = true;
        forwarded.value = function (...args) { guard(this); return source[key](...args); };
        Object.defineProperty(forwarded.value, "name", { value: key });
        Object.defineProperty(forwarded.value, "length", {
          value: d.value.length, writable: false, enumerable: false, configurable: true,
        });
      } else {
        // A getter and **no setter**: a data property here is almost always a
        // `readonly attribute`, and idlharness asserts the setter is undefined
        // for those. Mirroring one cost 19 subtests on `Navigator` alone
        // against the 11 the getters won back.
        forwarded.get = function () { guard(this); return source[key]; };
        Object.defineProperty(forwarded.get, "name", { value: `get ${key}` });
      }
      Object.defineProperty(proto, key, forwarded);
    }
  };
  // Deferred, because this tier is evaluated from the middle of the core
  // prelude — the polish above needs the prototypes in the state the core
  // leaves them in — and `navigator`, `history`, `location` and their kin are
  // installed on the global *after* that point. Mirroring here would find
  // nothing and silently do nothing, which is how the first version of this
  // lost every subtest it had won. The core calls this back once the globals
  // are up.
  internals.mirrorSingletons = () => {
    const g = globalThis;
    mirror(g.Navigator, g.navigator, (v) => v === g.navigator);
    mirror(g.History, g.history, (v) => v === g.history);
    mirror(g.Location, g.location, (v) => v === g.location);
    mirror(g.Performance, g.performance, (v) => v === g.performance);
    mirror(g.CustomElementRegistry, g.customElements, (v) => v === g.customElements);
    mirror(g.Storage, g.localStorage, (v) => v === g.localStorage || v === g.sessionStorage);
  };
})();
