import { canConnect, canWake } from "./status";
import type { Device, PowerAction } from "./types";

export type Verb = "screen" | "terminal" | "wake" | PowerAction;

export interface PaletteEntry {
  device: Device;
  verb: Verb;
  /** Already-translated label, e.g. "Terminal · Windows PC". */
  label: string;
}

export const DESTRUCTIVE: ReadonlySet<Verb> = new Set<Verb>(["sleep", "restart", "shutdown"]);

/** Which verbs make sense for a device right now. */
export function verbsFor(d: Device): Verb[] {
  const verbs: Verb[] = [];
  if (canConnect(d)) verbs.push("screen", "terminal", "lock", "sleep", "restart", "shutdown");
  if (canWake(d)) verbs.push("wake");
  return verbs;
}

/** Every command for every device, filtered by all whitespace-separated words of `query`. */
export function paletteEntries(
  devices: Device[],
  query: string,
  verbLabel: (v: Verb) => string,
): PaletteEntry[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const out: PaletteEntry[] = [];
  for (const device of devices) {
    for (const verb of verbsFor(device)) {
      const label = `${verbLabel(verb)} · ${device.name}`;
      const hay = label.toLowerCase();
      if (words.every((w) => hay.includes(w))) out.push({ device, verb, label });
    }
  }
  return out;
}
