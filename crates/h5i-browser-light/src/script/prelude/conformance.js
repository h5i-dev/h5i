// The WebIDL member decoration, which only a conformance harness observes.
//
// Parsed and evaluated only when `RealmOptions::webidl_conformance` is set,
// which `wpt/run.py` sets and nothing else does. It lived in the core prelude
// until it was measured: rebuilding every descriptor of every interface
// prototype cost **15 ms of the 83 ms** a realm took, on every page, for two
// properties no page reads. See `TIERS` in `mod.rs` for the rule this file is
// an instance of.
//
// A separate `eval` has no way into the core's closure, so what it needs
// arrives through `__h5iInternals`: the interfaces that are not reachable by
// name at the point this runs, and the tag-to-interface map.
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
})();
