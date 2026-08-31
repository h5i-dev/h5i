// The display, for a session whose identity declares one.
//
// Its own source, and loaded on a condition rather than on a property read.
// That distinction is the whole reason this file exists rather than sitting in
// the core prelude or behind `lazyGlobals`:
//
//   - Not in the core, because `native` and `privacy` declare no display and
//     most sessions are one of those. They should not parse or run a Screen
//     interface they will never expose.
//   - Not behind `lazyGlobals`, because that installs an accessor on the global
//     and turns it into a data property on first read. For `WebSocket` that is
//     invisible and free. For `screen` it would be the exact tell this whole
//     feature exists to avoid: a page that reads
//     `Object.getOwnPropertyDescriptor(window, "screen")` before touching it
//     would find a getter whose `toString` is this engine's own source, where
//     every browser has a plain value. A fingerprinting script reads
//     descriptors; it is the first thing it does.
//
// So the core prelude loads this the moment it knows the identity declares a
// display, before the page runs a statement. A session that pays for it gets
// `screen` as an ordinary own property that was simply always there, and a
// session that does not never sees the file.
//
// The numbers are the identity's, and they were validated at the session's
// door: `availWidth <= width`, a colour depth a browser actually reports, and
// a viewport that fits inside the display. See `identity.rs`.
(function () {
  "use strict";

  const identity = globalThis.__h5i.identity();
  if (!identity.screen) return;

  // Accessors rather than data properties, because that is what the interface
  // is: every member of `Screen` is a `readonly attribute`, so a page that
  // assigns to `screen.width` must find the assignment ignored rather than see
  // it stick — and idlharness reads the descriptor to check.
  class Screen {
    constructor() { throw new TypeError("Illegal constructor"); }
  }

  const values = Object.freeze({
    width: identity.screen.width,
    height: identity.screen.height,
    availWidth: identity.screen.availWidth,
    availHeight: identity.screen.availHeight,
    colorDepth: identity.screen.colorDepth,
    // The same number as the colour depth, on every browser that ships. A
    // `pixelDepth` that disagreed with it would be a pairing nothing reports.
    pixelDepth: identity.screen.colorDepth,
    // 0 and 0: the identity states a work area by *size*, and an origin would
    // be a second, unstated fact about where the system chrome sits.
    availLeft: 0,
    availTop: 0,
  });

  for (const name of Object.keys(values)) {
    Object.defineProperty(Screen.prototype, name, {
      configurable: true,
      enumerable: true,
      get() {
        if (!(this instanceof Screen) || this === Screen.prototype) {
          throw new TypeError(`Illegal invocation: ${name} needs a Screen`);
        }
        return values[name];
      },
    });
  }
  Object.defineProperty(Screen.prototype, Symbol.toStringTag, {
    value: "Screen", configurable: true,
  });

  // The interface object and the instance together, or neither. No browser has
  // the class without the object, so exposing one alone would be its own tell.
  Object.defineProperty(globalThis, "screen", {
    value: Object.create(Screen.prototype),
    writable: true, enumerable: true, configurable: true,
  });
  Object.defineProperty(globalThis, "Screen", {
    value: Screen,
    // Not enumerable: WebIDL §3.7, and the same rule the core prelude's own
    // enumerability pass applies — a pass that has long since run by the time
    // this file loads, which is why it is set here by hand.
    writable: true, enumerable: false, configurable: true,
  });
})();
