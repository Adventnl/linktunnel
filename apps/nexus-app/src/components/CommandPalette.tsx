import { useEffect, useMemo, useRef, useState } from "react";
import { paletteEntries, type PaletteEntry, type Verb } from "../lib/palette";
import type { Key } from "../lib/i18n";
import type { Device } from "../lib/types";
import { Modal } from "./Modal";

type T = (key: Key, args?: Record<string, string | number>) => string;

/** ⌘K / Ctrl+K: every command for every device, filtered as you type. */
export function CommandPalette({ devices, t, onRun, onClose }: { devices: Device[]; t: T; onRun: (e: PaletteEntry) => void; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [sel, setSel] = useState(0);
  const list = useRef<HTMLUListElement>(null);
  const entries = useMemo(() => paletteEntries(devices, query, (v: Verb) => t(`verb.${v}` as Key)), [devices, query, t]);
  useEffect(() => setSel(0), [query]);
  useEffect(() => {
    list.current?.children[sel]?.scrollIntoView({ block: "nearest" });
  }, [sel]);
  const run = (e: PaletteEntry | undefined) => e && onRun(e);
  return (
    <Modal title={t("palette.hint")} onClose={onClose} closeLabel={t("common.close")}>
      <input
        data-autofocus
        role="combobox"
        aria-expanded="true"
        aria-controls="palette-list"
        aria-activedescendant={entries[sel] ? `pal-${sel}` : undefined}
        aria-label={t("palette.placeholder")}
        placeholder={t("palette.placeholder")}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setSel((s) => Math.min(s + 1, entries.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setSel((s) => Math.max(s - 1, 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            run(entries[sel]);
          }
        }}
        className="mb-3 w-full rounded-lg border border-line bg-panel2 px-3 py-2 text-sm text-fg placeholder:text-muted"
      />
      <ul id="palette-list" role="listbox" ref={list} className="max-h-72 overflow-auto">
        {entries.length === 0 && <li className="px-3 py-6 text-center text-sm text-muted">{t("palette.empty")}</li>}
        {entries.map((e, i) => (
          <li
            key={`${e.device.id}-${e.verb}`}
            id={`pal-${i}`}
            role="option"
            aria-selected={i === sel}
            onMouseMove={() => setSel(i)}
            onClick={() => run(e)}
            className={`cursor-pointer rounded-md px-3 py-2 text-sm ${i === sel ? "bg-accent text-accent-fg" : ""}`}
          >
            {e.label}
          </li>
        ))}
      </ul>
    </Modal>
  );
}
