import React from "react";
import ReactDOM from "react-dom/client";
import { FocusStyleManager } from "@blueprintjs/core";

import "normalize.css/normalize.css";
import "@blueprintjs/core/lib/css/blueprint.css";
import "@blueprintjs/icons/lib/css/blueprint-icons.css";
import "./theme.css";

import { SandboxView } from "./SandboxView";

FocusStyleManager.onlyShowFocusOnTabs();

// The token arrives in the query string and the server answers with a
// SameSite=Strict cookie, so the page can drop it from the address bar right
// away: nothing should keep a live credential in scrollback, in a bookmark, or
// in whatever the browser syncs.
//
// The fragment survives. It never reaches the server anyway, so it carries none
// of the same risk.
if (window.location.search.includes("token=")) {
  window.history.replaceState(
    {},
    "",
    window.location.pathname + window.location.hash,
  );
}

/**
 * One surface: the console.
 *
 * The shell is a flex column rather than a bare mount point because the
 * console's own shell is `flex: 1` inside it, and the fleet/detail divider is a
 * grid column rather than a border — a plain block here leaves both at content
 * height and stops the divider halfway down the window.
 */
function Shell() {
  return (
    <div className="wb-shell">
      <SandboxView />
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Shell />
  </React.StrictMode>,
);
