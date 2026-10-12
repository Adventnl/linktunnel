import type { Device, PathKind, PresenceState } from "./types";

/** The dot colours of spec §8.1: 🟢 online/idle · 🔵 in use · 🟡 locked · 🟠 sleeping · 🔴 offline · ⚪ unknown. */
export const DOT_COLOR: Record<PresenceState, string> = {
  online: "var(--dot-green)",
  idle: "var(--dot-green)",
  in_use: "var(--dot-blue)",
  locked: "var(--dot-yellow)",
  sleeping: "var(--dot-orange)",
  hibernating: "var(--dot-orange)",
  offline: "var(--dot-red)",
  unknown: "var(--dot-grey)",
};

export function isAsleep(d: Pick<Device, "state">): boolean {
  return d.state === "sleeping" || d.state === "hibernating";
}

/** Connect/terminal/files need a machine that is up. */
export function canConnect(d: Pick<Device, "online" | "state">): boolean {
  return d.online && !isAsleep(d);
}

/** Wake is offered for anything that is not already up. */
export function canWake(d: Pick<Device, "online" | "state">): boolean {
  return !canConnect(d);
}

export function pathLabelKey(p: PathKind): string {
  return `path.${p}`;
}

/** "12s ago" style relative time as a [key, n] pair for the i18n layer. */
export function seenParts(lastSeenMs: number, nowMs: number): { key: string; n: number } {
  if (lastSeenMs <= 0) return { key: "seen.never", n: 0 };
  const secs = Math.max(0, Math.floor((nowMs - lastSeenMs) / 1000));
  if (secs < 5) return { key: "seen.now", n: 0 };
  if (secs < 60) return { key: "seen.secs", n: secs };
  if (secs < 3600) return { key: "seen.mins", n: Math.floor(secs / 60) };
  if (secs < 86400) return { key: "seen.hours", n: Math.floor(secs / 3600) };
  return { key: "seen.days", n: Math.floor(secs / 86400) };
}
