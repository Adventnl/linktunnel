import type { AppModel } from "../hooks/useApp";
import type { Bridge } from "../lib/types";
import { Icon } from "./Icon";

export function Titlebar({ model, onWindowAction }: {
  model: AppModel;
  onWindowAction: (action: Parameters<Bridge["windowAction"]>[0]) => void;
}) {
  const { t, bridge, desktopInfo } = model;
  const nativeChrome = desktopInfo.platform !== "windows";
  return (
    <header className="titlebar"
      onMouseDown={(event) => {
        if (event.button === 0 && !(event.target as HTMLElement).closest("button")) onWindowAction("drag");
      }}
      onDoubleClick={(event) => {
        if (!(event.target as HTMLElement).closest("button")) onWindowAction("maximize");
      }}>
      <div className="app-identity">
        <img src="/linktunnel.png" alt="" width={28} height={28} />
        <span>Link Tunnel</span>
      </div>
      <div className="titlebar-account" title={desktopInfo.account ?? t("account.unavailable")}>
        <span className="tailscale-mark" aria-hidden="true">{Array.from({ length: 9 }, (_, i) => <i key={i} />)}</span>
        <span><small>Tailscale</small><strong>{desktopInfo.account ?? t("account.unavailable")}</strong></span>
      </div>
      <button className="settings-trigger" aria-label={t("phone.title")} type="button" onClick={() => model.setPhone(true)}>
        <Icon name="phone" size={18} /><span>{t("phone.title")}</span>
      </button>
      <button className="settings-trigger" aria-label={t("settings.title")} type="button" onClick={() => model.setSettings(true)}>
        <Icon name="settings" size={18} /><span>{t("settings.title")}</span>
      </button>
      {!bridge.mock && !nativeChrome && <div className="window-controls">
        <button type="button" title={t("workspace.minimize")} onClick={() => onWindowAction("minimize")}><Icon name="minus" size={14} /></button>
        <button type="button" title={t("workspace.maximize")} onClick={() => onWindowAction("maximize")}><Icon name="square" size={13} /></button>
        <button type="button" title={t("common.close")} className="window-close" onClick={() => onWindowAction("close")}><Icon name="close" size={16} /></button>
      </div>}
      {bridge.mock && <span className="preview-label">{t("workspace.preview")}</span>}
    </header>
  );
}
