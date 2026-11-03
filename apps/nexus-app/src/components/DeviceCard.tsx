import { Icon } from "./Icon";
import { type Verb } from "../lib/palette";
import { canConnect, canWake, isAsleep, seenParts } from "../lib/status";
import type { Key } from "../lib/i18n";
import type { Device } from "../lib/types";
import { Button } from "./Modal";
import { StatusDot } from "./StatusDot";

type T = (key: Key, args?: Record<string, string | number>) => string;

export function DeviceCard({
  device,
  now,
  wakingSince,
  connecting,
  connectionError,
  t,
  onAction,
}: {
  device: Device;
  now: number;
  /** Set while "wake and connect" is in progress. */
  wakingSince: number | null;
  connecting: boolean;
  connectionError?: string;
  t: T;
  onAction: (device: Device, verb: Verb | "files" | "settings") => void;
}) {
  const up = canConnect(device);
  const waking = wakingSince !== null;
  const seen = seenParts(device.lastSeenMs, now);
  const stateLabel = t(`state.${device.state}` as Key);
  return (
    <article
      aria-label={`${device.name}, ${stateLabel}`}
      className="device-overview"
    >
      <div className="computer-portrait">
        <Icon
          name={device.batteryPercent !== null ? "laptop" : "monitor"}
          size={88}
        />
        <span className="portrait-status">
          <StatusDot
            state={device.state}
            label={stateLabel}
            pulse={waking || connecting}
          />
        </span>
      </div>
      <h1>{device.name}</h1>
      <p className="overview-subtitle">
        {device.osVersion || t("state.unknown")} {stateLabel}
      </p>
      <p className="overview-description">
        {up ? t("workspace.description") : t("workspace.offline")}
      </p>
      <dl className="device-facts">
        <dt className="text-muted">{stateLabel}</dt>
        <dd className="text-right text-muted">
          {t(seen.key as Key, { n: seen.n })}
        </dd>
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

      {connectionError && (
        <div role="alert" className="text-sm">
          <p className="text-danger">{t("card.connect_error")}</p>
          <details className="mt-1 text-xs text-muted">
            <summary className="cursor-pointer">
              {t("card.connect_details")}
            </summary>
            <p className="mt-1 break-words">{connectionError}</p>
          </details>
        </div>
      )}
      {canWake(device) && (
        <p className="text-xs text-muted">{t("card.wake_help")}</p>
      )}

      <div className="overview-actions" aria-busy={connecting || waking}>
        {up && (
          <>
            <Button
              kind="primary"
              disabled={connecting}
              onClick={() => onAction(device, "screen")}
            >
              {connecting ? t("card.connecting") : t("verb.screen")}
            </Button>
            <Button onClick={() => onAction(device, "terminal")}>
              <Icon name="terminal" />
              {t("verb.terminal")}
            </Button>
          </>
        )}
        {isAsleep(device) && (
          <Button
            kind="primary"
            disabled={waking || connecting}
            onClick={() => onAction(device, "screen")}
          >
            {connecting
              ? t("card.connecting")
              : waking
                ? t("card.waking", {
                    secs: Math.floor((now - (wakingSince ?? now)) / 1000),
                  })
                : t("card.wake_and_connect")}
          </Button>
        )}
        {canWake(device) && !isAsleep(device) && (
          <Button disabled={waking} onClick={() => onAction(device, "wake")}>
            <Icon name="power" />
            {t("verb.wake")}
          </Button>
        )}
      </div>
    </article>
  );
}
