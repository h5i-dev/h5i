// The Streams standard: `ReadableStream` and the writable and transform
// halves that `pipeThrough` needs.
//
// Its own source, parsed only when a page reads one of those names — see
// `TIERS` in `mod.rs`. React's flight client builds one to hydrate through, so
// an App Router page needs it and a page of plain markup never asks.
(function () {
  "use strict";

  const api = globalThis.__h5i;
  const { AbortController } = globalThis;

  //
  // Enough of the Streams standard for what pages construct: a source with
  // `start`/`pull`/`cancel`, one default reader at a time, and the writable and
  // transform halves `pipeThrough` needs.
  //
  // Byte streams are deliberately absent. A page asking for `type: "bytes"` is
  // told it is missing rather than handed a default stream wearing the name,
  // which would answer a BYOB read with the wrong shape.

  class ReadableStreamDefaultController {
    constructor(stream, source, highWaterMark, sizeOf) {
      this._stream = stream;
      this._source = source;
      this._queue = [];
      this._queued = 0;
      this._hwm = Number.isFinite(highWaterMark) ? highWaterMark : 1;
      this._sizeOf = sizeOf;
      this._closeRequested = false;
      this._started = false;
      this._pulling = false;
      this._pullAgain = false;
    }

    get desiredSize() {
      if (this._stream._state === "errored") return null;
      if (this._stream._state === "closed") return 0;
      return this._hwm - this._queued;
    }

    enqueue(chunk) {
      if (this._closeRequested || this._stream._state !== "readable") {
        throw new TypeError("cannot enqueue to a stream that is closed or closing");
      }
      // A parked read takes the chunk straight from here. Queueing it and
      // draining a moment later is the same answer with a microtask in front
      // of it, and the queue is what `desiredSize` reports.
      const reader = this._stream._reader;
      if (reader && reader._requests.length) {
        reader._requests.shift().resolve({ value: chunk, done: false });
      } else {
        let size = 1;
        if (this._sizeOf) {
          try {
            size = Number(this._sizeOf(chunk));
          } catch (error) {
            this._fail(error);
            throw error;
          }
        }
        this._queue.push({ chunk, size });
        this._queued += size;
      }
      this._pullIfNeeded();
    }

    close() {
      if (this._closeRequested || this._stream._state !== "readable") {
        throw new TypeError("cannot close a stream that is already closed or errored");
      }
      this._closeRequested = true;
      // Only once the queue is empty: a close with chunks still queued is a
      // promise that the reader will see them first.
      if (!this._queue.length) this._finish();
    }

    error(reason) { this._fail(reason); }

    _finish() {
      const stream = this._stream;
      if (stream._state !== "readable") return;
      stream._state = "closed";
      const reader = stream._reader;
      if (!reader) return;
      for (const request of reader._requests.splice(0)) {
        request.resolve({ value: undefined, done: true });
      }
      reader._settleClosed(null);
    }

    _fail(reason) {
      const stream = this._stream;
      if (stream._state !== "readable") return;
      stream._state = "errored";
      stream._storedError = reason;
      this._queue.length = 0;
      this._queued = 0;
      const reader = stream._reader;
      if (!reader) return;
      for (const request of reader._requests.splice(0)) request.reject(reason);
      reader._settleClosed(reason);
    }

    _wantsMore() {
      if (!this._started || this._closeRequested) return false;
      if (this._stream._state !== "readable") return false;
      const reader = this._stream._reader;
      if (reader && reader._requests.length) return true;
      return this.desiredSize > 0;
    }

    _pullIfNeeded() {
      if (!this._wantsMore()) return;
      // One `pull` in flight at a time, with a single re-arm rather than a
      // counter: the source is asked again once it answers, which is what the
      // standard's `pullAgain` flag is for.
      if (this._pulling) { this._pullAgain = true; return; }
      this._pulling = true;
      const pull = this._source.pull;
      let running;
      try {
        running = pull ? pull.call(this._source, this) : undefined;
      } catch (error) {
        this._pulling = false;
        this._fail(error);
        return;
      }
      Promise.resolve(running).then(
        () => {
          this._pulling = false;
          if (this._pullAgain) { this._pullAgain = false; this._pullIfNeeded(); }
        },
        (error) => { this._pulling = false; this._fail(error); }
      );
    }

    _take() {
      const entry = this._queue.shift();
      this._queued = Math.max(0, this._queued - entry.size);
      if (!this._queue.length && this._closeRequested) this._finish();
      else this._pullIfNeeded();
      return entry.chunk;
    }

    _cancelSource(reason) {
      this._queue.length = 0;
      this._queued = 0;
      const cancel = this._source.cancel;
      try {
        return Promise.resolve(cancel ? cancel.call(this._source, reason) : undefined);
      } catch (error) {
        return Promise.reject(error);
      }
    }
  }

  class ReadableStreamDefaultReader {
    constructor(stream) {
      if (!(stream instanceof ReadableStream)) {
        throw new TypeError("a reader reads a ReadableStream");
      }
      if (stream._reader) throw new TypeError("this stream is already locked to a reader");
      this._stream = stream;
      this._requests = [];
      stream._reader = this;
      this._closed = new Promise((resolve, reject) => {
        this._closedResolve = resolve;
        this._closedReject = reject;
      });
      // A `closed` nobody reads still rejects when the lock is released, and an
      // unhandled rejection is not what releasing a lock means.
      this._closed.catch(() => {});
      if (stream._state === "closed") this._settleClosed(null);
      else if (stream._state === "errored") this._settleClosed(stream._storedError);
    }

    get closed() { return this._closed; }

    _settleClosed(reason) {
      if (reason === null) this._closedResolve(undefined);
      else this._closedReject(reason);
    }

    read() {
      const stream = this._stream;
      if (!stream) return Promise.reject(new TypeError("this reader has been released"));
      if (stream._state === "errored") return Promise.reject(stream._storedError);
      const controller = stream._controller;
      if (controller._queue.length) {
        return Promise.resolve({ value: controller._take(), done: false });
      }
      if (stream._state === "closed") return Promise.resolve({ value: undefined, done: true });
      return new Promise((resolve, reject) => {
        this._requests.push({ resolve, reject });
        controller._pullIfNeeded();
      });
    }

    cancel(reason) {
      if (!this._stream) return Promise.reject(new TypeError("this reader has been released"));
      return this._stream.cancel(reason);
    }

    releaseLock() {
      const stream = this._stream;
      if (!stream) return;
      if (this._requests.length) throw new TypeError("a read is still pending on this reader");
      stream._reader = null;
      this._stream = null;
      this._closedReject(new TypeError("this reader has been released"));
    }
  }

  class ReadableStream {
    constructor(source, strategy) {
      const underlying = source || {};
      const limits = strategy || {};
      if (underlying.type !== undefined && String(underlying.type) !== "") {
        api.unsupported(`ReadableStream type: ${String(underlying.type)}`);
      }
      this._state = "readable";
      this._storedError = undefined;
      this._reader = null;
      const hwm = limits.highWaterMark === undefined ? 1 : Number(limits.highWaterMark);
      const sizeOf = typeof limits.size === "function" ? limits.size : null;
      this._controller = new ReadableStreamDefaultController(this, underlying, hwm, sizeOf);
      let started;
      try {
        started = underlying.start ? underlying.start.call(underlying, this._controller) : undefined;
      } catch (error) {
        this._controller._fail(error);
        return;
      }
      // `pull` waits for `start` to finish, which is why the flag is set here
      // and not in the constructor: a source that enqueues from `start` should
      // not also be asked to `pull` while it is still starting.
      Promise.resolve(started).then(
        () => { this._controller._started = true; this._controller._pullIfNeeded(); },
        (error) => this._controller._fail(error)
      );
    }

    get locked() { return this._reader !== null; }

    getReader(options) {
      const mode = options && options.mode;
      if (mode !== undefined && mode !== null && String(mode) !== "") {
        throw new TypeError(`this engine has no ${String(mode)} readers`);
      }
      return new ReadableStreamDefaultReader(this);
    }

    cancel(reason) {
      if (this._state === "errored") return Promise.reject(this._storedError);
      if (this._state === "closed") return Promise.resolve(undefined);
      const controller = this._controller;
      this._state = "closed";
      const reader = this._reader;
      if (reader) {
        for (const request of reader._requests.splice(0)) {
          request.resolve({ value: undefined, done: true });
        }
        reader._settleClosed(null);
      }
      return controller._cancelSource(reason).then(() => undefined);
    }

    /// Two streams over one source, which is how a payload gets read twice.
    ///
    /// The source is cancelled only when *both* branches have been, because a
    /// branch nobody is reading must not stop the one that is.
    tee() {
      const reader = this.getReader();
      const controllers = [];
      const cancelled = [false, false];
      let reasons = [];
      let done = false;

      const step = () => reader.read().then(
        (result) => {
          if (result.done) {
            if (done) return;
            done = true;
            for (const controller of controllers) {
              // Already closed: a branch the reader cancelled.
              try { controller.close(); } catch (error) { void error; }
            }
            return;
          }
          for (const controller of controllers) {
            // A branch that has gone away takes no more chunks.
            try { controller.enqueue(result.value); } catch (error) { void error; }
          }
        },
        (error) => {
          done = true;
          for (const controller of controllers) {
            try { controller.error(error); } catch (x) { void x; }
          }
        }
      );

      const branch = (index) => new ReadableStream({
        start(controller) { controllers[index] = controller; },
        pull() { return step(); },
        cancel: (reason) => {
          cancelled[index] = true;
          reasons[index] = reason;
          if (cancelled[0] && cancelled[1]) return reader.cancel(reasons);
          return undefined;
        },
      });

      return [branch(0), branch(1)];
    }

    pipeTo(destination, options) {
      const preventClose = !!(options && options.preventClose);
      const reader = this.getReader();
      const writer = destination.getWriter();
      const step = () => reader.read().then((result) => {
        if (result.done) {
          reader.releaseLock();
          if (preventClose) { writer.releaseLock(); return undefined; }
          return writer.close().then(() => { writer.releaseLock(); });
        }
        return writer.write(result.value).then(step);
      });
      return step().catch((error) => {
        // A read still in flight refuses the release, and that is fine here.
        try { reader.releaseLock(); } catch (x) { void x; }
        return writer.abort(error).then(
          () => { throw error; },
          () => { throw error; }
        );
      });
    }

    pipeThrough(pair, options) {
      const writable = pair && pair.writable;
      const readable = pair && pair.readable;
      if (!writable || !readable) {
        throw new TypeError("pipeThrough takes a { readable, writable } pair");
      }
      // The pipe runs on its own: `pipeThrough` hands back the readable end
      // immediately, and a failure surfaces there rather than as a rejection
      // nobody is holding.
      this.pipeTo(writable, options).catch(() => {});
      return readable;
    }

    values(options) {
      const preventCancel = !!(options && options.preventCancel);
      const reader = this.getReader();
      const iterator = {
        next() {
          return reader.read().then((result) => {
            if (result.done) reader.releaseLock();
            return result;
          });
        },
        return(value) {
          if (preventCancel) {
            reader.releaseLock();
            return Promise.resolve({ value, done: true });
          }
          const cancelled = reader.cancel(value);
          reader.releaseLock();
          return cancelled.then(() => ({ value, done: true }));
        },
      };
      iterator[Symbol.asyncIterator] = function () { return this; };
      return iterator;
    }

    static from(source) {
      const iterator =
        source && typeof source[Symbol.asyncIterator] === "function"
          ? source[Symbol.asyncIterator]()
          : source[Symbol.iterator]();
      return new ReadableStream({
        pull(controller) {
          return Promise.resolve(iterator.next()).then((step) => {
            if (step.done) controller.close();
            else controller.enqueue(step.value);
          });
        },
        cancel(reason) {
          if (typeof iterator.return === "function") return iterator.return(reason);
          return undefined;
        },
      });
    }
  }

  ReadableStream.prototype[Symbol.asyncIterator] = function (options) {
    return this.values(options);
  };

  class WritableStreamDefaultController {
    constructor(stream) {
      this._stream = stream;
      this._abort = new AbortController();
    }
    get signal() { return this._abort.signal; }
    error(reason) { this._stream._fail(reason); }
  }

  class WritableStreamDefaultWriter {
    constructor(stream) {
      if (!(stream instanceof WritableStream)) {
        throw new TypeError("a writer writes a WritableStream");
      }
      if (stream._writer) throw new TypeError("this stream is already locked to a writer");
      this._stream = stream;
      stream._writer = this;
    }
    get closed() { return this._stream ? this._stream._closed : Promise.resolve(undefined); }
    get desiredSize() { return this._stream ? this._stream._hwm : null; }
    get ready() { return this._stream ? this._stream._ready : Promise.resolve(undefined); }
    write(chunk) {
      if (!this._stream) return Promise.reject(new TypeError("this writer has been released"));
      return this._stream._write(chunk);
    }
    close() {
      if (!this._stream) return Promise.reject(new TypeError("this writer has been released"));
      return this._stream._close();
    }
    abort(reason) {
      if (!this._stream) return Promise.reject(new TypeError("this writer has been released"));
      return this._stream._abortWith(reason);
    }
    releaseLock() {
      if (!this._stream) return;
      this._stream._writer = null;
      this._stream = null;
    }
  }

  class WritableStream {
    constructor(sink, strategy) {
      const underlying = sink || {};
      this._sink = underlying;
      this._state = "writable";
      this._storedError = undefined;
      this._writer = null;
      this._hwm =
        strategy && strategy.highWaterMark !== undefined ? Number(strategy.highWaterMark) : 1;
      this._controller = new WritableStreamDefaultController(this);
      this._closed = new Promise((resolve, reject) => {
        this._closedResolve = resolve;
        this._closedReject = reject;
      });
      this._closed.catch(() => {});
      let started;
      try {
        started = underlying.start ? underlying.start.call(underlying, this._controller) : undefined;
      } catch (error) {
        this._fail(error);
        started = undefined;
      }
      this._ready = Promise.resolve(started);
      this._ready.catch(() => {});
      // Writes run one after another, each waiting on the last. A sink is
      // written as if it were: `write` returning a promise is how it says it
      // is not ready for the next chunk yet.
      this._tail = this._ready.then(() => undefined, () => undefined);
    }

    get locked() { return this._writer !== null; }
    getWriter() { return new WritableStreamDefaultWriter(this); }

    _fail(reason) {
      if (this._state !== "writable") return;
      this._state = "errored";
      this._storedError = reason;
      this._closedReject(reason);
    }

    _write(chunk) {
      if (this._state !== "writable") {
        return Promise.reject(this._storedError || new TypeError("the stream is not writable"));
      }
      const run = this._tail.then(() => {
        if (this._state !== "writable") {
          throw this._storedError || new TypeError("the stream is not writable");
        }
        const write = this._sink.write;
        return write ? write.call(this._sink, chunk, this._controller) : undefined;
      });
      this._tail = run.then(() => undefined, () => undefined);
      return run;
    }

    _close() {
      if (this._state !== "writable") {
        return Promise.reject(this._storedError || new TypeError("the stream is not writable"));
      }
      const run = this._tail.then(() => {
        if (this._state !== "writable") {
          throw this._storedError || new TypeError("the stream is not writable");
        }
        this._state = "closed";
        const close = this._sink.close;
        return Promise.resolve(close ? close.call(this._sink) : undefined).then(() => {
          this._closedResolve(undefined);
        });
      });
      this._tail = run.then(() => undefined, () => undefined);
      return run;
    }

    _abortWith(reason) {
      if (this._state === "closed") return Promise.resolve(undefined);
      if (this._state === "errored") return Promise.resolve(undefined);
      this._state = "errored";
      this._storedError = reason;
      this._closedReject(reason);
      this._controller._abort.abort(reason);
      const abort = this._sink.abort;
      try {
        return Promise.resolve(abort ? abort.call(this._sink, reason) : undefined);
      } catch (error) {
        return Promise.reject(error);
      }
    }
  }

  class TransformStreamDefaultController {
    constructor() { this._readable = null; }
    get desiredSize() { return this._readable ? this._readable.desiredSize : null; }
    enqueue(chunk) { this._readable.enqueue(chunk); }
    error(reason) { this._readable.error(reason); }
    terminate() {
      try { this._readable.close(); } catch (error) { void error; }
    }
  }

  class TransformStream {
    constructor(transformer, writableStrategy, readableStrategy) {
      const shape = transformer || {};
      const controller = new TransformStreamDefaultController();
      this.readable = new ReadableStream(
        { start(readable) { controller._readable = readable; } },
        readableStrategy
      );
      this.writable = new WritableStream(
        {
          start() { return shape.start ? shape.start.call(shape, controller) : undefined; },
          write(chunk) {
            // No `transform` is the identity transform, which is what a
            // `TransformStream` with only a `flush` is for.
            if (shape.transform) return shape.transform.call(shape, chunk, controller);
            controller.enqueue(chunk);
            return undefined;
          },
          close() {
            return Promise.resolve(
              shape.flush ? shape.flush.call(shape, controller) : undefined
            ).then(() => controller.terminate());
          },
          abort(reason) { controller.error(reason); },
        },
        writableStrategy
      );
    }
  }

  class CountQueuingStrategy {
    constructor(init) { this.highWaterMark = init ? Number(init.highWaterMark) : 0; }
    get size() { return () => 1; }
  }

  class ByteLengthQueuingStrategy {
    constructor(init) { this.highWaterMark = init ? Number(init.highWaterMark) : 0; }
    get size() { return (chunk) => (chunk && chunk.byteLength) || 0; }
  }

  globalThis.ReadableStream = ReadableStream;
  globalThis.ReadableStreamDefaultReader = ReadableStreamDefaultReader;
  globalThis.ReadableStreamDefaultController = ReadableStreamDefaultController;
  globalThis.WritableStream = WritableStream;
  globalThis.WritableStreamDefaultWriter = WritableStreamDefaultWriter;
  globalThis.WritableStreamDefaultController = WritableStreamDefaultController;
  globalThis.TransformStream = TransformStream;
  globalThis.TransformStreamDefaultController = TransformStreamDefaultController;
  globalThis.CountQueuingStrategy = CountQueuingStrategy;
  globalThis.ByteLengthQueuingStrategy = ByteLengthQueuingStrategy;
})();
