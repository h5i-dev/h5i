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
if (window.location.search.includes("token=")) {
  window.history.replaceState({}, "", window.location.pathname);
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <div className="wb-shell">
      <SandboxView />
    </div>
  </React.StrictMode>,
);
