import { lazy, Suspense, useEffect, useRef, useState } from "react";
import type { AppModel } from "../hooks/useApp";
import type { Quality, ScaleMode } from "../lib/types";
import { DeviceCard } from "./DeviceCard";
import { Icon } from "./Icon";
import { Button } from "./Modal";
import { useViewerViewport } from "../hooks/useViewerViewport";

const TerminalPanel = lazy(() =>
  import("./TerminalPanel").then((m) => ({ default: m.TerminalPanel })),
);

export function DesktopWorkspace({
  model,
  overlayOpen = false,
}: {
  model: AppModel;
  overlayOpen?: boolean;
}) {
  const { selected: device, screen, terminal, bridge, t } = model;
  const [scale, setScale] = useState<ScaleMode>("fit");
  const [tab, setTab] = useState<"screen" | "terminal">("screen");
  const viewport = useRef<HTMLDivElement>(null);
  const active = screen?.id === device?.id;
  const showTerminal = terminal?.id === device?.id && tab === "terminal";
  useEffect(() => {
    if (terminal) setTab("terminal");
  }, [terminal]);
  useEffect(() => {
    if (screen) setTab("screen");
  }, [screen]);
  useViewerViewport(model, viewport, {
    deviceId: device?.id,
    active,
    showTerminal,
    scale,
    overlayOpen,
  });

  if (!device)
    return (
      <div className="workspace-empty">
        <Icon name="link" size={48} />
        <h1>{t("devices.empty.title")}</h1>
        <p>{t("devices.empty.body")}</p>
        <Button kind="primary" onClick={() => model.setPairing(true)}>
          {t("devices.pair")}
        </Button>
      </div>
    );
  return (
    <div className="desktop-workspace">
      <div className="workspace-toolbar">
        <div className="workspace-tabs" role="tablist" aria-label={device.name}>
          <button
            type="button"
            role="tab"
            aria-selected={!showTerminal}
            onClick={() => setTab("screen")}
          >
            <Icon name="monitor" size={16} />
            {t("workspace.screen")}
          </button>
          {terminal?.id === device.id && (
            <button
              type="button"
              role="tab"
              aria-selected={showTerminal}
              onClick={() => setTab("terminal")}
            >
              <Icon name="terminal" size={16} />
              {t("verb.terminal")}
            </button>
          )}
        </div>
        <div className="viewer-controls">
          <select
            aria-label={t("workspace.scale")}
            value={scale}
            onChange={(event) => setScale(event.target.value as ScaleMode)}
          >
            <option value="fit">{t("workspace.fit")}</option>
            <option value="fill">{t("workspace.fill")}</option>
            <option value="pixel">{t("workspace.pixel")}</option>
          </select>
          <select
            title={t("workspace.quality_hint")}
            aria-label={t("workspace.quality")}
            value={model.quality}
            onChange={(event) =>
              model.setQuality(event.target.value as Quality)
            }
          >
            <option value="auto">{t("workspace.auto")}</option>
            <option value="sharp">{t("workspace.sharp")}</option>
            <option value="smooth">{t("workspace.smooth")}</option>
          </select>
          {screen && (
            <button
              className="disconnect-button"
              type="button"
              onClick={() => void model.closeScreen()}
            >
              <Icon name="power" size={15} />
              {t("workspace.disconnect")}
            </button>
          )}
        </div>
      </div>
      <div
        ref={viewport}
        className={`viewport ${active && !showTerminal ? "viewport-active" : ""}`}
      >
        {terminal && (
          <div
            className="h-full min-h-0 min-w-0 flex-1"
            style={{ display: showTerminal ? "flex" : "none" }}
          >
            <Suspense
              fallback={<p className="p-6">{t("terminal.connecting")}</p>}
            >
              <TerminalPanel
                key={terminal.id}
                bridge={bridge}
                device={terminal}
                t={t}
                onClose={() => {
                  model.setTerminal(null);
                  setTab("screen");
                }}
              />
            </Suspense>
          </div>
        )}
        {!showTerminal &&
          (active ? (
            <div className="viewer-waiting" role="status">
              <span className="connection-spinner" />
              <p>{t("workspace.connecting")}</p>
            </div>
          ) : (
            <DeviceCard
              key={device.id}
              device={device}
              now={model.now}
              wakingSince={model.waking[device.id] ?? null}
              connecting={model.connecting[device.id] ?? false}
              connectionError={model.connectionErrors[device.id]}
              t={t}
              onAction={model.run}
            />
          ))}
      </div>
      <footer className="workspace-status">
        <span>
          <Icon name="shield" size={14} />
          {t("workspace.secure")}
        </span>
        <span>
          {active
            ? model.viewerStats?.id === device.id
              ? model.viewerStats.text
              : t("workspace.session")
            : t("workspace.ready")}
        </span>
      </footer>
    </div>
  );
}
