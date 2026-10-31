import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles.css";
import "./workbench.css";
import "./desktop.css";
import "./composer-refinement.css";
import "./login.css";
import "./markdown.css";
import "./conversation-timeline.css";
import "./historical-reader.css";
import "./execution-trace.css";
createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
