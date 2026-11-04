import type { AppModel } from "../hooks/useApp";
import { useLayoutEffect, useRef, useState } from "react";
import type { Device } from "../lib/types";
import { Icon } from "./Icon";
import { StatusDot } from "./StatusDot";
export function DeviceSidebar({
  model,
  onMenu,
}: {
  model: AppModel;
  onMenu: (menu: {
    device: Device;
    position: { x: number; y: number };
  }) => void;
}) {
  const { t, devices, selected } = model;
  const list = useRef<HTMLElement>(null);
  const [highlight, setHighlight] = useState<{
    x: number;
    y: number;
    width: number;
    height: number;
  } | null>(null);
  useLayoutEffect(() => {
    const root = list.current;
    if (!root) return;
    const update = () => {
      const row = root.querySelector<HTMLElement>(".is-selected");
      if (!row) {
        setHighlight(null);
        return;
      }
      const rect = row.getBoundingClientRect();
      const parent = root.getBoundingClientRect();
      setHighlight({
        x: rect.x - parent.x + root.scrollLeft,
        y: rect.y - parent.y + root.scrollTop,
        width: rect.width,
        height: rect.height,
      });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(root);
    return () => observer.disconnect();
  }, [selected?.id, devices.length]);
  return (
    <aside className="device-sidebar" aria-label={t("workspace.devices")}>
      <div className="sidebar-heading">
        <h2>{t("workspace.devices")}</h2>
        <button
          type="button"
          className="icon-button"
          title={t("devices.pair")}
          onClick={() => model.setPairing(true)}
        >
          <Icon name="plus" />
        </button>
      </div>
      <nav ref={list} className="device-list">
        {highlight && (
          <span
            aria-hidden="true"
            className="selection-highlight"
            style={{
              transform: `translate(${highlight.x}px, ${highlight.y}px)`,
              width: highlight.width,
              height: highlight.height,
            }}
          />
        )}
        <ul>
          {devices.map((device) => (
            <li
              key={device.id}
              className={`device-row ${selected?.id === device.id ? "is-selected" : ""}`}
              onContextMenu={(event) => {
                event.preventDefault();
                model.selectDevice(device.id);
                onMenu({
                  device,
                  position: { x: event.clientX, y: event.clientY },
                });
              }}
            >
              <button
                type="button"
                className="device-select"
                aria-current={selected?.id === device.id ? "true" : undefined}
                onClick={() => model.selectDevice(device.id)}
                onDoubleClick={() => model.run(device, "screen")}
                onKeyDown={(event) => {
                  if (
                    event.key === "ContextMenu" ||
                    (event.shiftKey && event.key === "F10")
                  ) {
                    event.preventDefault();
                    const rect = event.currentTarget.getBoundingClientRect();
                    onMenu({
                      device,
                      position: { x: rect.x + 20, y: rect.bottom },
                    });
                  }
                }}
              >
                <Icon
                  name={device.batteryPercent !== null ? "laptop" : "monitor"}
                  size={24}
                />
                <span className="device-row-copy">
                  <strong>{device.name}</strong>
                  <span>
                    <StatusDot
                      state={device.state}
                      label={t(`state.${device.state}`)}
                    />
                    {t(`state.${device.state}`)}
                  </span>
                </span>
              </button>
              <button
                className="row-menu"
                type="button"
                aria-label={`${t("workspace.menu")}: ${device.name}`}
                aria-haspopup="menu"
                onClick={(event) => {
                  const rect = event.currentTarget.getBoundingClientRect();
                  onMenu({
                    device,
                    position: { x: rect.right, y: rect.bottom },
                  });
                }}
              >
                <Icon name="more" size={18} />
              </button>
            </li>
          ))}
        </ul>
      </nav>
    </aside>
  );
}
