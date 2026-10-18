import type { AppEvent, ConsentRequest, Device, PairingSas, Snapshot } from "./types";

export interface Toast {
  id: number;
  level: "info" | "error";
  text: string;
}

export interface AppState {
  snapshot: Snapshot;
  consents: ConsentRequest[];
  pairingSas: PairingSas | null;
  pairingResult: { success: boolean; reason: string; peerName: string } | null;
  toasts: Toast[];
  nextToast: number;
}

export const initialState: AppState = {
  snapshot: { agent: "unreachable", status: null, devices: [], sessions: [], error: null },
  consents: [],
  pairingSas: null,
  pairingResult: null,
  toasts: [],
  nextToast: 1,
};

/** Online first, then by name: the order of the grid, the tray and the palette. */
export function sortDevices(devices: Device[]): Device[] {
  return [...devices].sort(
    (a, b) => Number(b.online) - Number(a.online) || a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
  );
}

export function reduce(state: AppState, e: AppEvent): AppState {
  switch (e.kind) {
    case "snapshot":
      return { ...state, snapshot: { ...e.snapshot, devices: sortDevices(e.snapshot.devices) } };
    case "agent":
      return {
        ...state,
        snapshot: e.agent === "unreachable" ? { agent: "unreachable", status: null, devices: [], sessions: [], error: e.error } : { ...state.snapshot, agent: e.agent, error: e.error },
        // Prompts belong to a connection that no longer exists.
        consents: e.agent === "unreachable" ? [] : state.consents,
      };
    case "device_changed": {
      const rest = state.snapshot.devices.filter((d) => d.id !== e.device.id);
      return { ...state, snapshot: { ...state.snapshot, devices: sortDevices([...rest, e.device]) } };
    }
    case "device_removed":
      return { ...state, snapshot: { ...state.snapshot, devices: state.snapshot.devices.filter((d) => d.id !== e.id) } };
    case "signal_state":
      return state.snapshot.status
        ? { ...state, snapshot: {
          ...state.snapshot,
          status: { ...state.snapshot.status, signalConnected: e.connected },
          // Older agents only emit signal_state when their presence cache becomes stale.
          devices: e.connected ? state.snapshot.devices : state.snapshot.devices.map((device) => ({
            ...device, online: false,
            state: device.state === "sleeping" || device.state === "hibernating" ? device.state : "offline",
          })),
        } }
        : state;
    case "sessions_changed":
      return { ...state, snapshot: { ...state.snapshot, sessions: e.sessions } };
    case "consent_request":
      return { ...state, consents: [...state.consents.filter((c) => c.requestId !== e.request.requestId), e.request] };
    case "consent_resolved":
      return { ...state, consents: state.consents.filter((c) => c.requestId !== e.requestId) };
    case "pairing_sas":
      return { ...state, pairingSas: e.sas, pairingResult: null };
    case "pairing_finished":
      return { ...state, pairingSas: null, pairingResult: { success: e.success, reason: e.reason, peerName: e.peerName } };
    case "notice":
      return pushToast(state, e.level, e.text);
    default:
      return state; // terminal events are handled by the terminal panel
  }
}

export function pushToast(state: AppState, level: Toast["level"], text: string): AppState {
  const toast = { id: state.nextToast, level, text };
  return { ...state, toasts: [...state.toasts.slice(-3), toast], nextToast: state.nextToast + 1 };
}

export function dismissToast(state: AppState, id: number): AppState {
  return { ...state, toasts: state.toasts.filter((t) => t.id !== id) };
}
