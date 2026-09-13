import React from "react";
import ReactDOM from "react-dom/client";

import "./theme.css";

import { App } from "./App";

// The token arrives in the query string and the server answers with a
// SameSite=Strict cookie, so the page drops it from the address bar right
// away: nothing should keep a live credential in scrollback or a bookmark.
// The fragment survives; it never reaches the server.
if (window.location.search.includes("token=")) {
  window.history.replaceState({}, "", window.location.pathname + window.location.hash);
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
