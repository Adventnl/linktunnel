import { useState } from "react";
import type { Key, Locale } from "../lib/i18n";
import type { AgentStatus, Bridge, Device } from "../lib/types";
import { Button, Modal } from "./Modal";

type T = (key: Key, args?: Record<string, string | number>) => string;
export type Theme = "auto" | "light" | "dark";

export function SettingsDialog({
  bridge,
  t,
  status,
  devices,
  theme,
  locale,
  onTheme,
  onLocale,
  onClose,
  onError,
}: {
  bridge: Bridge;
  t: T;
  status: AgentStatus | null;
  devices: Device[];
  theme: Theme;
  locale: Locale;
  onTheme: (t: Theme) => void;
  onLocale: (l: Locale) => void;
  onClose: () => void;
  onError: (text: string) => void;
}) {
  const [confirming, setConfirming] = useState<Device | null>(null);
  const fail = (e: unknown) => onError(e instanceof Error ? e.message : String(e));
  return (
    <Modal title={t("settings.title")} onClose={onClose} closeLabel={t("common.close")} wide>
      <div className="grid gap-6 md:grid-cols-2">
        <section aria-labelledby="set-this">
          <h3 id="set-this" className="mb-2 text-sm font-semibold">
            {t("settings.this_device")}
          </h3>
          {status && (
            <dl className="mb-4 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
              <dt className="text-muted">{t("settings.this_device")}</dt>
              <dd>{status.deviceName}</dd>
              <dt className="text-muted">{t("settings.fingerprint")}</dt>
              <dd className="break-all font-mono">{status.fingerprint}</dd>
              <dt className="text-muted">{t("settings.version")}</dt>
              <dd>{status.agentVersion}</dd>
            </dl>
          )}
          <label className="mb-3 block text-sm">
            <span className="mb-1 block text-muted">{t("settings.theme")}</span>
            <select className="w-full rounded-lg border border-line bg-panel2 px-2 py-1.5" value={theme} onChange={(e) => onTheme(e.target.value as Theme)}>
              <option value="auto">{t("settings.theme.auto")}</option>
              <option value="light">{t("settings.theme.light")}</option>
              <option value="dark">{t("settings.theme.dark")}</option>
            </select>
          </label>
          <label className="block text-sm">
            <span className="mb-1 block text-muted">{t("settings.language")}</span>
            <select className="w-full rounded-lg border border-line bg-panel2 px-2 py-1.5" value={locale} onChange={(e) => onLocale(e.target.value as Locale)}>
              <option value="en">English</option>
              <option value="zh-CN">简体中文</option>
            </select>
          </label>
        </section>

        <section aria-labelledby="set-devices">
          <h3 id="set-devices" className="mb-2 text-sm font-semibold">
            {t("app.title")}
          </h3>
          <ul className="space-y-3">
            {devices.map((d) => (
              <li key={d.id} className="rounded-lg border border-line p-3">
                <div className="mb-2 flex items-center justify-between gap-2">
                  <span className="truncate font-medium">{d.name}</span>
                  <Button kind="ghost" onClick={() => setConfirming(d)}>
                    {t("settings.unpair")}
                  </Button>
                </div>
                <label className="flex items-center gap-2 text-xs text-muted">
                  <input type="checkbox" checked={d.unattended} onChange={(e) => void bridge.setUnattended(d.id, e.target.checked).catch(fail)} />
                  {t("settings.preauth")}
                </label>
              </li>
            ))}
          </ul>
        </section>
      </div>
      {confirming && (
        <div role="alertdialog" aria-label={t("settings.unpair")} className="mt-4 rounded-lg border border-danger p-3">
          <p className="mb-3 text-sm">{t("settings.unpair.confirm", { name: confirming.name })}</p>
          <div className="flex justify-end gap-2">
            <Button autoFocus onClick={() => setConfirming(null)}>
              {t("confirm.cancel")}
            </Button>
            <Button
              kind="danger"
              onClick={() => {
                const id = confirming.id;
                setConfirming(null);
                void bridge.unpair(id).catch(fail);
              }}
            >
              {t("settings.unpair")}
            </Button>
          </div>
        </div>
      )}
    </Modal>
  );
}
