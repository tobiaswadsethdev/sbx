import React from "react";
import ReactDOM from "react-dom/client";
import { LucideProvider } from "lucide-react";

import App from "./App";
import { markUntrustedMetrics } from "./charSize";
import { ICON_SIZE, ICON_STROKE } from "./icons";
import "./style.css";

// Before the first render: the correction is a stylesheet rule keyed on a class
// on <html>, and adding it afterwards would paint the window wrong once.
markUntrustedMetrics();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {/* The one place the icon grid is set. lucide's defaults are a 24-pixel
        icon with a 2-pixel stroke, drawn for interfaces built at a larger
        scale than this one; every icon in the window inherits the numbers in
        `icons.tsx` from here instead. `absoluteStrokeWidth` is what makes the
        stroke a pixel count rather than a ratio, so the handful of icons that
        pass their own `size` stay the same weight as the rest. */}
    <LucideProvider size={ICON_SIZE} strokeWidth={ICON_STROKE} absoluteStrokeWidth>
      <App />
    </LucideProvider>
  </React.StrictMode>,
);
