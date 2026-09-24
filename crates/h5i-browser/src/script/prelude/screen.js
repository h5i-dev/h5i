// The display.
//
// A declared identity states its own geometry. Without one the numbers are the
// viewport this engine laid the page out at: a stated fact about this run rather
// than a guess, and what `innerWidth` already answers with. Absent was worse —
// every browser has `window.screen`, so reading `screen.width` threw a TypeError
// no browser produces.
(function () {
  "use strict";

  // Guarded the way the core prelude guards it: `api.identity` is behind the
  // `identity` feature, and the build the smoke lane uses turns it off
  // (`--no-default-features --features browser`). This tier used to load only
  // when an identity declared a display, so it never ran in that build; loading
  // it always meant an absent `identity()` threw here and took the whole prelude
  // down with it, on every page.
  const api = globalThis.__h5i;
  const declared = api.identity ? api.identity().screen : undefined;
  const viewport = api.viewport();

  // Accessors rather than data properties, because that is what the interface
  // is: every member of `Screen` is a `readonly attribute`, so a page that
  // assigns to `screen.width` must find the assignment ignored rather than see
  // it stick — and idlharness reads the descriptor to check.
  class Screen {
    constructor() { throw new TypeError("Illegal constructor"); }
  }

  // 24 for an undeclared display, because that is the only depth any browser
  // in use reports; it is a constant rather than a measurement.
  const depth = declared ? declared.colorDepth : 24;
  const values = Object.freeze({
    width: declared ? declared.width : viewport.width,
    height: declared ? declared.height : viewport.height,
    // No system chrome to subtract when nothing was declared, so the work area
    // is the whole of it.
    availWidth: declared ? declared.availWidth : viewport.width,
    availHeight: declared ? declared.availHeight : viewport.height,
    colorDepth: depth,
    // The same number as the colour depth, on every browser that ships. A
    // `pixelDepth` that disagreed with it would be a pairing nothing reports.
    pixelDepth: depth,
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
