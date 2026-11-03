import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";
import { b64ToBytes, textToB64 } from "../lib/b64";
import type { Key } from "../lib/i18n";
import type { Bridge, Device } from "../lib/types";
import { Button } from "./Modal";
import { Icon } from "./Icon";

type T = (key: Key, args?: Record<string, string | number>) => string;

/** A terminal on a paired device, rendered with xterm.js over the agent's encrypted session. */
export function TerminalPanel({
  bridge,
  device,
  t,
  onClose,
}: {
  bridge: Bridge;
  device: Device;
  t: T;
  onClose: () => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<"connecting" | "open" | "ended">(
    "connecting",
  );

  useEffect(() => {
    // Delay allocation until the committed frame. StrictMode can cancel its setup replay
    // before xterm schedules viewport work against a terminal that is already disposed.
    let cleanup: (() => void) | undefined;
    const start = () => {
      const el = host.current;
      if (!el) return;
      let disposed = false;
      const readTheme = () => {
        const css = getComputedStyle(document.documentElement);
        return {
          background: css.getPropertyValue("--panel-2").trim() || "#1f242d",
          foreground: css.getPropertyValue("--text").trim() || "#e8ebf1",
          cursor: css.getPropertyValue("--accent").trim() || "#5b8cff",
          black: css.getPropertyValue("--terminal-black").trim(),
          red: css.getPropertyValue("--terminal-red").trim(),
          green: css.getPropertyValue("--terminal-green").trim(),
          yellow: css.getPropertyValue("--terminal-yellow").trim(),
          blue: css.getPropertyValue("--terminal-blue").trim(),
          magenta: css.getPropertyValue("--terminal-magenta").trim(),
          cyan: css.getPropertyValue("--terminal-cyan").trim(),
          white: css.getPropertyValue("--text").trim(),
          brightBlack: css.getPropertyValue("--muted").trim(),
          brightRed: css.getPropertyValue("--terminal-red").trim(),
          brightGreen: css.getPropertyValue("--terminal-green").trim(),
          brightYellow: css.getPropertyValue("--terminal-yellow").trim(),
          brightBlue: css.getPropertyValue("--terminal-blue").trim(),
          brightMagenta: css.getPropertyValue("--terminal-magenta").trim(),
          brightCyan: css.getPropertyValue("--terminal-cyan").trim(),
          brightWhite: css.getPropertyValue("--text").trim(),
        };
      };
      const css = getComputedStyle(document.documentElement);
      const term = new Terminal({
        fontFamily: css.getPropertyValue("--font-mono") || "monospace",
        fontSize: 13,
        cursorBlink: true,
        allowProposedApi: false,
        scrollback: 10000,
        minimumContrastRatio: 4.5,
        theme: readTheme(),
      });
      const refreshTheme = () => {
        if (!disposed) term.options.theme = readTheme();
      };
      const themeObserver = new MutationObserver(refreshTheme);
      themeObserver.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });
      const appearance = matchMedia("(prefers-color-scheme: dark)");
      appearance.addEventListener("change", refreshTheme);
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(el);
      if (el.clientWidth > 0 && el.clientHeight > 0) {
        fit.fit();
        term.focus();
      }

      let terminalId: number | null = null;
      let unlisten: (() => void) | null = null;
      const pending: string[] = [];

      void (async () => {
        unlisten = await bridge.onEvent((e) => {
          if (
            terminalId === null ||
            !("terminalId" in e) ||
            e.terminalId !== terminalId
          )
            return;
          if (e.kind === "terminal_ready") setStatus("open");
          else if (e.kind === "terminal_output")
            term.write(b64ToBytes(e.dataB64));
          else if (e.kind === "terminal_ended") {
            setStatus("ended");
            term.write(
              `\r\n\x1b[2m[${t("terminal.ended")}${e.reason ? `: ${e.reason}` : ""}]\x1b[0m\r\n`,
            );
          }
        });
        try {
          const { terminalId: id } = await bridge.terminalOpen(
            device.id,
            term.cols,
            term.rows,
          );
          if (disposed) {
            void bridge.terminalClose(id, false).catch(() => {});
            return;
          }
          terminalId = id;
          // Keys typed while the shell was starting.
          for (const data of pending.splice(0))
            void bridge.terminalInput(id, data).catch(() => {});
        } catch (err) {
          setStatus("ended");
          term.write(
            `\r\n\x1b[31m${err instanceof Error ? err.message : String(err)}\x1b[0m\r\n`,
          );
        }
      })();

      const data = term.onData((d) => {
        const b64 = textToB64(d);
        if (terminalId === null) pending.push(b64);
        else void bridge.terminalInput(terminalId, b64).catch(() => {});
      });
      const resize = () => {
        if (disposed || el.clientWidth === 0 || el.clientHeight === 0) return;
        fit.fit();
        if (terminalId !== null)
          void bridge
            .terminalResize(terminalId, term.cols, term.rows)
            .catch(() => {});
      };
      const ro = new ResizeObserver(resize);
      ro.observe(el);

      return () => {
        disposed = true;
        ro.disconnect();
        themeObserver.disconnect();
        appearance.removeEventListener("change", refreshTheme);
        data.dispose();
        unlisten?.();
        // Closing the panel detaches; the shell is not killed (it may be a named, persistent one).
        if (terminalId !== null)
          void bridge.terminalClose(terminalId, false).catch(() => {});
        term.dispose();
      };
    };
    const frame = requestAnimationFrame(() => {
      cleanup = start();
    });
    return () => {
      cancelAnimationFrame(frame);
      cleanup?.();
    };
  }, [bridge, device.id, t]);

  return (
    <section
      aria-label={t("terminal.open", { name: device.name })}
      className="flex h-full w-full min-h-0 min-w-0 flex-1 flex-col bg-panel"
    >
      <header className="flex items-center justify-between gap-3 border-b border-line px-4 py-2">
        <h2 className="flex min-w-0 items-center gap-2 truncate text-sm font-semibold">
          <Icon name="terminal" size={16} />
          {t("terminal.open", { name: device.name })}
          {status === "connecting" && (
            <span className="ml-2 text-xs font-normal text-muted pulse">
              {t("terminal.connecting")}
            </span>
          )}
        </h2>
        <Button kind="ghost" onClick={onClose}>
          {t("terminal.close")}
        </Button>
      </header>
      <div ref={host} className="min-h-0 flex-1 bg-panel2 p-2" />
    </section>
  );
}
