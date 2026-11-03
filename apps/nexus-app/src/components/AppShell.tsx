import { useCallback, useState } from "react";
import type { AppModel } from "../hooks/useApp";
import type { Device } from "../lib/types";
import { DeviceMenu } from "./DeviceMenu";
import { DesktopWorkspace } from "./DesktopWorkspace";
import { Icon } from "./Icon";
import { DeviceSidebar } from "./DeviceSidebar";
import { Titlebar } from "./Titlebar";

export function AppShell({ model }: { model: AppModel }) {
  const { t, selected, status, windowAction } = model;
  const [collapsed, collapse] = useState(false);
  const [menu, setMenu] = useState<{
    device: Device;
    position: { x: number; y: number };
  } | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);
  return (
    <div className={`app-shell ${collapsed ? "sidebar-collapsed" : ""}`}>
      <Titlebar model={model} onWindowAction={windowAction} />
      <DeviceSidebar model={model} onMenu={setMenu} />
      <main className="workspace-main">
        <div className="workspace-heading">
          <button
            type="button"
            className="icon-button"
            aria-expanded={!collapsed}
            title={t("workspace.sidebar")}
            onClick={() => collapse(!collapsed)}
          >
            <Icon name="sidebar" />
          </button>
          <span>{selected?.name ?? t("workspace.choose")}</span>
          <div className="heading-end">
            {selected && (
              <button
                type="button"
                className="icon-button"
                title={t("card.more")}
                aria-haspopup="menu"
                onClick={(event) => {
                  const rect = event.currentTarget.getBoundingClientRect();
                  setMenu({
                    device: selected,
                    position: { x: rect.x, y: rect.bottom },
                  });
                }}
              >
                <Icon name="more" />
              </button>
            )}
            <button
              type="button"
              className="icon-button"
              title={t("workspace.fullscreen")}
              onClick={() => windowAction("fullscreen")}
            >
              <Icon name="expand" size={17} />
            </button>
          </div>
        </div>
        {status && !status.signalConnected && (
          <div role="status" className="workspace-notice">
            {t(status.signalUrl ? "pair.offline" : "pair.unconfigured")}
          </div>
        )}
        {model.state.snapshot.sessions.length > 0 && (
          <div className="incoming-banner" role="alert">
            <Icon name="shield" />
            <span>
              {t("connected.banner", {
                names: model.state.snapshot.sessions
                  .map((session) => session.peerName)
                  .join(", "),
              })}
            </span>
            <button type="button" onClick={model.disconnectIncoming}>
              {t("connected.disconnect")}
            </button>
            <button type="button" onClick={model.panic}>
              {t("connected.panic")}
            </button>
          </div>
        )}
        {model.state.snapshot.agent === "unreachable" ? (
          <div className="workspace-empty" role="status">
            <Icon name="link" size={40} />
            <h1>{t("agent.unreachable")}</h1>
            <p>{t("agent.unreachable.help")}</p>
            <p className="text-danger">{model.state.snapshot.error}</p>
          </div>
        ) : (
          <DesktopWorkspace model={model} overlayOpen={!!menu} />
        )}
      </main>
      {menu && (
        <DeviceMenu
          {...menu}
          t={t}
          onClose={closeMenu}
          onAction={model.run}
          onUnpair={model.setUnpairing}
        />
      )}
      <ul className="toast-list" aria-live="assertive">
        {model.state.toasts.map((toast) => (
          <li
            key={toast.id}
            className={toast.level === "error" ? "toast-error" : ""}
          >
            <button
              type="button"
              onClick={() => model.dispatch({ kind: "dismiss", id: toast.id })}
            >
              {toast.text}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
