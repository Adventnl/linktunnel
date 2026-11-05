import { Icon } from "./Icon";
import { type Verb } from "../lib/palette";
import { canConnect, canWake, isAsleep, seenParts } from "../lib/status";
import type { Key } from "../lib/i18n";
import type { Device, PhoneOptions } from "../lib/types";
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
  phoneOptions,
  onPhoneOptions,
  onPhoneSetup,
}: {
  device: Device;
  now: number;
  /** Set while "wake and connect" is in progress. */
  wakingSince: number | null;
  connecting: boolean;
  connectionError?: string;
  t: T;
  onAction: (device: Device, verb: Verb | "files" | "settings") => void;
  phoneOptions?: Omit<PhoneOptions, "serial">;
  onPhoneOptions?: (options: Omit<PhoneOptions, "serial">) => void;
  onPhoneSetup?: () => void;
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
          name={device.phoneSerial ? "phone" : device.batteryPercent !== null ? "laptop" : "monitor"}
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
        {device.phoneSerial ? t(device.phoneState === "unauthorized" ? "phone.authorize" : up ? "phone.description" : "phone.offline") : up ? t("workspace.description") : t("workspace.offline")}
      </p>
      {device.phoneSerial && phoneOptions && onPhoneOptions && (
        <fieldset className="phone-options" disabled={connecting}>
          <label><input type="checkbox" checked={phoneOptions.screenOff} onChange={e => onPhoneOptions({ ...phoneOptions, screenOff: e.target.checked })} />{t("phone.screen_off")}</label>
          <label><input type="checkbox" checked={phoneOptions.mediaAudio} onChange={e => onPhoneOptions({ ...phoneOptions, mediaAudio: e.target.checked })} />{t("phone.media_audio")}</label>
          <label>{t("phone.quality")}<select value={phoneOptions.maxSize} onChange={e => onPhoneOptions({ ...phoneOptions, maxSize: Number(e.target.value) })}>
            <option value={1080}>1080 · 60 fps</option><option value={1600}>1600 · 60 fps</option><option value={2560}>2560 · 60 fps</option>
          </select></label>
        </fieldset>
      )}
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
          <p className="text-danger">{t(device.phoneSerial ? "phone.connect_error" : "card.connect_error")}</p>
          {device.phoneSerial && <p className="mt-1 break-words whitespace-pre-wrap">{connectionError}</p>}
          {!device.phoneSerial &&
          <details className="mt-1 text-xs text-muted">
            <summary className="cursor-pointer">
              {t("card.connect_details")}
            </summary>
            <p className="mt-1 break-words">{connectionError}</p>
          </details>}
        </div>
      )}
      {!device.phoneSerial && canWake(device) && (
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
            {!device.phoneSerial && <Button onClick={() => onAction(device, "terminal")}>
              <Icon name="terminal" />
              {t("verb.terminal")}
            </Button>}
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
        {device.phoneSerial && <Button onClick={onPhoneSetup}>{t("phone.connection_settings")}</Button>}
        {!device.phoneSerial && canWake(device) && !isAsleep(device) && (
          <Button disabled={waking} onClick={() => onAction(device, "wake")}>
            <Icon name="power" />
            {t("verb.wake")}
          </Button>
        )}
      </div>
    </article>
  );
}
