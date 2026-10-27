/** What the UI knows about the world. The Rust side (src-tauri) produces exactly these shapes. */

export type PresenceState =
  | "online"
  | "idle"
  | "locked"
  | "in_use"
  | "sleeping"
  | "hibernating"
  | "offline"
  | "unknown";

export type OsFamily = "windows" | "macos" | "linux" | "unknown";
export type PathKind = "lan" | "direct" | "relay" | "none";

export interface Device {
  /** Lower-case hex of the 16-byte device id. */
  id: string;
  name: string;
  state: PresenceState;
  online: boolean;
  lastSeenMs: number;
  osFamily: OsFamily;
  osVersion: string;
  path: PathKind;
  activeUser: string;
  /** null when the device has no battery or does not report one. */
  batteryPercent: number | null;
  fingerprint: string;
  /** True if this device may use this one without a person approving each session. */
  unattended: boolean;
  /** What this machine may do on that device. */
  grantsOnPeer: string[];
}

export interface AgentStatus {
  deviceId: string;
  deviceName: string;
  fingerprint: string;
  agentVersion: string;
  signalConnected: boolean;
  signalUrl: string;
  lockdown: boolean;
  pairedDevices: number;
  activeSessions: number;
}

/** Someone connected to this machine right now. */
export interface ActiveSession {
  id: number;
  deviceId: string;
  peerName: string;
  startedMs: number;
  path: PathKind;
  grants: string[];
}

export interface Snapshot {
  /** "unreachable" when no agent is running for this user. */
  agent: "connected" | "unreachable";
  status: AgentStatus | null;
  devices: Device[];
  /** Who is connected to this machine right now. */
  sessions: ActiveSession[];
  error: string | null;
}

export interface ConsentRequest {
  requestId: number;
  peerName: string;
  peerFingerprint: string;
  grants: string[];
  timeoutSecs: number;
  path: PathKind;
  /** Local time the request arrived (ms), to draw the countdown. */
  receivedAtMs: number;
}

export interface PairingSas {
  pairingId: number;
  words: string[];
  digits: string;
  peerName: string;
  peerFingerprint: string;
}

export type PowerAction = "lock" | "sleep" | "restart" | "shutdown";

export type AppEvent =
  | { kind: "agent"; agent: Snapshot["agent"]; error: string | null }
  | { kind: "snapshot"; snapshot: Snapshot }
  | { kind: "device_changed"; device: Device }
  | { kind: "device_removed"; id: string }
  | { kind: "signal_state"; connected: boolean }
  | { kind: "sessions_changed"; sessions: ActiveSession[] }
  | { kind: "consent_request"; request: ConsentRequest }
  | { kind: "consent_resolved"; requestId: number }
  | { kind: "pairing_sas"; sas: PairingSas }
  | { kind: "pairing_finished"; pairingId: number; success: boolean; reason: string; peerName: string }
  | { kind: "terminal_ready"; terminalId: number; name: string; reattached: boolean }
  | { kind: "terminal_output"; terminalId: number; dataB64: string }
  | { kind: "terminal_ended"; terminalId: number; exited: boolean; exitCode: number; reason: string }
  | { kind: "notice"; level: "info" | "error"; text: string };

/** Everything the UI can ask the app to do. */
export interface Bridge {
  readonly mock: boolean;
  phoneStatus(): Promise<PhoneStatus>;
  phoneDevices(): Promise<PhoneDevice[]>;
  phonePair(address: string, code: string): Promise<void>;
  phoneConnect(address: string): Promise<void>;
  phoneDisconnect(address: string): Promise<void>;
  phoneStart(options: PhoneOptions): Promise<void>;
  phoneSession(): Promise<PhoneSession>;
  phoneStop(): Promise<void>;
  phoneCalls(): Promise<void>;
  snapshot(): Promise<Snapshot>;
  onEvent(cb: (e: AppEvent) => void): Promise<() => void>;
  connectScreen(id: string): Promise<void>;
  wake(id: string): Promise<void>;
  power(id: string, action: PowerAction): Promise<void>;
  setUnattended(id: string, on: boolean): Promise<void>;
  setLockdown(on: boolean): Promise<void>;
  /** Ends the live incoming sessions of one device, or of everyone. */
  disconnectSessions(id?: string): Promise<void>;
  /** Lockdown on and everyone out. */
  panic(): Promise<void>;
  unpair(id: string): Promise<void>;
  pairOffer(): Promise<{ pairingId: number; code: string }>;
  pairJoin(code: string): Promise<{ pairingId: number }>;
  pairConfirm(pairingId: number, wordsMatch: boolean): Promise<void>;
  pairCancel(pairingId: number): Promise<void>;
  consentDecide(requestId: number, allow: boolean, remember: boolean): Promise<void>;
  terminalOpen(id: string, cols: number, rows: number, name?: string): Promise<{ terminalId: number }>;
  terminalInput(terminalId: number, dataB64: string): Promise<void>;
  terminalResize(terminalId: number, cols: number, rows: number): Promise<void>;
  terminalClose(terminalId: number, kill: boolean): Promise<void>;
  /** Tells the OS-level tray to quit the whole app. */
  quit(): Promise<void>;
}

export interface PhoneDevice { serial: string; state: string; model: string }
export interface PhoneOptions { serial: string; screenOff: boolean; mediaAudio: boolean; maxSize: number; maxFps: number }
export interface PhoneSession { serial: string | null; error: string | null }
export interface PhoneStatus {
  adb: { Ok: string } | { Err: string };
  scrcpy: { Ok: string } | { Err: string };
  platform: string;
  session: PhoneSession;
}
