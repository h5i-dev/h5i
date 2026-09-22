// IndexedDB: `indexedDB`, its databases, stores, indexes and cursors.
//
// Its own source, parsed only when a page reads one of those names — see
// `TIERS` in `mod.rs`. Most pages never open a database; the ones that do are
// applications, and for them the name being absent is fatal rather than
// degrading: the store is where their state lives.
//
// In memory, for the life of the realm, which is what `localStorage` is here
// too. Nothing is written to disk and nothing survives the session, so a page
// that stores and reads back within one run behaves; one that expects to find
// last week's data finds an empty database, exactly as a first visit would.
(function () {
  "use strict";

  const { Event, EventTarget, DOMException, structuredClone, console } = globalThis;
  const { withStack } = globalThis.__h5iInternals;

  // ── keys ─────────────────────────────────────────────────────────────────
  //
  // The standard gives keys a total order across types, and everything else
  // here (cursors, ranges, index lookups) is defined in terms of it: number <
  // date < string < binary < array, and arrays compare element by element.

  const KIND_NUMBER = 0;
  const KIND_DATE = 1;
  const KIND_STRING = 2;
  const KIND_BINARY = 3;
  const KIND_ARRAY = 4;

  function isBinary(value) {
    return value instanceof ArrayBuffer || ArrayBuffer.isView(value);
  }

  function bytesOf(value) {
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }

  /// The key a value *is*, or `undefined` when it is not a key at all.
  ///
  /// Dates and binary are copied on the way in: a key held by the store must
  /// not change because the page mutated the object it passed.
  function toKey(value, seen) {
    if (typeof value === "number") {
      if (Number.isNaN(value)) return undefined;
      return { kind: KIND_NUMBER, value };
    }
    if (value instanceof Date) {
      const at = value.getTime();
      if (Number.isNaN(at)) return undefined;
      return { kind: KIND_DATE, value: at };
    }
    if (typeof value === "string") return { kind: KIND_STRING, value };
    if (isBinary(value)) return { kind: KIND_BINARY, value: bytesOf(value).slice() };
    if (Array.isArray(value)) {
      // A cycle would otherwise recurse until the stack gave out, and the
      // standard calls it an invalid key rather than an error to survive.
      const visited = seen || new Set();
      if (visited.has(value)) return undefined;
      visited.add(value);
      const parts = [];
      for (const item of value) {
        const part = toKey(item, visited);
        if (part === undefined) return undefined;
        parts.push(part);
      }
      visited.delete(value);
      return { kind: KIND_ARRAY, value: parts };
    }
    return undefined;
  }

  function compareKeys(a, b) {
    if (a.kind !== b.kind) return a.kind < b.kind ? -1 : 1;
    if (a.kind === KIND_ARRAY) {
      const length = Math.min(a.value.length, b.value.length);
      for (let i = 0; i < length; i++) {
        const order = compareKeys(a.value[i], b.value[i]);
        if (order !== 0) return order;
      }
      if (a.value.length === b.value.length) return 0;
      return a.value.length < b.value.length ? -1 : 1;
    }
    if (a.kind === KIND_BINARY) {
      const length = Math.min(a.value.length, b.value.length);
      for (let i = 0; i < length; i++) {
        if (a.value[i] !== b.value[i]) return a.value[i] < b.value[i] ? -1 : 1;
      }
      if (a.value.length === b.value.length) return 0;
      return a.value.length < b.value.length ? -1 : 1;
    }
    if (a.value === b.value) return 0;
    return a.value < b.value ? -1 : 1;
  }

  /// The key as the page handed it in, for handing back.
  function fromKey(key) {
    if (key === undefined) return undefined;
    switch (key.kind) {
      case KIND_DATE:
        return new Date(key.value);
      case KIND_BINARY:
        return key.value.slice().buffer;
      case KIND_ARRAY:
        return key.value.map(fromKey);
      default:
        return key.value;
    }
  }

  function demandKey(value, what) {
    const key = toKey(value);
    if (key === undefined) {
      throw new DOMException(`${what} is not a valid key`, "DataError");
    }
    return key;
  }

  // ── key paths ────────────────────────────────────────────────────────────

  function evaluatePath(value, path) {
    if (Array.isArray(path)) {
      const parts = [];
      for (const one of path) {
        const part = evaluatePath(value, one);
        if (part === undefined) return undefined;
        parts.push(part);
      }
      return parts;
    }
    if (path === "") return value;
    let at = value;
    for (const step of String(path).split(".")) {
      if (at === null || at === undefined) return undefined;
      at = at[step];
    }
    return at;
  }

  /// Write a generated key back into the value, which is what `autoIncrement`
  /// with a `keyPath` means: the record carries the key the store chose.
  function injectPath(value, path, key) {
    const steps = String(path).split(".");
    let at = value;
    for (let i = 0; i < steps.length - 1; i++) {
      if (at[steps[i]] === null || typeof at[steps[i]] !== "object") at[steps[i]] = {};
      at = at[steps[i]];
    }
    at[steps[steps.length - 1]] = key;
  }

  // ── IDBKeyRange ──────────────────────────────────────────────────────────

  class IDBKeyRange {
    constructor(lower, upper, lowerOpen, upperOpen) {
      this._lower = lower;
      this._upper = upper;
      this._lowerOpen = !!lowerOpen;
      this._upperOpen = !!upperOpen;
    }
    get lower() { return fromKey(this._lower); }
    get upper() { return fromKey(this._upper); }
    get lowerOpen() { return this._lowerOpen; }
    get upperOpen() { return this._upperOpen; }
    static only(value) {
      const key = demandKey(value, "the key");
      return new IDBKeyRange(key, key, false, false);
    }
    static lowerBound(value, open) {
      return new IDBKeyRange(demandKey(value, "the bound"), undefined, open, false);
    }
    static upperBound(value, open) {
      return new IDBKeyRange(undefined, demandKey(value, "the bound"), false, open);
    }
    static bound(lower, upper, lowerOpen, upperOpen) {
      const low = demandKey(lower, "the lower bound");
      const high = demandKey(upper, "the upper bound");
      const order = compareKeys(low, high);
      if (order > 0 || (order === 0 && (lowerOpen || upperOpen))) {
        throw new DOMException("the lower bound is above the upper bound", "DataError");
      }
      return new IDBKeyRange(low, high, lowerOpen, upperOpen);
    }
    includes(value) { return rangeHolds(this, demandKey(value, "the key")); }
  }

  function rangeHolds(range, key) {
    if (range._lower !== undefined) {
      const order = compareKeys(key, range._lower);
      if (order < 0 || (order === 0 && range._lowerOpen)) return false;
    }
    if (range._upper !== undefined) {
      const order = compareKeys(key, range._upper);
      if (order > 0 || (order === 0 && range._upperOpen)) return false;
    }
    return true;
  }

  /// A bare key stands for the range holding only it, which is what every
  /// `get(key)` and `openCursor(key)` on the platform accepts.
  function asRange(query, required) {
    if (query instanceof IDBKeyRange) return query;
    if (query === undefined || query === null) {
      if (required) throw new DOMException("a key is required here", "DataError");
      return new IDBKeyRange(undefined, undefined, false, false);
    }
    return IDBKeyRange.only(query);
  }

  // ── the records themselves ───────────────────────────────────────────────
  //
  // Kept sorted by key, so a cursor is a walk and a range is a pair of binary
  // searches rather than a scan of the whole store.

  /// The index of the first record whose key is not below `key`.
  function lowerBoundIndex(records, key) {
    let low = 0;
    let high = records.length;
    while (low < high) {
      const mid = (low + high) >> 1;
      if (compareKeys(records[mid].key, key) < 0) low = mid + 1;
      else high = mid;
    }
    return low;
  }

  function findIndex(records, key) {
    const at = lowerBoundIndex(records, key);
    if (at < records.length && compareKeys(records[at].key, key) === 0) return at;
    return -1;
  }

  /// The half-open span of records the range covers.
  function spanOf(records, range) {
    let start = 0;
    let end = records.length;
    if (range._lower !== undefined) {
      start = lowerBoundIndex(records, range._lower);
      if (range._lowerOpen) {
        while (start < end && compareKeys(records[start].key, range._lower) === 0) start++;
      }
    }
    if (range._upper !== undefined) {
      end = lowerBoundIndex(records, range._upper);
      if (!range._upperOpen) {
        while (end < records.length && compareKeys(records[end].key, range._upper) === 0) end++;
      }
    }
    return [start, Math.max(start, end)];
  }

  // ── errors and events ────────────────────────────────────────────────────


  /// Deliver an event to a target's handler property *and* its listeners.
  ///
  /// `request.onsuccess = fn` is how nearly every page uses this API, and a
  /// handler property is not a listener: `dispatchEvent` alone never calls it.
  function emit(target, event) {
    // Before the handler runs, not after: `e.target.result` is how a page
    // reaches its database from `onupgradeneeded`, and `dispatchEvent` would
    // not have filled it in yet.
    if (event.target === null || event.target === undefined) {
      try {
        event.target = target;
        event.currentTarget = target;
      } catch (error) {
        void error;
      }
    }
    const handler = target["on" + event.type];
    if (typeof handler === "function") {
      try {
        handler.call(target, event);
      } catch (error) {
        console.error(`indexeddb ${event.type} handler threw: ${withStack(error)}`);
      }
    }
    target.dispatchEvent(event);
    return event;
  }

  /// Deliver a request's event, and — for `error` — on up to the transaction
  /// and the database, which is the propagation a page relies on when it puts
  /// one handler on the transaction.
  ///
  /// Returns whether anything called `preventDefault`. An `error` nobody
  /// handled aborts the transaction, which is the standard's own rule and the
  /// difference between a failed write and a half-written store.
  function deliver(request, type) {
    const event = new Event(type, { bubbles: type === "error", cancelable: type === "error" });
    emit(request, event);
    if (type === "error" && request._transaction) {
      emit(request._transaction, event);
      if (request._transaction._db) emit(request._transaction._db, event);
    }
    return event.defaultPrevented;
  }

  class IDBRequest extends EventTarget {
    constructor(source, transaction) {
      super();
      this._source = source ?? null;
      this._transaction = transaction ?? null;
      this._result = undefined;
      this._error = null;
      this._done = false;
      // Set apart from `_done`, because an open request hands back its
      // database during `upgradeneeded` — `e.target.result` there is how every
      // page creates its stores — while the request itself is still pending.
      this._hasResult = false;
    }
    get source() { return this._source; }
    get transaction() { return this._transaction; }
    get readyState() { return this._done ? "done" : "pending"; }
    get result() {
      if (!this._done && !this._hasResult) {
        throw new DOMException("this request has not finished", "InvalidStateError");
      }
      if (this._error) throw this._error;
      return this._result;
    }
    get error() {
      if (!this._done) {
        throw new DOMException("this request has not finished", "InvalidStateError");
      }
      return this._error;
    }
    _succeed(value) {
      this._done = true;
      this._hasResult = true;
      this._result = value;
      this._error = null;
      deliver(this, "success");
    }
    _fail(error) {
      this._done = true;
      this._result = undefined;
      this._error = error;
      const handled = deliver(this, "error");
      // Not handled means not survivable: the transaction goes back.
      if (!handled && this._transaction) this._transaction._abort(error);
    }
  }

  class IDBOpenDBRequest extends IDBRequest {}

  class IDBVersionChangeEvent extends Event {
    constructor(type, init) {
      super(type, init);
      this.oldVersion = (init && init.oldVersion) || 0;
      this.newVersion = init && init.newVersion !== undefined ? init.newVersion : null;
    }
  }

  // ── transactions ─────────────────────────────────────────────────────────

  class IDBTransaction extends EventTarget {
    constructor(db, names, mode) {
      super();
      this._db = db;
      this._names = names.slice();
      this._mode = mode;
      this._pending = 0;
      this._state = "active";
      this._error = null;
      this._stores = new Map();
      // What the stores looked like before this transaction touched them.
      // Writes replace entries rather than mutate them, so a copy of each
      // array is a whole rollback.
      this._undo = mode === "readonly" ? null : db._snapshot(names);
    }
    get db() { return this._db; }
    get mode() { return this._mode; }
    get error() { return this._error; }
    get objectStoreNames() { return stringList(this._names); }

    objectStore(name) {
      if (this._state === "finished") {
        throw new DOMException("this transaction has finished", "InvalidStateError");
      }
      if (!this._names.includes(String(name))) {
        throw new DOMException(`\`${name}\` is not in this transaction`, "NotFoundError");
      }
      const known = this._stores.get(String(name));
      if (known) return known;
      const store = new IDBObjectStore(this, this._db._stores.get(String(name)));
      this._stores.set(String(name), store);
      return store;
    }

    abort() { this._abort(null); }

    commit() {
      // Explicit commit only says "no more requests are coming"; the work
      // already queued still runs, and `complete` still fires after it.
      this._maybeFinish();
    }

    /// Queue one request's work. Everything a store does goes through here, so
    /// the transaction can tell when it has nothing left to do.
    _run(request, work) {
      if (this._state !== "active") {
        throw new DOMException("this transaction has finished", "InvalidStateError");
      }
      this._pending++;
      queueMicrotask(() => {
        this._pending--;
        if (this._state !== "active") return;
        try {
          request._succeed(work());
        } catch (error) {
          request._fail(
            error instanceof DOMException
              ? error
              : new DOMException(String(error && error.message ? error.message : error), "UnknownError")
          );
        }
        this._maybeFinish();
      });
      return request;
    }

    _maybeFinish() {
      if (this._state !== "active" || this._pending > 0) return;
      // One more turn before committing: a success handler is free to start
      // another request on this transaction, and a cursor walk is exactly
      // that. Committing in the same turn would cut the walk off at one step.
      queueMicrotask(() => {
        if (this._state !== "active" || this._pending > 0) return;
        this._state = "finished";
        this._db._finish(this);
        emit(this, new Event("complete"));
      });
    }

    _abort(error) {
      if (this._state === "finished") return;
      this._state = "finished";
      this._error = error || new DOMException("the transaction was aborted", "AbortError");
      if (this._undo) this._db._restore(this._undo);
      this._db._finish(this);
      emit(this, new Event("abort", { bubbles: true }));
    }

    _demandWritable() {
      if (this._mode === "readonly") {
        throw new DOMException("this transaction is read-only", "ReadOnlyError");
      }
    }
  }

  // ── object stores ────────────────────────────────────────────────────────
  //
  // `IDBObjectStore` is the handle a transaction hands out; `data` below is the
  // store itself, shared by every handle and owned by the database.

  class IDBObjectStore {
    constructor(transaction, data) {
      this._transaction = transaction;
      this._data = data;
    }
    get name() { return this._data.name; }
    get keyPath() { return this._data.keyPath; }
    get autoIncrement() { return this._data.autoIncrement; }
    get transaction() { return this._transaction; }
    get indexNames() { return stringList([...this._data.indexes.keys()]); }

    /// The key a record will be filed under, from the value or from the
    /// argument, and the one the store generates when neither says.
    _keyFor(value, explicit, forAdd) {
      const data = this._data;
      if (data.keyPath !== null) {
        if (explicit !== undefined) {
          throw new DOMException(
            "this store takes its key from the value, so a key may not be passed",
            "DataError"
          );
        }
        const found = evaluatePath(value, data.keyPath);
        if (found === undefined) {
          if (!data.autoIncrement) {
            throw new DOMException("the value has no key at the store's key path", "DataError");
          }
          const generated = data.nextKey++;
          if (!Array.isArray(data.keyPath)) injectPath(value, data.keyPath, generated);
          return { kind: KIND_NUMBER, value: generated };
        }
        return demandKey(found, "the value's key");
      }
      if (explicit !== undefined) return demandKey(explicit, "the key");
      if (!data.autoIncrement) {
        throw new DOMException("this store needs a key", "DataError");
      }
      void forAdd;
      return { kind: KIND_NUMBER, value: data.nextKey++ };
    }

    _write(value, explicit, forAdd) {
      this._transaction._demandWritable();
      const data = this._data;
      const stored = structuredClone(value);
      const key = this._keyFor(stored, explicit, forAdd);
      // A generated number moves the counter past itself, so an explicit `7`
      // is not handed out again later.
      if (key.kind === KIND_NUMBER && key.value >= data.nextKey) {
        data.nextKey = Math.floor(key.value) + 1;
      }
      const at = findIndex(data.records, key);
      if (at >= 0 && forAdd) {
        throw new DOMException("a record with that key already exists", "ConstraintError");
      }
      const record = { key, value: stored };
      if (at >= 0) {
        data.records = data.records.slice();
        data.records[at] = record;
      } else {
        const insert = lowerBoundIndex(data.records, key);
        data.records = data.records.slice();
        data.records.splice(insert, 0, record);
      }
      reindex(data);
      return fromKey(key);
    }

    add(value, key) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () =>
        this._write(value, key, true)
      );
    }
    put(value, key) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () =>
        this._write(value, key, false)
      );
    }
    get(query) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = spanOf(this._data.records, asRange(query, true));
        return start < end ? structuredClone(this._data.records[start].value) : undefined;
      });
    }
    getKey(query) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = spanOf(this._data.records, asRange(query, true));
        return start < end ? fromKey(this._data.records[start].key) : undefined;
      });
    }
    getAll(query, count) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = spanOf(this._data.records, asRange(query, false));
        const stop = count ? Math.min(end, start + Number(count)) : end;
        return this._data.records.slice(start, stop).map((r) => structuredClone(r.value));
      });
    }
    getAllKeys(query, count) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = spanOf(this._data.records, asRange(query, false));
        const stop = count ? Math.min(end, start + Number(count)) : end;
        return this._data.records.slice(start, stop).map((r) => fromKey(r.key));
      });
    }
    count(query) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = spanOf(this._data.records, asRange(query, false));
        return end - start;
      });
    }
    delete(query) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        this._transaction._demandWritable();
        const [start, end] = spanOf(this._data.records, asRange(query, true));
        if (start < end) {
          this._data.records = this._data.records.slice();
          this._data.records.splice(start, end - start);
          reindex(this._data);
        }
        return undefined;
      });
    }
    clear() {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        this._transaction._demandWritable();
        this._data.records = [];
        reindex(this._data);
        return undefined;
      });
    }
    openCursor(query, direction) {
      return openCursor(
        this, this._transaction, () => this._data.records, query, direction, true, this
      );
    }
    openKeyCursor(query, direction) {
      return openCursor(
        this, this._transaction, () => this._data.records, query, direction, false, this
      );
    }

    index(name) {
      const data = this._data.indexes.get(String(name));
      if (!data) throw new DOMException(`there is no index \`${name}\``, "NotFoundError");
      return new IDBIndex(this, data);
    }
    createIndex(name, keyPath, options) {
      if (this._transaction._mode !== "versionchange") {
        throw new DOMException("an index is created during an upgrade", "InvalidStateError");
      }
      const settings = options || {};
      const data = {
        name: String(name),
        keyPath: Array.isArray(keyPath) ? keyPath.slice() : String(keyPath),
        unique: !!settings.unique,
        multiEntry: !!settings.multiEntry,
        entries: [],
        store: this._data,
      };
      this._data.indexes.set(data.name, data);
      reindex(this._data);
      return new IDBIndex(this, data);
    }
    deleteIndex(name) {
      if (this._transaction._mode !== "versionchange") {
        throw new DOMException("an index is deleted during an upgrade", "InvalidStateError");
      }
      this._data.indexes.delete(String(name));
    }
  }

  /// Rebuild every index over a store.
  ///
  /// Wholesale rather than incrementally: a store is small enough here that the
  /// cost is a rounding error next to the bugs an incremental path invites,
  /// and `unique` has to be checked against the whole store anyway.
  function reindex(store) {
    for (const index of store.indexes.values()) {
      const entries = [];
      for (const record of store.records) {
        const found = evaluatePath(record.value, index.keyPath);
        if (found === undefined) continue;
        if (index.multiEntry && Array.isArray(found)) {
          const seen = [];
          for (const one of found) {
            const key = toKey(one);
            if (key === undefined) continue;
            if (seen.some((k) => compareKeys(k, key) === 0)) continue;
            seen.push(key);
            entries.push({ key, primary: record.key, value: record.value });
          }
          continue;
        }
        const key = toKey(found);
        if (key === undefined) continue;
        entries.push({ key, primary: record.key, value: record.value });
      }
      entries.sort((a, b) => compareKeys(a.key, b.key) || compareKeys(a.primary, b.primary));
      if (index.unique) {
        for (let i = 1; i < entries.length; i++) {
          if (compareKeys(entries[i - 1].key, entries[i].key) === 0) {
            throw new DOMException(
              `index \`${index.name}\` is unique and this key is already in it`,
              "ConstraintError"
            );
          }
        }
      }
      index.entries = entries;
    }
  }

  // ── cursors ──────────────────────────────────────────────────────────────
  //
  // Position is remembered as a key, not an offset: the list underneath can
  // grow or shrink between steps — a cursor that deletes as it walks is the
  // ordinary case — and an offset would silently skip or repeat a record.

  /// The `DOMStringList` the name lists are. An array would answer `length`
  /// and index access, and then `objectStoreNames.contains(name)` — which is
  /// how every page asks whether it has to create a store — would not be a
  /// function.
  function stringList(names) {
    const list = names.slice().sort();
    list.contains = (name) => list.indexOf(String(name)) >= 0;
    list.item = (index) => (index >= 0 && index < list.length ? list[index] : null);
    return list;
  }

  function primaryOf(item) {
    return item.primary === undefined ? item.key : item.primary;
  }

  class IDBCursor {
    constructor(source, direction, request, range, listOf, store) {
      this._source = source;
      this._direction = direction;
      this._request = request;
      this._range = range;
      this._listOf = listOf;
      this._store = store;
      this._key = undefined;
      this._primary = undefined;
      this._value = undefined;
      this._started = false;
      this._done = false;
    }
    get source() { return this._source; }
    get direction() { return this._direction; }
    get key() { return fromKey(this._key); }
    get primaryKey() { return fromKey(this._primary); }
    get request() { return this._request; }

    _step(target, targetPrimary, count) {
      const items = this._listOf();
      const forward = this._direction === "next" || this._direction === "nextunique";
      const unique = this._direction === "nextunique" || this._direction === "prevunique";
      const ordered = forward ? items : items.slice().reverse();
      let steps = count === undefined ? 1 : Number(count);

      for (const item of ordered) {
        if (!rangeHolds(this._range, item.key)) continue;
        if (this._started) {
          const order = compareKeys(item.key, this._key);
          const ahead = forward ? order > 0 : order < 0;
          if (!ahead) {
            // `nextunique` and `prevunique` only ever move to a new key.
            if (unique || order !== 0) continue;
            const side = compareKeys(primaryOf(item), this._primary);
            if (!(forward ? side > 0 : side < 0)) continue;
          }
        }
        if (target !== undefined) {
          const order = compareKeys(item.key, target);
          if (!(forward ? order >= 0 : order <= 0)) continue;
          if (targetPrimary !== undefined && order === 0) {
            const side = compareKeys(primaryOf(item), targetPrimary);
            if (!(forward ? side >= 0 : side <= 0)) continue;
          }
        }
        if (--steps > 0) {
          this._key = item.key;
          this._primary = primaryOf(item);
          this._started = true;
          continue;
        }
        this._key = item.key;
        this._primary = primaryOf(item);
        this._value = item.value;
        this._started = true;
        return true;
      }
      this._done = true;
      return false;
    }

    /// Ask for the next record. The *same* request fires `success` again, which
    /// is what makes a cursor walk a loop over one `onsuccess`.
    _resume(target, targetPrimary, count) {
      if (this._done) {
        throw new DOMException("this cursor has passed the end", "InvalidStateError");
      }
      const request = this._request;
      request._done = false;
      request._transaction._run(request, () =>
        this._step(target, targetPrimary, count) ? this : null
      );
    }

    continue(key) {
      this._resume(key === undefined ? undefined : demandKey(key, "the key"), undefined, 1);
    }
    continuePrimaryKey(key, primary) {
      this._resume(demandKey(key, "the key"), demandKey(primary, "the primary key"), 1);
    }
    advance(count) {
      const steps = Number(count);
      if (!Number.isFinite(steps) || steps < 1) {
        throw new TypeError("advance takes a count of at least 1");
      }
      this._resume(undefined, undefined, steps);
    }
    update(value) {
      if (!this._started || this._done) {
        throw new DOMException("this cursor is not on a record", "InvalidStateError");
      }
      const store = this._store;
      return store._transaction._run(new IDBRequest(store, store._transaction), () =>
        store._write(value, store._data.keyPath === null ? fromKey(this._primary) : undefined, false)
      );
    }
    delete() {
      if (!this._started || this._done) {
        throw new DOMException("this cursor is not on a record", "InvalidStateError");
      }
      const store = this._store;
      const primary = this._primary;
      return store._transaction._run(new IDBRequest(store, store._transaction), () => {
        store._transaction._demandWritable();
        const at = findIndex(store._data.records, primary);
        if (at >= 0) {
          store._data.records = store._data.records.slice();
          store._data.records.splice(at, 1);
          reindex(store._data);
        }
        return undefined;
      });
    }
  }

  class IDBCursorWithValue extends IDBCursor {
    get value() { return this._value === undefined ? undefined : structuredClone(this._value); }
  }

  function openCursor(source, transaction, listOf, query, direction, withValue, store) {
    const request = new IDBRequest(source, transaction);
    const Kind = withValue ? IDBCursorWithValue : IDBCursor;
    const cursor = new Kind(
      source,
      direction || "next",
      request,
      asRange(query, false),
      listOf,
      store
    );
    return transaction._run(request, () => (cursor._step(undefined, undefined, 1) ? cursor : null));
  }

  // ── indexes ──────────────────────────────────────────────────────────────

  class IDBIndex {
    constructor(store, data) {
      this._store = store;
      this._data = data;
      this._transaction = store._transaction;
    }
    get name() { return this._data.name; }
    get keyPath() { return this._data.keyPath; }
    get unique() { return this._data.unique; }
    get multiEntry() { return this._data.multiEntry; }
    get objectStore() { return this._store; }

    _span(query, required) {
      return spanOf(this._data.entries, asRange(query, required));
    }
    get(query) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = this._span(query, true);
        return start < end ? structuredClone(this._data.entries[start].value) : undefined;
      });
    }
    getKey(query) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = this._span(query, true);
        return start < end ? fromKey(this._data.entries[start].primary) : undefined;
      });
    }
    getAll(query, count) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = this._span(query, false);
        const stop = count ? Math.min(end, start + Number(count)) : end;
        return this._data.entries.slice(start, stop).map((e) => structuredClone(e.value));
      });
    }
    getAllKeys(query, count) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = this._span(query, false);
        const stop = count ? Math.min(end, start + Number(count)) : end;
        return this._data.entries.slice(start, stop).map((e) => fromKey(e.primary));
      });
    }
    count(query) {
      return this._transaction._run(new IDBRequest(this, this._transaction), () => {
        const [start, end] = this._span(query, false);
        return end - start;
      });
    }
    openCursor(query, direction) {
      return openCursor(
        this, this._transaction, () => this._data.entries, query, direction, true, this._store
      );
    }
    openKeyCursor(query, direction) {
      return openCursor(
        this, this._transaction, () => this._data.entries, query, direction, false, this._store
      );
    }
  }

  // ── databases ────────────────────────────────────────────────────────────

  class IDBDatabase extends EventTarget {
    constructor(record) {
      super();
      this._record = record;
      this._stores = record.stores;
      this._closed = false;
      this._open = new Set();
    }
    get name() { return this._record.name; }
    get version() { return this._record.version; }
    get objectStoreNames() { return stringList([...this._stores.keys()]); }

    createObjectStore(name, options) {
      if (!this._upgrade || this._upgrade._state !== "active") {
        throw new DOMException("a store is created during an upgrade", "InvalidStateError");
      }
      const settings = options || {};
      const key = settings.keyPath === undefined || settings.keyPath === null
        ? null
        : (Array.isArray(settings.keyPath) ? settings.keyPath.slice() : String(settings.keyPath));
      if (this._stores.has(String(name))) {
        throw new DOMException(`\`${name}\` already exists`, "ConstraintError");
      }
      const data = {
        name: String(name),
        keyPath: key,
        autoIncrement: !!settings.autoIncrement,
        records: [],
        indexes: new Map(),
        nextKey: 1,
      };
      this._stores.set(data.name, data);
      // Reachable from the upgrade transaction the moment it exists, which is
      // what every `onupgradeneeded` does next.
      this._upgrade._names.push(data.name);
      return this._upgrade.objectStore(data.name);
    }

    deleteObjectStore(name) {
      if (!this._upgrade || this._upgrade._state !== "active") {
        throw new DOMException("a store is deleted during an upgrade", "InvalidStateError");
      }
      if (!this._stores.delete(String(name))) {
        throw new DOMException(`there is no store \`${name}\``, "NotFoundError");
      }
    }

    transaction(names, mode) {
      if (this._closed) {
        throw new DOMException("this database is closed", "InvalidStateError");
      }
      const wanted = (typeof names === "string" ? [names] : [...names]).map(String);
      if (wanted.length === 0) {
        throw new DOMException("a transaction needs at least one store", "InvalidAccessError");
      }
      for (const one of wanted) {
        if (!this._stores.has(one)) {
          throw new DOMException(`there is no store \`${one}\``, "NotFoundError");
        }
      }
      const transaction = new IDBTransaction(this, wanted, mode === "readwrite" ? "readwrite" : "readonly");
      this._open.add(transaction);
      return transaction;
    }

    close() { this._closed = true; }

    _snapshot(names) {
      const undo = [];
      for (const name of names) {
        const store = this._stores.get(name);
        if (!store) continue;
        undo.push({
          store,
          records: store.records,
          nextKey: store.nextKey,
          indexes: [...store.indexes.values()].map((i) => ({ index: i, entries: i.entries })),
        });
      }
      return undo;
    }
    _restore(undo) {
      for (const held of undo) {
        held.store.records = held.records;
        held.store.nextKey = held.nextKey;
        for (const kept of held.indexes) kept.index.entries = kept.entries;
      }
    }
    _finish(transaction) { this._open.delete(transaction); }
  }

  // ── the factory ──────────────────────────────────────────────────────────

  const databases = new Map();

  class IDBFactory {
    open(name, version) {
      const wanted = version === undefined ? undefined : Number(version);
      if (wanted !== undefined && (!Number.isInteger(wanted) || wanted < 1)) {
        throw new TypeError("a database version is a positive integer");
      }
      const request = new IDBOpenDBRequest(null, null);
      queueMicrotask(() => {
        let record = databases.get(String(name));
        const fresh = !record;
        if (!record) {
          record = { name: String(name), version: 0, stores: new Map() };
          databases.set(record.name, record);
        }
        const from = record.version;
        const to = wanted === undefined ? (fresh ? 1 : record.version) : wanted;
        if (to < from) {
          request._fail(
            new DOMException(
              `the database is at version ${from} and cannot go back to ${to}`,
              "VersionError"
            )
          );
          return;
        }
        const db = new IDBDatabase(record);
        if (to > from) {
          record.version = to;
          // The upgrade runs in its own transaction over every store, and the
          // page's `onupgradeneeded` is the only chance it gets to change the
          // shape of the database.
          const upgrade = new IDBTransaction(db, [...record.stores.keys()], "versionchange");
          db._upgrade = upgrade;
          db._open.add(upgrade);
          request._result = db;
          request._hasResult = true;
          request._transaction = upgrade;
          const event = new IDBVersionChangeEvent("upgradeneeded", {
            oldVersion: from,
            newVersion: to,
          });
          emit(request, event);
          upgrade.addEventListener("complete", () => {
            db._upgrade = null;
            request._transaction = null;
            request._succeed(db);
          });
          upgrade.addEventListener("abort", () => {
            db._upgrade = null;
            request._transaction = null;
            request._fail(upgrade.error || new DOMException("the upgrade was aborted", "AbortError"));
          });
          upgrade._maybeFinish();
          return;
        }
        request._succeed(db);
      });
      return request;
    }

    deleteDatabase(name) {
      const request = new IDBOpenDBRequest(null, null);
      queueMicrotask(() => {
        const record = databases.get(String(name));
        const had = record ? record.version : 0;
        databases.delete(String(name));
        request._result = undefined;
        request._done = true;
        emit(request, new IDBVersionChangeEvent("success", { oldVersion: had, newVersion: null }));
      });
      return request;
    }

    databases() {
      return Promise.resolve(
        [...databases.values()].map((d) => ({ name: d.name, version: d.version }))
      );
    }

    cmp(a, b) {
      return compareKeys(demandKey(a, "the first key"), demandKey(b, "the second key"));
    }
  }

  globalThis.indexedDB = new IDBFactory();
  globalThis.IDBFactory = IDBFactory;
  globalThis.IDBDatabase = IDBDatabase;
  globalThis.IDBObjectStore = IDBObjectStore;
  globalThis.IDBIndex = IDBIndex;
  globalThis.IDBTransaction = IDBTransaction;
  globalThis.IDBRequest = IDBRequest;
  globalThis.IDBOpenDBRequest = IDBOpenDBRequest;
  globalThis.IDBCursor = IDBCursor;
  globalThis.IDBCursorWithValue = IDBCursorWithValue;
  globalThis.IDBKeyRange = IDBKeyRange;
  globalThis.IDBVersionChangeEvent = IDBVersionChangeEvent;
})();
