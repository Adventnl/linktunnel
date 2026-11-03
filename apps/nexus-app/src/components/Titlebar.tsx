import type { AppModel } from "../hooks/useApp";
import type { Bridge } from "../lib/types";
import { Icon } from "./Icon";
export function Titlebar({
  model,
  onWindowAction,
}: {
  model: AppModel;
  onWindowAction: (action: Parameters<Bridge["windowAction"]>[0]) => void;
}) {
  const { t, bridge, selected } = model;
  return (
    <header
      className="titlebar"
      onMouseDown={(event) => {
        if (
          event.button === 0 &&
          !(event.target as HTMLElement).closest("button")
        )
          onWindowAction("drag");
      }}
      onDoubleClick={(event) => {
        if (!(event.target as HTMLElement).closest("button"))
          onWindowAction("maximize");
      }}
    >
      <div className="app-identity">
        <img src="/linktunnel.png" alt="" width={26} height={26} />
        <span>Link Tunnel</span>
      </div>
      <div className="titlebar-center">
        <Icon name="shield" size={14} />
        <span>{selected?.name ?? t("workspace.devices")}</span>
      </div>
      <div className="mobile-utilities">
        <button
          className="icon-button"
          type="button"
          title={t("phone.title")}
          onClick={() => model.setPhone(true)}
        >
          <Icon name="phone" />
        </button>
        <button
          className="icon-button"
          type="button"
          title={t("settings.title")}
          onClick={() => model.setSettings(true)}
        >
          <Icon name="settings" />
        </button>
      </div>
      {!bridge.mock && (
        <div className="window-controls">
          <button
            type="button"
            title={t("workspace.minimize")}
            onClick={() => onWindowAction("minimize")}
          >
            <Icon name="minus" size={14} />
          </button>
          <button
            type="button"
            title={t("workspace.maximize")}
            onClick={() => onWindowAction("maximize")}
          >
            <Icon name="square" size={13} />
          </button>
          <button
            type="button"
            title={t("common.close")}
            className="window-close"
            onClick={() => onWindowAction("close")}
          >
            <Icon name="close" size={16} />
          </button>
        </div>
      )}
      {bridge.mock && (
        <span className="preview-label">{t("workspace.preview")}</span>
      )}
    </header>
  );
}
