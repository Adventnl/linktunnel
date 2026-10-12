import { useState } from "react";
import { DESTRUCTIVE, type Verb } from "../lib/palette";
import { canConnect, canWake, isAsleep, seenParts } from "../lib/status";
import type { Key } from "../lib/i18n";
import type { Device } from "../lib/types";
import { Button } from "./Modal";
import { StatusDot } from "./StatusDot";

type T = (key: Key, args?: Record<string, string | number>) => string;

const OS_GLYPH = { windows: "⊞", macos: "", linux: "🐧", unknown: "◻" } as const;

export function DeviceCard({
  device,
  now,
  wakingSince,
  t,
  onAction,
}: {
  device: Device;
  now: number;
  /** Set while "wake and connect" is in progress. */
  wakingSince: number | null;
  t: T;
  onAction: (device: Device, verb: Verb | "files" | "settings") => void;
}) {
  const [more, setMore] = useState(false);
  const up = canConnect(device);
  const waking = wakingSince !== null;
  const seen = seenParts(device.lastSeenMs, now);
  const stateLabel = t(`state.${device.state}` as Key);
  const pathLabel = t(`path.${device.path}` as Key);
  const verbs: Verb[] = ["lock", "sleep", "restart", "shutdown"];
  return (
    <article
      aria-label={`${device.name}, ${stateLabel}`}
      className="flex flex-col gap-3 rounded-xl border border-line bg-panel p-4 transition-colors hover:border-accent/60"
    >
      <header className="flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <StatusDot state={device.state} label={stateLabel} pulse={waking} />
          <div className="min-w-0">
            <h3 className="truncate text-base font-semibold">{device.name}</h3>
            <p className="truncate text-xs text-muted">
              <span aria-hidden="true">{OS_GLYPH[device.osFamily]} </span>
              {device.osVersion || t("state.unknown")}
            </p>
          </div>
        </div>
        <span className="shrink-0 rounded-full bg-panel2 px-2 py-0.5 text-xs text-muted" title={device.unattended ? t("card.unattended") : t("card.attended")}>
          {device.unattended ? "🔓" : "🔒"} {pathLabel}
        </span>
      </header>

      <dl className="grid grid-cols-2 gap-x-3 gap-y-1 text-xs">
        <dt className="text-muted">{stateLabel}</dt>
        <dd className="text-right text-muted">{t(seen.key as Key, { n: seen.n })}</dd>
        {device.activeUser && (
          <>
            <dt className="text-muted">{t("card.user")}</dt>
            <dd className="truncate text-right">{device.activeUser}</dd>
          </>
        )}
        {device.batteryPercent !== null && (
          <>
            <dt className="text-muted">{t("card.battery")}</dt>
            <dd className="text-right">{device.batteryPercent}%</dd>
          </>
        )}
      </dl>

      <div className="mt-auto flex flex-wrap items-center gap-2">
        {up && (
          <>
            <Button kind="primary" onClick={() => onAction(device, "screen")}>
              🖥 {t("verb.screen")}
            </Button>
            <Button onClick={() => onAction(device, "terminal")}>⌨ {t("verb.terminal")}</Button>
          </>
        )}
        {isAsleep(device) && (
          <Button kind="primary" disabled={waking} onClick={() => onAction(device, "screen")}>
            {waking ? t("card.waking", { secs: Math.floor((now - (wakingSince ?? now)) / 1000) }) : `⏻ ${t("card.wake_and_connect")}`}
          </Button>
        )}
        {canWake(device) && !isAsleep(device) && (
          <Button disabled={waking} onClick={() => onAction(device, "wake")}>
            ⏻ {t("verb.wake")}
          </Button>
        )}
        {up && (
          <div className="relative ml-auto">
            <Button kind="ghost" title={t("card.more")} onClick={() => setMore((m) => !m)}>
              <span aria-label={t("card.more")}>⋯</span>
            </Button>
            {more && (
              <ul
                role="menu"
                className="absolute right-0 z-10 mt-1 w-40 rounded-lg border border-line bg-panel p-1 shadow-[var(--shadow)]"
                onKeyDown={(e) => e.key === "Escape" && setMore(false)}
              >
                {verbs.map((v) => (
                  <li key={v} role="none">
                    <button
                      role="menuitem"
                      type="button"
                      className={`w-full rounded px-2 py-1.5 text-left text-sm hover:bg-panel2 ${DESTRUCTIVE.has(v) ? "text-danger" : ""}`}
                      onClick={() => {
                        setMore(false);
                        onAction(device, v);
                      }}
                    >
                      {t(`verb.${v}` as Key)}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>
    </article>
  );
}
