// h5i server-side prototype-pollution gadget probe (Silent Spring / GHunter).
//
// Loaded with `node --require gadget-probe.js <target>`, it pollutes
// Object.prototype with unique sentinel markers, wraps the dangerous sinks, and
// records which pollution reaches which sink. The sentinel is carried INTO the
// sink argument, so a kernel observer (the eBPF detect lane) can confirm the
// same flow by identity rather than by coincidence. It writes one JSON report
// to the path in H5I_GADGET_REPORT, or to stdout.
"use strict";

var crypto = require("crypto");
var RUN = crypto.randomBytes(4).toString("hex");

// A unique, greppable marker per gadget property. The value is what flows to
// the sink, so the host and the kernel lane both match on this exact string.
function mark(name) {
  return "h5iGADGET_" + name + "_" + RUN;
}

var findings = [];
var seen = Object.create(null);
function record(gadget, prop, sink, detail) {
  var key = gadget + "|" + sink;
  if (seen[key]) return;
  seen[key] = true;
  findings.push({ gadget: gadget, property: prop, sink: sink, marker: mark(prop), detail: String(detail).slice(0, 256) });
}

// The pollutions. String-valued gadgets go straight onto Object.prototype; the
// command gadget needs `env` to be an object carrying a polluted NODE_OPTIONS.
var PROTO = Object.prototype;
function pollute() {
  try { PROTO.shell = mark("shell"); } catch (e) {}
  try { PROTO.main = mark("main"); } catch (e) {}
  // `fetch` options SSRF (GHunter): method/body are read off the options object.
  try { PROTO.method = mark("method"); } catch (e) {}
  // The command-injection chain (Silent Spring G1/G10): a polluted env whose
  // NODE_OPTIONS the spawned process would inherit.
  try {
    Object.defineProperty(PROTO, "env", {
      configurable: true,
      enumerable: false,
      get: function () {
        return { NODE_OPTIONS: mark("NODE_OPTIONS") };
      },
    });
  } catch (e) {}
}

// A value that was inherited from Object.prototype (not an own property) and
// equals our marker is a reachable gadget.
function inherited(obj, key) {
  if (obj == null) return undefined;
  if (Object.prototype.hasOwnProperty.call(obj, key)) return undefined;
  return obj[key];
}

function wrap(obj, name, onCall) {
  try {
    var orig = obj[name];
    if (typeof orig !== "function") return;
    obj[name] = function () {
      try { onCall(arguments); } catch (e) {}
      return orig.apply(this, arguments);
    };
  } catch (e) {}
}

function install() {
  var cp = require("child_process");
  var execFamily = ["exec", "execSync", "spawn", "spawnSync", "execFile", "execFileSync", "fork"];
  execFamily.forEach(function (fn) {
    wrap(cp, fn, function (args) {
      // The options object is the last object-typed argument.
      var opts = null;
      for (var i = args.length - 1; i >= 0; i--) {
        if (args[i] && typeof args[i] === "object" && !Array.isArray(args[i])) { opts = args[i]; break; }
      }
      if (inherited(opts || {}, "shell") === mark("shell")) {
        record("shell", "shell", "child_process." + fn, "inherited options.shell");
      }
      var env = inherited(opts || {}, "env");
      if (env && env.NODE_OPTIONS === mark("NODE_OPTIONS")) {
        record("NODE_OPTIONS", "env", "child_process." + fn, "inherited env.NODE_OPTIONS");
      }
    });
  });

  // require / import of a package with no "main": the loader falls back to the
  // polluted Object.prototype.main. We see the marker become the resolved path.
  try {
    var Module = require("module");
    var builtins = Module.builtinModules || [];
    wrap(Module, "_load", function (args) {
      var request = args[0];
      // The gadget needs a bare package name (no path, not a core module):
      // only then can a missing "main" fall through to the polluted prototype.
      if (
        typeof request === "string" &&
        request.indexOf("/") === -1 &&
        !request.startsWith("node:") &&
        builtins.indexOf(request) === -1 &&
        inherited({}, "main") === mark("main")
      ) {
        record("main", "main", "require", "require('" + request + "') with polluted main");
      }
    });
  } catch (e) {}

  // fetch SSRF (GHunter): a polluted options.method/body reaching the call.
  try {
    if (typeof globalThis.fetch === "function") {
      wrap(globalThis, "fetch", function (args) {
        var opts = args[1];
        if (inherited(opts || {}, "method") === mark("method")) {
          record("method", "method", "fetch", "inherited options.method");
        }
      });
    }
  } catch (e) {}
}

function report() {
  var out = JSON.stringify({ run: RUN, findings: findings });
  try {
    var path = process.env.H5I_GADGET_REPORT;
    if (path) require("fs").writeFileSync(path, out);
    else process.stdout.write(out + "\n");
  } catch (e) {
    try { process.stdout.write(out + "\n"); } catch (e2) {}
  }
}

pollute();
install();
process.on("exit", report);
