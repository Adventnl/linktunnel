import { tauriBridge } from "./bridge.tauri";
import { createMockBridge } from "./bridge.mock";
import type { Bridge } from "./types";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

/** Inside the app: the real bridge. In a plain browser (`npm run dev`): a mock world. */
export function selectBridge(): Bridge {
  return typeof window !== "undefined" && window.__TAURI_INTERNALS__ ? tauriBridge : createMockBridge();
}
