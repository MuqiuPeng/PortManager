import React from "react";
import ReactDOM from "react-dom/client";

import App from "./App";
import Panel from "./Panel";
import "./theme.css";
import "./styles.css";

// The panel is the same bundle at `index.html#panel`, so both windows share the
// API layer and the event subscription without a router dependency.
const isPanel = window.location.hash === "#panel";
if (isPanel) {
  document.documentElement.classList.add("is-panel");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isPanel ? <Panel /> : <App />}</React.StrictMode>,
);
