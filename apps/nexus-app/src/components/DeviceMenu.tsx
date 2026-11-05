import { useEffect, useRef } from "react";
import { canConnect, canWake } from "../lib/status";
import type { Device, Translator } from "../lib/types";
import type { Verb } from "../lib/palette";
import { Icon, type IconName } from "./Icon";

export function DeviceMenu({
  device,
  position,
  t,
  onAction,
  onUnpair,
  onClose,
  onPhoneSetup,
}: {
  device: Device;
  position: { x: number; y: number };
  t: Translator;
  onAction: (device: Device, verb: Verb) => void;
  onUnpair: (device: Device) => void;
  onClose: () => void;
  onPhoneSetup?: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const menu = ref.current;
    if (!menu) return;
    menu.style.left = `${Math.max(8, Math.min(position.x, window.innerWidth - menu.offsetWidth - 8))}px`;
    menu.style.top = `${Math.max(8, Math.min(position.y, window.innerHeight - menu.offsetHeight - 8))}px`;
    menu.querySelector<HTMLButtonElement>("button")?.focus();
    const outside = (event: PointerEvent) => {
      if (!menu.contains(event.target as Node)) onClose();
    };
    window.addEventListener("pointerdown", outside);
    return () => {
      window.removeEventListener("pointerdown", outside);
      previous?.focus?.();
    };
  }, [position, onClose]);
  const actions: { verb: Verb; icon: IconName }[] = device.phoneSerial
    ? (canConnect(device) ? [{ verb: "screen", icon: "phone" }] : []) : [
    ...(canConnect(device)
      ? [
          { verb: "screen" as const, icon: "monitor" as const },
          { verb: "terminal" as const, icon: "terminal" as const },
          { verb: "lock" as const, icon: "lock" as const },
          { verb: "sleep" as const, icon: "moon" as const },
          { verb: "restart" as const, icon: "refresh" as const },
          { verb: "shutdown" as const, icon: "power" as const },
        ]
      : []),
    ...(canWake(device)
      ? [{ verb: "wake" as const, icon: "power" as const }]
      : []),
  ];
  return (
    <div
      ref={ref}
      role="menu"
      aria-label={device.name}
      className="device-menu"
      style={{ left: position.x, top: position.y }}
      onKeyDown={(event) => {
        const items = [
          ...(ref.current?.querySelectorAll<HTMLButtonElement>("button") ?? []),
        ];
        const index = items.indexOf(
          document.activeElement as HTMLButtonElement,
        );
        if (["Escape", "Tab"].includes(event.key)) {
          event.preventDefault();
          onClose();
        }
        if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
          event.preventDefault();
          const next =
            event.key === "Home"
              ? 0
              : event.key === "End"
                ? items.length - 1
                : (index +
                    (event.key === "ArrowDown" ? 1 : -1) +
                    items.length) %
                  items.length;
          items[next]?.focus();
        }
      }}
    >
      {actions.map(({ verb, icon }) => (
        <button
          key={verb}
          type="button"
          role="menuitem"
          onClick={() => {
            onClose();
            onAction(device, verb);
          }}
        >
          <Icon name={icon} />
          {t(`verb.${verb}`)}
        </button>
      ))}
      {device.phoneSerial && <button type="button" role="menuitem" onClick={() => { onClose(); onPhoneSetup?.(); }}><Icon name="settings" />{t("phone.connection_settings")}</button>}
      {!device.phoneSerial && <><div className="menu-separator" />
      <button
        type="button"
        role="menuitem"
        className="text-danger"
        onClick={() => {
          onClose();
          onUnpair(device);
        }}
      >
        <Icon name="trash" />
        {t("settings.unpair")}
      </button></>}
    </div>
  );
}
