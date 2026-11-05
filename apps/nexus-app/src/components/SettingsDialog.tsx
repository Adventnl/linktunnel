import type { Key, Locale } from "../lib/i18n";
import type { AgentStatus, Bridge, Device, Quality } from "../lib/types";
import { Button, Modal } from "./Modal";
import { Icon } from "./Icon";

type T = (key: Key, args?: Record<string, string | number>) => string;
export type Theme = "auto" | "light" | "dark";
export type Material = "auto" | "glass" | "solid";

export function SettingsDialog({ bridge, t, status, devices, theme, locale, onTheme, onLocale,
  onClose, onError, onUnpair, onLockdown, quality, onQuality, material, onMaterial,
}: {
  bridge: Bridge; t: T; status: AgentStatus | null; devices: Device[];
  theme: Theme; locale: Locale; onTheme: (t: Theme) => void; onLocale: (l: Locale) => void;
  onClose: () => void; onError: (text: string) => void; onUnpair: (device: Device) => void;
  onLockdown: () => void; quality: Quality; onQuality: (quality: Quality) => void;
  material: Material; onMaterial: (material: Material) => void;
}) {
  const fail = (e: unknown) => onError(e instanceof Error ? e.message : String(e));
  return <Modal title={t("settings.title")} onClose={onClose} closeLabel={t("common.close")} wide>
    <div className="settings-content">
      <section className="settings-section" aria-labelledby="appearance-heading">
        <h3 id="appearance-heading">{t("settings.appearance")}</h3>
        <div className="appearance-options" role="group" aria-label={t("settings.theme")}>
          {(["light", "dark", "auto"] as const).map(value => <button key={value} type="button"
            className={`appearance-option appearance-${value}`} aria-pressed={theme === value} onClick={() => onTheme(value)}>
            <span className="appearance-sample" aria-hidden="true"><span /><span /></span>
            <span>{t(`settings.theme.${value}`)}{theme === value && <Icon name="check" size={14} />}</span>
          </button>)}
        </div>
        <div className="settings-row">
          <div className="settings-row-copy"><label htmlFor="material">{t("settings.material")}</label><p id="material-help">{t("settings.material.help")}</p></div>
          <select id="material" value={material} aria-describedby="material-help" onChange={e => onMaterial(e.target.value as Material)}>
            <option value="auto">{t("settings.material.auto")}</option>
            <option value="glass">{t("settings.material.glass")}</option>
            <option value="solid">{t("settings.material.solid")}</option>
          </select>
        </div>
        <label className="settings-row"><span>{t("settings.language")}</span>
          <select value={locale} onChange={e => onLocale(e.target.value as Locale)}><option value="en">English</option><option value="zh-CN">简体中文</option></select>
        </label>
      </section>
      <section className="settings-section" aria-labelledby="security-heading">
        <h3 id="security-heading">{t("settings.security")}</h3>
        <div className="settings-row">
          <div className="settings-row-copy"><label htmlFor="lockdown-switch">{t("settings.lockdown")}</label><p>{t("settings.lockdown.help")}</p></div>
          <button id="lockdown-switch" className="glass-switch" type="button" role="switch" aria-checked={status?.lockdown ?? false}
            aria-label={t("settings.lockdown")} disabled={!status} onClick={onLockdown}><span /></button>
        </div>
      </section>
      <section className="settings-section" aria-labelledby="sessions-heading">
        <h3 id="sessions-heading">{t("settings.session")}</h3>
        <label className="settings-row"><span>{t("workspace.quality")}</span>
          <select value={quality} title={t("workspace.quality_hint")} onChange={e => onQuality(e.target.value as Quality)}>
            <option value="auto">{t("workspace.auto")}</option><option value="sharp">{t("workspace.sharp")}</option><option value="smooth">{t("workspace.smooth")}</option>
          </select>
        </label>
      </section>
      <details className="settings-section settings-devices">
        <summary>{t("settings.devices")}</summary>
        <ul>{devices.map(d => <li key={d.id} className="settings-device">
          <div className="settings-row"><span className="settings-device-name"><Icon name="monitor" size={18} />{d.name}</span>
            <Button kind="ghost" onClick={() => onUnpair(d)}>{t("settings.unpair")}</Button></div>
          <label className="settings-preauth"><input type="checkbox" checked={d.unattended} onChange={e => void bridge.setUnattended(d.id, e.target.checked).catch(fail)} />{t("settings.preauth")}</label>
        </li>)}</ul>
      </details>
      {status && <details className="settings-details"><summary>{t("settings.details")}</summary><dl>
        <dt>{t("settings.this_device")}</dt><dd>{status.deviceName}</dd>
        <dt>{t("settings.version")}</dt><dd>{status.agentVersion}</dd>
        <dt>{t("settings.fingerprint")}</dt><dd className="font-mono">{status.fingerprint}</dd>
      </dl></details>}
    </div>
  </Modal>;
}
