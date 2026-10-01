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

  // The source pool: the attacker-controlled parts of this navigation, each
  // clipped — a hostile page can set a megabyte-long window.name, and the taint
  // scan must stay bounded regardless.
  var SRC_CAP = 1024;
  function sources() {
    var out = [];
    function add(name, raw) {
      try {
        var v = String(raw);
        if (v) out.push([name, v.length > SRC_CAP ? v.slice(0, SRC_CAP) : v]);
      } catch (e) {}
    }
    try { add("hash", decodeURIComponent(location.hash.replace(/^#/, ""))); } catch (e) { add("hash", location.hash); }
    try { add("search", decodeURIComponent(location.search.replace(/^\?/, ""))); } catch (e) { add("search", location.search); }
    try { add("name", window.name); } catch (e) {}
    try { add("referrer", document.referrer); } catch (e) {}
    return out;
  }

  // A sink argument is tainted when it contains a run of >=N chars that also
  // appears verbatim in a source. We build the set of source needles once (both
  // sides clipped), then slide once over the arg — O(|sources| + |arg|), not the
  // product. N is large enough that a shared "https://" alone does not trip it;
  // wholly generic windows are skipped.
  var NEEDLE = 12;
  var GENERIC = /^(https?:\/\/|www\.|\/|\s)+$/;
  function tainted(arg) {
    var a = clip(arg);
    if (a.length < NEEDLE) return null;
    var needles = Object.create(null);
    var src = sources();
    for (var i = 0; i < src.length; i++) {
      var val = src[i][1];
      for (var s = 0; s + NEEDLE <= val.length; s++) {
        var n = val.slice(s, s + NEEDLE);
        if (!GENERIC.test(n) && !(n in needles)) needles[n] = src[i][0];
      }
    }
    for (var j = 0; j + NEEDLE <= a.length; j++) {
      var w = a.slice(j, j + NEEDLE);
      if (w in needles) return needles[w];
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
  // eval is deliberately not wrapped: a wrapper turns the page's direct eval
  // into indirect eval (global scope), which changes behaviour of the very
  // page we are measuring. innerHTML/write/setAttribute cover the DOM sinks.

  // postMessage: a Chromium-only feature here (a single-realm engine has no
  // second frame to receive from). Log the origin and a clipped sample.
  try {
    window.addEventListener("message", function (ev) {
      try {
        if (messages.length < 16) {
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
