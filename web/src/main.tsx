import React from "react";
import ReactDOM from "react-dom/client";
import { FocusStyleManager } from "@blueprintjs/core";

import "normalize.css/normalize.css";
import "@blueprintjs/core/lib/css/blueprint.css";
import "@blueprintjs/icons/lib/css/blueprint-icons.css";
import "./theme.css";
import "./board.css";

import { SandboxView } from "./SandboxView";
import { BoardView } from "./BoardView";

FocusStyleManager.onlyShowFocusOnTabs();

// The token arrives in the query string and the server answers with a
// SameSite=Strict cookie, so the page can drop it from the address bar right
// away: nothing should keep a live credential in scrollback, in a bookmark, or
// in whatever the browser syncs.
if (window.location.search.includes("token=")) {
  window.history.replaceState({}, "", window.location.pathname);
}

type Surface = "console" | "board";

/**
 * Two surfaces, one shell.
 *
 * Not a router: there is one page, and adding a routing library to switch
 * between two panes would be more machinery than the choice deserves. The
 * surface is remembered so a reload lands where you were.
 *
 * They are deliberately not merged. The console answers "what is this box
 * doing"; the board answers "what are these agents telling each other". They
 * look different because they *are* different instruments, and a reader should
 * know which one they are holding without reading a label.
 */
function Shell() {
  const [surface, setSurface] = React.useState<Surface>(
    () => (localStorage.getItem("h5i.surface") as Surface) ?? "console",
  );
  const pick = (s: Surface) => {
    setSurface(s);
    localStorage.setItem("h5i.surface", s);
  };
  return (
    <div className="wb-shell">
      <div className="wb-tabs" role="tablist" aria-label="surface">
        {/* The wordmark names the product, so it lives with the switch between
            its two screens rather than inside either one of them. */}
        <span
          className="wb-brand"
          title="h5i — read-only · loopback only · lifecycle verbs stay in the CLI"
        >
          h5i
        </span>
        <button
          type="button"
          role="tab"
          aria-selected={surface === "console"}
          className={`wb-tab for-console${surface === "console" ? " is-on" : ""}`}
          onClick={() => pick("console")}
        >
          console
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={surface === "board"}
          className={`wb-tab for-board${surface === "board" ? " is-on" : ""}`}
          onClick={() => pick("board")}
        >
          board
        </button>
      </div>
      <div className="wb-surface">
        {surface === "console" ? <SandboxView /> : <BoardView />}
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Shell />
  </React.StrictMode>,
);
