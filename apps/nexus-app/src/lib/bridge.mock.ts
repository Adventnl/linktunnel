import type { AppEvent, Bridge, Device, Snapshot } from "./types";

/**
 * A believable fake world for running the UI in a plain browser (design work, screenshots,
 * tests). It is never used inside the real app: `bridge.ts` picks it only when Tauri is absent.
 */
const now = Date.now();
const dev = (d: Partial<Device> & Pick<Device, "id" | "name">): Device => ({
  state: "online",
  online: true,
  lastSeenMs: now,
  osFamily: "windows",
  osVersion: "Windows 11 23H2",
  path: "lan",
  activeUser: "alex",
  batteryPercent: null,
  fingerprint: "S2TX-XX7H-3NSH-A56H-PRC7",
  unattended: false,
  grantsOnPeer: ["screen", "input", "clipboard", "files", "terminal", "power", "wake", "audio", "privacy"],
  ...d,
});

const DEVICES: Device[] = [
  dev({ id: "01", name: "Windows PC", state: "idle", activeUser: "alex", unattended: true }),
  dev({ id: "02", name: "Mac mini", osFamily: "macos", osVersion: "macOS 26.1", state: "in_use", activeUser: "sam", path: "direct" }),
  dev({ id: "03", name: "Gaming laptop", state: "locked", batteryPercent: 64, lastSeenMs: now - 30_000, path: "relay" }),
  dev({ id: "04", name: "Old tower", state: "sleeping", online: false, lastSeenMs: now - 3_600_000, activeUser: "", path: "none" }),
  dev({ id: "05", name: "Home server", osFamily: "linux", osVersion: "Ubuntu 24.04 LTS", state: "online", activeUser: "", unattended: true }),
  dev({ id: "06", name: "Office desktop", state: "offline", online: false, lastSeenMs: now - 5 * 86_400_000, activeUser: "", path: "none" }),
];

export const MOCK_SNAPSHOT: Snapshot = {
  agent: "connected",
  status: {
    deviceId: "ffeeddccbbaa99887766554433221100",
    deviceName: "MacBook",
    fingerprint: "NX-KGJV-OQAF-QYQD-QLNF-Z55W-SOBK-Z4",
    agentVersion: "0.1.0",
    signalConnected: true,
    signalUrl: "wss://signal.example.com/v1/ws",
    lockdown: false,
    pairedDevices: DEVICES.length,
    activeSessions: 0,
  },
  devices: DEVICES,
  sessions: [],
  error: null,
};

export function createMockBridge(snapshot: Snapshot = MOCK_SNAPSHOT): Bridge {
  const listeners = new Set<(e: AppEvent) => void>();
  const emit = (e: AppEvent) => listeners.forEach((l) => l(e));
  let lockdown = snapshot.status?.lockdown ?? false;
  let nextId = 100;
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  return {
    mock: true,
    snapshot: async () => structuredClone(snapshot),
    onEvent: async (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    connectScreen: async () => {},
    wake: async (id) => {
      // Pretend the machine takes a moment to come up.
      void sleep(1500).then(() => {
        const d = DEVICES.find((x) => x.id === id);
        if (d) emit({ kind: "device_changed", device: { ...d, state: "online", online: true, lastSeenMs: Date.now() } });
      });
    },
    power: async () => {},
    setUnattended: async (id, on) => {
      const d = DEVICES.find((x) => x.id === id);
      if (d) emit({ kind: "device_changed", device: { ...d, unattended: on } });
    },
    disconnectSessions: async () => emit({ kind: "sessions_changed", sessions: [] }),
    panic: async () => {
      lockdown = true;
      if (snapshot.status) emit({ kind: "snapshot", snapshot: { ...snapshot, sessions: [], status: { ...snapshot.status, lockdown } } });
    },
    setLockdown: async (on) => {
      lockdown = on;
      if (snapshot.status) emit({ kind: "snapshot", snapshot: { ...snapshot, status: { ...snapshot.status, lockdown } } });
    },
    unpair: async (id) => emit({ kind: "device_removed", id }),
    pairOffer: async () => {
      const pairingId = nextId++;
      void sleep(1200).then(() =>
        emit({
          kind: "pairing_sas",
          sas: { pairingId, words: ["upset", "vacant", "imitate", "column"], digits: "979936", peerName: "Office desktop", peerFingerprint: "S2TX-XX7H-3NSH-A56H-PRC7" },
        }),
      );
      return { pairingId, code: "orbit candle harvest meadow tunnel silver" };
    },
    pairJoin: async () => {
      const pairingId = nextId++;
      void sleep(600).then(() =>
        emit({
          kind: "pairing_sas",
          sas: { pairingId, words: ["upset", "vacant", "imitate", "column"], digits: "979936", peerName: "Office desktop", peerFingerprint: "S2TX-XX7H-3NSH-A56H-PRC7" },
        }),
      );
      return { pairingId };
    },
    pairConfirm: async (pairingId, ok) => {
      void sleep(300).then(() => emit({ kind: "pairing_finished", pairingId, success: ok, reason: ok ? "" : "the words did not match", peerName: "Office desktop" }));
    },
    pairCancel: async () => {},
    consentDecide: async (requestId) => emit({ kind: "consent_resolved", requestId }),
    terminalOpen: async () => {
      const terminalId = nextId++;
      const enc = (s: string) => btoa(unescape(encodeURIComponent(s)));
      void sleep(200).then(() => {
        emit({ kind: "terminal_ready", terminalId, name: "", reattached: false });
        emit({ kind: "terminal_output", terminalId, dataB64: enc("\x1b[1;32malex@mock\x1b[0m:\x1b[1;34m~\x1b[0m$ ") });
      });
      return { terminalId };
    },
    terminalInput: async (terminalId, dataB64) => {
      // Echo what was typed, and answer Enter with a fresh prompt.
      const text = decodeURIComponent(escape(atob(dataB64)));
      const out = text === "\r" ? "\r\n\x1b[1;32malex@mock\x1b[0m:\x1b[1;34m~\x1b[0m$ " : text;
      emit({ kind: "terminal_output", terminalId, dataB64: btoa(unescape(encodeURIComponent(out))) });
    },
    terminalResize: async () => {},
    terminalClose: async () => {},
    quit: async () => {},
  };
}
