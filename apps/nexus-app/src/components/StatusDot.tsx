import { DOT_COLOR } from "../lib/status";
import type { PresenceState } from "../lib/types";

export function StatusDot({ state, label, pulse = false }: { state: PresenceState; label: string; pulse?: boolean }) {
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className={`inline-block h-3 w-3 shrink-0 rounded-full ${pulse ? "pulse" : ""}`}
      style={{ background: DOT_COLOR[state], boxShadow: `0 0 0 3px color-mix(in srgb, ${DOT_COLOR[state]} 22%, transparent)` }}
    />
  );
}
