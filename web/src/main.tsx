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
//
// The fragment survives, because it names which surface to open and that is
// worth putting in a message to a colleague. It never reaches the server
// anyway, so it carries none of the same risk.
if (window.location.search.includes("token=")) {
  window.history.replaceState(
    {},
    "",
    window.location.pathname + window.location.hash,
  );
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
  // The fragment wins over the remembered choice, so a URL can name a surface:
  // `…/#board` opens the board whatever this browser was last looking at, which
  // is what makes a link to it worth sending.
  const [surface, setSurface] = React.useState<Surface>(() => {
    const head = window.location.hash.replace(/^#/, "").split("/")[0];
    if (head === "board" || head === "console") return head;
    return (localStorage.getItem("h5i.surface") as Surface) ?? "console";
  });
  const pick = (s: Surface) => {
    setSurface(s);
    localStorage.setItem("h5i.surface", s);
    window.history.replaceState({}, "", `${window.location.pathname}#${s}`);
  };
  // `#board/<thread>` opens straight into one conversation, which is the link
  // worth sending to a colleague — "look at this thread", not "open the board
  // and find it".
  const initialThread =
    surface === "board"
      ? (window.location.hash.replace(/^#/, "").split("/")[1] ?? null)
      : null;
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
        {surface === "console" ? (
          <SandboxView />
        ) : (
          <BoardView initialThread={initialThread} />
        )}
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Shell />
  </React.StrictMode>,
);
