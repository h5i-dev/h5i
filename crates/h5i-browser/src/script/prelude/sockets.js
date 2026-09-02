// Long-lived connections: `WebSocket` and `EventSource`.
//
// Its own source, parsed only when a page reads one of those two names. Most
// pages read neither, and the ones that do pay a few hundred microseconds to
// parse this at the moment they ask — see `TIERS` in `mod.rs` for the rule.
//
// The drain hooks below *replace* the do-nothing versions the core installs.
// A page with no sockets has nothing to deliver, and the settle loop asking it
// every round is how the core can answer without this file being here.
(function () {
  "use strict";

  const api = globalThis.__h5i;
  const { withStack } = globalThis.__h5iInternals;
  const { Event, EventTarget, console } = globalThis;

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
})();
