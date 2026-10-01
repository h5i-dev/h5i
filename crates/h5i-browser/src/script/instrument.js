// h5i DOM instrument: the shared payload injected into a page (proxy vehicle)
// or installed before page scripts (engine vehicle). It detects prototype
// pollution, records source->sink flows, and logs postMessage traffic, then
// reports once as JSON. __H5I_TOKEN__, __H5I_VALUE__ and __H5I_BEACON__ are
// substituted by the host before delivery.
(function () {
  "use strict";
  var TOKEN = "__H5I_TOKEN__";
  var VALUE = "__H5I_VALUE__";
  var BEACON = "__H5I_BEACON__";
  var CAP = 2048; // never ship an unbounded sample of a page's own strings

  var flows = [];
  var messages = [];
  var seen = Object.create(null);

  function clip(s) {
    try {
      s = String(s);
    } catch (e) {
      return "";
    }
    return s.length > CAP ? s.slice(0, CAP) : s;
  }

  // The source pool: the attacker-controlled parts of this navigation. A sink
  // argument that quotes a long enough slice of these is a source->sink flow.
  function sources() {
    var out = [];
    try { out.push(["hash", decodeURIComponent(location.hash.replace(/^#/, ""))]); } catch (e) {}
    try { out.push(["search", decodeURIComponent(location.search.replace(/^\?/, ""))]); } catch (e) {}
    try { if (window.name) out.push(["name", String(window.name)]); } catch (e) {}
    try { if (document.referrer) out.push(["referrer", String(document.referrer)]); } catch (e) {}
    return out;
  }

  // A sink argument is tainted when it contains a run of >=8 chars that also
  // appears verbatim in a source. Cheap, and it does not false-positive on the
  // short tokens the page itself uses.
  function tainted(arg) {
    var a = clip(arg);
    if (a.length < 8) return null;
    var src = sources();
    for (var i = 0; i < src.length; i++) {
      var val = src[i][1];
      for (var start = 0; start + 8 <= val.length; start += 8) {
        var needle = val.slice(start, start + 8);
        if (needle && a.indexOf(needle) !== -1) return src[i][0];
      }
    }
    return null;
  }

  function flow(sink, arg) {
    try {
      var source = tainted(arg);
      if (!source) return;
      var key = source + "|" + sink;
      if (seen[key]) return;
      seen[key] = true;
      flows.push({ source: source, sink: sink, sample: clip(arg) });
    } catch (e) {}
  }

  // Wrap the DOM/JS sinks. Each wrapper records a flow then calls through, so
  // the page behaves exactly as it would without us.
  function wrapSetter(proto, name, sink) {
    try {
      var d = Object.getOwnPropertyDescriptor(proto, name);
      if (!d || !d.set) return;
      var orig = d.set;
      Object.defineProperty(proto, name, {
        configurable: true,
        enumerable: d.enumerable,
        get: d.get,
        set: function (v) {
          flow(sink, v);
          return orig.call(this, v);
        },
      });
    } catch (e) {}
  }

  function wrapMethod(obj, name, sink, argIndex) {
    try {
      var orig = obj[name];
      if (typeof orig !== "function") return;
      obj[name] = function () {
        try { flow(sink, arguments[argIndex]); } catch (e) {}
        return orig.apply(this, arguments);
      };
    } catch (e) {}
  }

  try { wrapSetter(Element.prototype, "innerHTML", "innerHTML"); } catch (e) {}
  try { wrapSetter(Element.prototype, "outerHTML", "outerHTML"); } catch (e) {}
  try { wrapMethod(Element.prototype, "insertAdjacentHTML", "insertAdjacentHTML", 1); } catch (e) {}
  try { wrapMethod(Element.prototype, "setAttribute", "setAttribute", 1); } catch (e) {}
  try { wrapMethod(document, "write", "document.write", 0); } catch (e) {}
  try { wrapMethod(window, "eval", "eval", 0); } catch (e) {}

  // postMessage: a Chromium-only feature here (a single-realm engine has no
  // second frame to receive from). Log the origin and a clipped sample.
  try {
    window.addEventListener("message", function (ev) {
      try {
        if (messages.length < 64) {
          var data = typeof ev.data === "string" ? ev.data : JSON.stringify(ev.data);
          messages.push({ origin: ev.origin, data: clip(data) });
        }
      } catch (e) {}
    }, true);
  } catch (e) {}

  function report() {
    var polluted = false;
    try {
      polluted = Object.prototype[TOKEN] === VALUE;
    } catch (e) {}
    var payload = {
      url: location.href,
      canary: polluted,
      property: polluted ? TOKEN : null,
      flows: flows,
      messages: messages,
    };
    var body = "";
    try { body = JSON.stringify(payload); } catch (e) { body = '{"url":"' + clip(location.href) + '","error":"serialize"}'; }
    try {
      var blob = new Blob([body], { type: "text/plain" });
      if (!navigator.sendBeacon || !navigator.sendBeacon(BEACON, blob)) {
        // sendBeacon is unavailable or refused; a keepalive fetch is the
        // fallback and still avoids a CORS preflight with a text/plain body.
        fetch(BEACON, { method: "POST", body: body, keepalive: true, mode: "no-cors" });
      }
    } catch (e) {}
  }

  // Report after the page has had a tick to parse the payload and run its own
  // scripts, and again on unload to catch late pollution.
  try {
    if (document.readyState === "complete") setTimeout(report, 300);
    else window.addEventListener("load", function () { setTimeout(report, 300); });
    window.addEventListener("pagehide", report, true);
  } catch (e) {
    setTimeout(report, 300);
  }
})();
