import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppEvent, Bridge, PowerAction, Snapshot } from "./types";

/** The real bridge: every call is a Tauri command implemented in src-tauri/src/commands.rs. */
export const tauriBridge: Bridge = {
  mock: false,
  snapshot: () => invoke<Snapshot>("snapshot"),
  onEvent: async (cb) => {
    // One channel carries every event; the Rust side tags them with `kind`.
    const off = await listen<AppEvent>("nexus://event", (e) => cb(e.payload));
    return off;
  },
  connectScreen: (id) => invoke("connect_screen", { id }),
  wake: (id) => invoke("wake", { id }),
  power: (id: string, action: PowerAction) => invoke("power", { id, action }),
  setUnattended: (id, on) => invoke("set_unattended", { id, on }),
  setLockdown: (on) => invoke("set_lockdown", { on }),
  unpair: (id) => invoke("unpair", { id }),
  pairOffer: () => invoke("pair_offer"),
  pairJoin: (code) => invoke("pair_join", { code }),
  pairConfirm: (pairingId, wordsMatch) => invoke("pair_confirm", { pairingId, wordsMatch }),
  pairCancel: (pairingId) => invoke("pair_cancel", { pairingId }),
  consentDecide: (requestId, allow, remember) => invoke("consent_decide", { requestId, allow, remember }),
  terminalOpen: (id, cols, rows, name) => invoke("terminal_open", { id, cols, rows, name: name ?? null }),
  terminalInput: (terminalId, dataB64) => invoke("terminal_input", { terminalId, dataB64 }),
  terminalResize: (terminalId, cols, rows) => invoke("terminal_resize", { terminalId, cols, rows }),
  terminalClose: (terminalId, kill) => invoke("terminal_close", { terminalId, kill }),
  quit: () => invoke("quit"),
};
