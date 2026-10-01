// h5i server-side prototype-pollution gadget probe (Silent Spring / GHunter).
//
// Loaded with NODE_OPTIONS=--require into the in-box target, it pollutes ONE
// Object.prototype property (named by H5I_GADGET_PROP) with a unique sentinel
// marker, wraps the dangerous sinks, and records which pollution reaches which
// sink. One property per run is deliberate: polluting several common names at
// once (`env`, `method`, `main`) breaks most real targets before any gadget is
// reached, exactly what GHunter/Silent Spring avoid. The marker is carried INTO
// the sink argument, so a kernel observer can confirm the flow by identity.
//
// The report is written to H5I_GADGET_REPORT (a file) after every record and on
// exit/termination, so a target killed by a timeout still leaves what was found.
"use strict";

var crypto = require("crypto");
var RUN = crypto.randomBytes(4).toString("hex");
var PROP = process.env.H5I_GADGET_PROP || "";
var REPORT = process.env.H5I_GADGET_REPORT || "";
var MARKER = "h5iGADGET_" + PROP + "_" + RUN;

var findings = [];
var seen = Object.create(null);

function write() {
  var out = JSON.stringify({ run: RUN, property: PROP, findings: findings });
  try {
    if (REPORT) require("fs").writeFileSync(REPORT, out);
    else process.stdout.write(out + "\n");
  } catch (e) {}
}

function record(sink, detail) {
  if (seen[sink]) return;
  seen[sink] = true;
  findings.push({
    gadget: PROP,
    property: PROP,
    sink: sink,
    marker: MARKER,
    detail: String(detail).slice(0, 256),
  });
  write(); // incremental, so a timeout kill still leaves this finding
}

// Pollute the single property this run targets.
//   NODE_OPTIONS -> an `env` object gadget (a spawned child inherits it).
//   main         -> a getter that fires only while a module is being resolved,
//                   which is the real gadget (a package whose package.json has
//                   no own `main` reads the inherited one). It returns undefined
//                   so resolution is unchanged, so it never breaks the target
//                   and never false-positives on unrelated `.main` reads.
//   others       -> a plain string marker read off the sink's options object.
var PROTO = Object.prototype;
var inRequire = 0;
function pollute() {
  try {
    if (PROP === "NODE_OPTIONS") {
      Object.defineProperty(PROTO, "env", {
        configurable: true,
        enumerable: false,
        get: function () {
          return { NODE_OPTIONS: MARKER };
        },
      });
    } else if (PROP === "main") {
      Object.defineProperty(PROTO, "main", {
        configurable: true,
        enumerable: false,
        get: function () {
          if (inRequire > 0) record("require", "a package 'main' fell through to the prototype");
          return undefined;
        },
      });
    } else if (PROP) {
      PROTO[PROP] = MARKER;
    }
  } catch (e) {}
}

// A value inherited from Object.prototype (not an own property) equal to our
// marker is a reachable gadget.
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

function lastOptions(args) {
  for (var i = args.length - 1; i >= 0; i--) {
    if (args[i] && typeof args[i] === "object" && !Array.isArray(args[i])) return args[i];
  }
  return null;
}

function install() {
  var cp = require("child_process");
  ["exec", "execSync", "spawn", "spawnSync", "execFile", "execFileSync", "fork"].forEach(function (fn) {
    wrap(cp, fn, function (args) {
      var opts = lastOptions(args) || {};
      if (inherited(opts, "shell") === MARKER) {
        record("child_process." + fn, "inherited options.shell");
      }
      var env = inherited(opts, "env");
      if (env && env.NODE_OPTIONS === MARKER) {
        record("child_process." + fn, "inherited env.NODE_OPTIONS");
      }
    });
  });

  // Count module resolutions so the `main` getter fires only inside one. The
  // getter itself (installed in pollute) does the recording, so this only has
  // to bracket the original call.
  try {
    var Module = require("module");
    var origLoad = Module._load;
    if (typeof origLoad === "function") {
      Module._load = function () {
        inRequire++;
        try {
          return origLoad.apply(this, arguments);
        } finally {
          inRequire--;
        }
      };
    }
  } catch (e) {}

  // fetch SSRF: a polluted options.method reaching the call.
  try {
    if (typeof globalThis.fetch === "function") {
      wrap(globalThis, "fetch", function (args) {
        if (inherited(args[1] || {}, "method") === MARKER) {
          record("fetch", "inherited options.method");
        }
      });
    }
  } catch (e) {}
}

pollute();
install();
// Write on normal exit and on the signals a run timeout sends, so a server that
// never exits on its own still yields whatever it reached before being stopped.
process.on("exit", write);
["SIGTERM", "SIGINT"].forEach(function (sig) {
  process.on(sig, function () {
    write();
    process.exit(0);
  });
});
