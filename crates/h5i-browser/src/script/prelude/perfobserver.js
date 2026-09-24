// `PerformanceObserver`, for the libraries that measure a page while it loads.
//
// Its own source, parsed only when a page reads the name — see `TIERS` in
// `mod.rs`. Sentry's tracing integration constructs one during `setup` without
// checking for it first, so on grok.com the missing global was the first thing
// the page threw, before any of its own code ran.
//
// `supportedEntryTypes` lists what this engine can actually deliver, which is
// what a library reads to decide what to watch. Naming a type here that never
// arrives would be worse than not having the interface: `onLCP` would install a
// handler and wait for a metric that is never coming, and the page would report
// itself as still measuring for ever. `mark` and `measure` are the two the
// `performance` object really produces.
(function () {
  "use strict";

  const SUPPORTED = ["mark", "measure"];

  /// What a callback receives. A list, with the two readers the spec gives it.
  class PerformanceObserverEntryList {
    constructor(entries) {
      this.__entries = entries;
    }
    getEntries() { return this.__entries.slice(); }
    getEntriesByType(type) {
      return this.__entries.filter((e) => e.entryType === String(type));
    }
    getEntriesByName(name, type) {
      return this.__entries.filter(
        (e) => e.name === String(name) && (type === undefined || e.entryType === String(type)),
      );
    }
  }

  class PerformanceObserver {
    constructor(callback) {
      if (typeof callback !== "function") {
        throw new TypeError("PerformanceObserver requires a callback");
      }
      this.__callback = callback;
      this.__watching = new Set();
      this.__queued = [];
    }

    /// `{ type }` and `{ entryTypes }` are the two forms, and the spec forbids
    /// mixing them. `buffered` replays what the timeline already holds, which is
    /// the whole reason a library that starts late still sees early entries.
    observe(options) {
      const opts = options ?? {};
      if ("entryTypes" in opts && "type" in opts) {
        throw new TypeError("PerformanceObserver.observe takes type or entryTypes, not both");
      }
      const types = "entryTypes" in opts
        ? Array.from(opts.entryTypes ?? []).map(String)
        : "type" in opts ? [String(opts.type)] : [];
      // A call naming only unsupported types is a no-op rather than an error,
      // which is what lets `onLCP` and friends fall silent instead of throwing.
      if ("entryTypes" in opts) this.__watching.clear();
      for (const type of types) {
        if (!SUPPORTED.includes(type)) continue;
        this.__watching.add(type);
        if (opts.buffered) {
          for (const entry of globalThis.performance.getEntriesByType(type)) {
            this.__queued.push(entry);
          }
        }
      }
      globalThis.__h5iAddPerfObserver(this);
      if (this.__queued.length > 0) this.__deliverSoon();
    }

    disconnect() {
      this.__watching.clear();
      this.__queued.length = 0;
      globalThis.__h5iRemovePerfObserver(this);
    }

    takeRecords() {
      const taken = this.__queued.slice();
      this.__queued.length = 0;
      return taken;
    }

    /// Called by the `performance` object when it produces an entry.
    __offer(entry) {
      if (!this.__watching.has(entry.entryType)) return;
      this.__queued.push(entry);
      this.__deliverSoon();
    }

    /// A task, not a synchronous call: the spec delivers records in a queued
    /// task, and a page that marks inside its own callback would otherwise
    /// re-enter that callback while it was still running.
    __deliverSoon() {
      if (this.__pending) return;
      this.__pending = true;
      globalThis.setTimeout(() => {
        this.__pending = false;
        const taken = this.takeRecords();
        if (taken.length === 0) return;
        try {
          this.__callback(new PerformanceObserverEntryList(taken), this);
        } catch (error) {
          console.error("PerformanceObserver callback threw: " + error);
        }
      }, 0);
    }
  }

  Object.defineProperty(PerformanceObserver, "supportedEntryTypes", {
    get() { return SUPPORTED.slice(); },
    configurable: true,
  });

  for (const [name, value] of [
    ["PerformanceObserver", PerformanceObserver],
    ["PerformanceObserverEntryList", PerformanceObserverEntryList],
  ]) {
    Object.defineProperty(globalThis, name, {
      value, writable: true, enumerable: false, configurable: true,
    });
  }
})();
