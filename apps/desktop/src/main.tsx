import React from "react";
import ReactDOM from "react-dom/client";

import App from "./App";
import { markUntrustedMetrics } from "./charSize";
import "./style.css";

// Before the first render: the correction is a stylesheet rule keyed on a class
// on <html>, and adding it afterwards would paint the window wrong once.
markUntrustedMetrics();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
