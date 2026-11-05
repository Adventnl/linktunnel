import type { AppModel } from "../hooks/useApp";
import type { Bridge } from "../lib/types";
import { Icon } from "./Icon";

export function Titlebar({ model, onWindowAction }: {
  model: AppModel;
  onWindowAction: (action: Parameters<Bridge["windowAction"]>[0]) => void;
}) {
  const { t, bridge, desktopInfo } = model;
  const windows = !bridge.mock && desktopInfo.platform === "windows";
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    if (!windows) return;
    const window = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const refresh = () => {
      void window.isMaximized().then(value => {
        if (!disposed) setMaximized(value);
      }).catch(() => { /* Caption actions still work if a state query fails. */ });
    };
    refresh();
    void window.onResized(refresh).then(off => {
      if (disposed) off();
      else unlisten = off;
    }).catch(() => { /* The initial state remains usable. */ });
    return () => { disposed = true; unlisten?.(); };
  }, [windows]);
  const maximizeLabel = t(maximized ? "workspace.restoreWindow" : "workspace.maximizeWindow");
  return (
    <header className={`titlebar ${windows ? "has-window-controls" : ""}`}
      onMouseDown={(event) => {
        if (event.button === 0 && !(event.target as Element).closest("button")) {
          onWindowAction(event.detail === 2 ? "maximize" : "drag");
        }
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
      {windows && <div className="window-controls" role="group" aria-label={t("workspace.windowControls")}>
        <button type="button" aria-label={t("workspace.minimize")} title={t("workspace.minimize")} onClick={() => onWindowAction("minimize")}><svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5.5h10" /></svg></button>
        <button type="button" aria-label={maximizeLabel} title={maximizeLabel} onClick={() => onWindowAction("maximize")}><svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d={maximized ? "M2.5 2.5h7v7h-7z M.5 7V.5H7" : "M.5.5h9v9h-9z"} /></svg></button>
        <button type="button" aria-label={t("common.close")} title={t("common.close")} className="window-close" onClick={() => onWindowAction("close")}><svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="m.5.5 9 9 M9.5.5l-9 9" /></svg></button>
      </div>}
      {bridge.mock && <span className="preview-label">{t("workspace.preview")}</span>}
    </header>
  );
}
import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
