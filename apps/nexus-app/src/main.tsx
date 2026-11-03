import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
import "./index.css";
import { App } from "./App";
import { selectBridge } from "./lib/bridge";

const root = document.getElementById("root");
if (root) {
  const bridge = selectBridge();
  document.documentElement.dataset.runtime = bridge.mock ? "browser" : "native";
  createRoot(root).render(
    <StrictMode>
      <App bridge={bridge} />
    </StrictMode>,
  );
}
