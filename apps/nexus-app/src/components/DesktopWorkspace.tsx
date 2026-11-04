import { lazy, Suspense, useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { AppModel } from "../hooks/useApp";
import type { ScaleMode } from "../lib/types";
import { DeviceCard } from "./DeviceCard";
import { Icon } from "./Icon";
import { Button } from "./Modal";
import { useViewerViewport } from "../hooks/useViewerViewport";

const TerminalPanel = lazy(() => import("./TerminalPanel").then(m => ({ default: m.TerminalPanel })));

export function DesktopWorkspace({ model, overlayOpen = false }: { model: AppModel; overlayOpen?: boolean }) {
  const { selected: device, screen, terminal, bridge, t, detached, setDetached } = model;
  const [scale, setScale] = useState<ScaleMode>("fit");
  const [tab, setTab] = useState<"screen" | "terminal">("screen");
  const viewport = useRef<HTMLDivElement>(null);
  const active = screen?.id === device?.id;
  const showTerminal = terminal?.id === device?.id && tab === "terminal";
  useEffect(() => { if (terminal) setTab("terminal"); }, [terminal]);
  useEffect(() => { if (screen) setTab("screen"); }, [screen]);
  useViewerViewport(model, viewport, { deviceId: device?.id, active: active && !detached, showTerminal, scale, overlayOpen });

  if (!device) return <div className="workspace-empty"><Icon name="link" size={48} />
    <h1>{t("devices.empty.title")}</h1><p>{t("devices.empty.body")}</p>
    <Button kind="primary" onClick={() => model.setPairing(true)}>{t("devices.pair")}</Button></div>;
  return <div className={`desktop-workspace ${active ? "has-session" : ""}`}>
    {(active || terminal?.id === device.id) && <div className="workspace-toolbar">
      <div className="workspace-tabs" role="tablist" aria-label={device.name}>
        <button type="button" role="tab" aria-selected={!showTerminal} onClick={() => setTab("screen")}><Icon name="monitor" size={16} />{t("workspace.screen")}</button>
        {terminal?.id === device.id && <button type="button" role="tab" aria-selected={showTerminal} onClick={() => setTab("terminal")}><Icon name="terminal" size={16} />{t("verb.terminal")}</button>}
      </div>
      {active && <div className="viewer-controls">
        <select aria-label={t("workspace.scale")} value={scale} onChange={e => setScale(e.target.value as ScaleMode)}>
          <option value="fit">{t("workspace.fit")}</option><option value="fill">{t("workspace.fill")}</option><option value="pixel">{t("workspace.pixel")}</option>
        </select>
        {model.desktopInfo.embedded && <button className="icon-button" type="button" aria-label={t(detached ? "workspace.embed" : "workspace.popout")}
          title={t(detached ? "workspace.embed" : "workspace.popout")} onClick={() => setDetached(!detached)}><Icon name={detached ? "sidebar" : "popout"} /></button>}
        {!detached && <button type="button" className="icon-button" title={t("workspace.fullscreen")} onClick={() => model.windowAction("fullscreen")}><Icon name="expand" /></button>}
        <button className="disconnect-button" type="button" onClick={() => void model.closeScreen()}><Icon name="power" size={15} />{t("workspace.disconnect")}</button>
      </div>}
    </div>}
    <div className={`viewport-stage ${showTerminal ? "terminal-stage" : ""}`}>
      <div ref={viewport} className={`viewport ${active && !showTerminal && !detached ? "viewport-active" : ""}`}
        style={{ "--remote-ratio": model.remoteSize.width / model.remoteSize.height } as CSSProperties}>
        {terminal && <div className="h-full min-h-0 min-w-0 flex-1" style={{ display: showTerminal ? "flex" : "none" }}>
          <Suspense fallback={<p className="p-6">{t("terminal.connecting")}</p>}><TerminalPanel key={terminal.id} bridge={bridge} device={terminal} t={t} onClose={() => { model.setTerminal(null); setTab("screen"); }} /></Suspense>
        </div>}
        {!showTerminal && (active ? detached ? <div className="detached-session" role="status">
          <Icon name="popout" size={42} /><h1>{device.name}</h1><p>{t("workspace.detached")}</p><span>{t("workspace.detached.help")}</span>
          {model.desktopInfo.embedded && <Button onClick={() => setDetached(false)}>{t("workspace.embed")}</Button>}
        </div> : <div className="viewer-waiting" role="status"><span className="connection-spinner" /><p>{t("workspace.connecting")}</p></div> :
          <DeviceCard key={device.id} device={device} now={model.now} wakingSince={model.waking[device.id] ?? null}
            connecting={model.connecting[device.id] ?? false} connectionError={model.connectionErrors[device.id]} t={t} onAction={model.run} />)}
      </div>
    </div>
  </div>;
}
