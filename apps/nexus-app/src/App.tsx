import { Suspense, lazy, useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { CommandPalette } from "./components/CommandPalette";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { ConsentDialog } from "./components/ConsentDialog";
import { DeviceCard } from "./components/DeviceCard";
import { Button } from "./components/Modal";
import { PairingDialog } from "./components/PairingDialog";
import { SettingsDialog, type Theme } from "./components/SettingsDialog";
import { DESTRUCTIVE, type PaletteEntry, type Verb } from "./lib/palette";
import { parseLocale, translate, type Key, type Locale } from "./lib/i18n";
import { dismissToast, initialState, pushToast, reduce } from "./lib/state";
import { canConnect, isAsleep } from "./lib/status";
import type { AppEvent, Bridge, Device, PowerAction } from "./lib/types";

// xterm.js is the biggest dependency; load it only when a terminal is opened.
const TerminalPanel = lazy(() => import("./components/TerminalPanel").then((m) => ({ default: m.TerminalPanel })));

/** How long "wake and connect" waits for the machine before giving up. */
const WAKE_TIMEOUT_MS = 120_000;

function stored<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return allowed.includes(v as T) ? (v as T) : fallback;
  } catch {
    return fallback;
  }
}
function remember(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* private window or blocked storage: the choice just does not persist */
  }
}

export function App({ bridge }: { bridge: Bridge }) {
  const [state, dispatch] = useReducer(
    (s: typeof initialState, a: AppEvent | { kind: "toast"; level: "info" | "error"; text: string } | { kind: "dismiss"; id: number }) =>
      a.kind === "toast" ? pushToast(s, a.level, a.text) : a.kind === "dismiss" ? dismissToast(s, a.id) : reduce(s, a),
    initialState,
  );
  const [locale, setLocale] = useState<Locale>(() => stored<Locale>("nexus.locale", ["en", "zh-CN"], parseLocale(typeof navigator === "undefined" ? "en" : navigator.language)));
  const [theme, setTheme] = useState<Theme>(() => stored<Theme>("nexus.theme", ["auto", "light", "dark"], "auto"));
  const [now, setNow] = useState(() => Date.now());
  const [palette, setPalette] = useState(false);
  const [settings, setSettings] = useState(false);
  const [pairing, setPairing] = useState(false);
  const [confirm, setConfirm] = useState<{ device: Device; action: PowerAction } | null>(null);
  const [terminal, setTerminal] = useState<Device | null>(null);
  const [waking, setWaking] = useState<Record<string, number>>({});
  const t = useCallback((key: Key, args?: Record<string, string | number>) => translate(locale, key, args), [locale]);
  const devicesRef = useRef(state.snapshot.devices);
  devicesRef.current = state.snapshot.devices;

  const toast = useCallback((level: "info" | "error", text: string) => dispatch({ kind: "toast", level, text }), []);
  const fail = useCallback((what: string, device: Device, e: unknown) => toast("error", t("toast.failed", { action: what, name: device.name, reason: e instanceof Error ? e.message : String(e) })), [t, toast]);

  // ---- bridge wiring ------------------------------------------------------------------------
  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    void bridge.onEvent((e) => dispatch(e)).then((u) => (cancelled ? u() : (off = u)));
    void bridge
      .snapshot()
      .then((snapshot) => dispatch({ kind: "snapshot", snapshot }))
      .catch((e: unknown) => dispatch({ kind: "agent", agent: "unreachable", error: String(e) }));
    return () => {
      cancelled = true;
      off?.();
    };
  }, [bridge]);

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    document.documentElement.lang = locale;
    document.title = translate(locale, "app.title");
    if (theme === "auto") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", theme);
  }, [locale, theme]);

  // Keyboard-first: ⌘K / Ctrl+K opens the palette from anywhere.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // ---- wake and connect ---------------------------------------------------------------------
  useEffect(() => {
    for (const [id, since] of Object.entries(waking)) {
      const d = state.snapshot.devices.find((x) => x.id === id);
      if (!d) {
        setWaking(({ [id]: _drop, ...rest }) => rest);
      } else if (canConnect(d)) {
        setWaking(({ [id]: _drop, ...rest }) => rest);
        void bridge.connectScreen(id).then(() => toast("info", t("toast.viewer", { name: d.name }))).catch((e: unknown) => toast("error", t("toast.connect_failed", { name: d.name, reason: String(e) })));
      } else if (Date.now() - since > WAKE_TIMEOUT_MS) {
        setWaking(({ [id]: _drop, ...rest }) => rest);
        toast("error", t("toast.wake_failed", { name: d.name, reason: "timeout" }));
      }
    }
  }, [state.snapshot.devices, waking, bridge, t, toast, now]);

  // ---- actions ------------------------------------------------------------------------------
  const run = useCallback(
    (device: Device, verb: Verb | "files" | "settings") => {
      switch (verb) {
        case "screen":
          if (isAsleep(device)) {
            setWaking((w) => ({ ...w, [device.id]: Date.now() }));
            void bridge.wake(device.id).catch((e: unknown) => {
              setWaking(({ [device.id]: _drop, ...rest }) => rest);
              toast("error", t("toast.wake_failed", { name: device.name, reason: String(e) }));
            });
          } else {
            void bridge
              .connectScreen(device.id)
              .then(() => toast("info", t("toast.viewer", { name: device.name })))
              .catch((e: unknown) => toast("error", t("toast.connect_failed", { name: device.name, reason: e instanceof Error ? e.message : String(e) })));
          }
          break;
        case "terminal":
          setTerminal(device);
          break;
        case "wake":
          void bridge
            .wake(device.id)
            .then(() => toast("info", t("toast.wake_sent", { name: device.name })))
            .catch((e: unknown) => toast("error", t("toast.wake_failed", { name: device.name, reason: e instanceof Error ? e.message : String(e) })));
          break;
        case "lock":
        case "sleep":
        case "restart":
        case "shutdown":
          if (DESTRUCTIVE.has(verb)) setConfirm({ device, action: verb });
          else
            void bridge
              .power(device.id, verb)
              .then(() => toast("info", t("toast.done", { action: t(`verb.${verb}` as Key), name: device.name })))
              .catch((e: unknown) => fail(t(`verb.${verb}` as Key), device, e));
          break;
        default:
          break;
      }
    },
    [bridge, fail, t, toast],
  );

  const status = state.snapshot.status;
  const devices = state.snapshot.devices;
  const lockdown = status?.lockdown ?? false;
  const consent = state.consents[0];
  const palettePick = useMemo(
    () => (e: PaletteEntry) => {
      setPalette(false);
      run(e.device, e.verb);
    },
    [run],
  );

  return (
    <div className="mx-auto flex h-full max-w-6xl flex-col gap-4 p-4 sm:p-6">
      <header className="flex flex-wrap items-center gap-3">
        <h1 className="mr-auto text-xl font-semibold">{t("app.title")}</h1>
        {status && (
          <span className="flex items-center gap-1.5 rounded-full bg-panel px-3 py-1 text-xs text-muted" title={status.signalUrl}>
            <span aria-hidden className="inline-block h-2 w-2 rounded-full" style={{ background: status.signalConnected ? "var(--dot-green)" : "var(--dot-yellow)" }} />
            {status.signalConnected ? t("signal.on") : t("signal.off")}
          </span>
        )}
        <Button onClick={() => setPalette(true)} title={t("palette.hint")}>
          ⌘K <span className="sr-only">{t("palette.hint")}</span>
        </Button>
        <Button onClick={() => setPairing(true)}>＋ {t("devices.pair")}</Button>
        {status && (
          <Button
            kind={lockdown ? "danger" : "secondary"}
            title={lockdown ? t("lockdown.disable") : t("lockdown.enable")}
            onClick={() => void bridge.setLockdown(!lockdown).catch((e: unknown) => toast("error", String(e)))}
          >
            {lockdown ? `🔒 ${t("lockdown.on")}` : `🔓 ${t("lockdown.off")}`}
          </Button>
        )}
        <Button kind="ghost" title={t("settings.title")} onClick={() => setSettings(true)}>
          ⚙ <span className="sr-only">{t("settings.title")}</span>
        </Button>
      </header>

      {state.snapshot.sessions.length > 0 && (
        <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-danger bg-panel px-4 py-2 text-sm">
          <span aria-hidden className="pulse text-danger">●</span>
          <span className="mr-auto font-medium">{t("connected.banner", { names: state.snapshot.sessions.map((s) => s.peerName).join(", ") })}</span>
          <Button onClick={() => void bridge.disconnectSessions().catch((e: unknown) => toast("error", String(e)))}>{t("connected.disconnect")}</Button>
          <Button kind="danger" title={t("connected.panic.hint")} onClick={() => void bridge.panic().catch((e: unknown) => toast("error", String(e)))}>
            {t("connected.panic")}
          </Button>
        </div>
      )}

      <main className="min-h-0 flex-1 overflow-auto" aria-live="polite">
        {state.snapshot.agent === "unreachable" ? (
          <div className="mt-16 text-center">
            <h2 className="text-lg font-semibold">{t("agent.unreachable")}</h2>
            <p className="mt-2 text-sm text-muted">{t("agent.unreachable.help")}</p>
            {state.snapshot.error && <p className="mt-3 font-mono text-xs text-muted">{state.snapshot.error}</p>}
          </div>
        ) : devices.length === 0 ? (
          <div className="mt-16 text-center">
            <h2 className="text-lg font-semibold">{t("devices.empty.title")}</h2>
            <p className="mx-auto mt-2 max-w-md text-sm text-muted">{t("devices.empty.body")}</p>
            <div className="mt-4">
              <Button kind="primary" onClick={() => setPairing(true)}>
                {t("devices.pair")}
              </Button>
            </div>
          </div>
        ) : (
          <div className={`grid gap-4 ${terminal ? "lg:grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)]" : ""}`}>
            <ul className={`grid content-start gap-4 ${terminal ? "sm:grid-cols-1" : "sm:grid-cols-2 lg:grid-cols-3"}`}>
              {devices.map((d) => (
                <li key={d.id}>
                  <DeviceCard device={d} now={now} wakingSince={waking[d.id] ?? null} t={t} onAction={run} />
                </li>
              ))}
            </ul>
            {terminal && (
              <div className="min-h-[24rem] lg:sticky lg:top-0 lg:h-[calc(100vh-9rem)]">
                <Suspense fallback={<p className="p-4 text-sm text-muted pulse">{t("terminal.connecting")}</p>}>
                  <TerminalPanel key={terminal.id} bridge={bridge} device={terminal} t={t} onClose={() => setTerminal(null)} />
                </Suspense>
              </div>
            )}
          </div>
        )}
      </main>

      <ul className="pointer-events-none fixed bottom-4 right-4 z-50 flex max-w-sm flex-col gap-2" aria-live="assertive">
        {state.toasts.map((x) => (
          <li key={x.id} className={`pointer-events-auto rounded-lg border px-3 py-2 text-sm shadow-[var(--shadow)] ${x.level === "error" ? "border-danger bg-panel text-danger" : "border-line bg-panel"}`}>
            <button type="button" className="text-left" onClick={() => dispatch({ kind: "dismiss", id: x.id })}>
              {x.text}
            </button>
          </li>
        ))}
      </ul>

      {palette && <CommandPalette devices={devices} t={t} onRun={palettePick} onClose={() => setPalette(false)} />}
      {settings && (
        <SettingsDialog
          bridge={bridge}
          t={t}
          status={status}
          devices={devices}
          theme={theme}
          locale={locale}
          onTheme={(v) => {
            setTheme(v);
            remember("nexus.theme", v);
          }}
          onLocale={(v) => {
            setLocale(v);
            remember("nexus.locale", v);
          }}
          onClose={() => setSettings(false)}
          onError={(text) => toast("error", text)}
        />
      )}
      {pairing && <PairingDialog bridge={bridge} t={t} sas={state.pairingSas} result={state.pairingResult} onClose={() => setPairing(false)} />}
      {confirm && (
        <ConfirmDialog
          action={t(`verb.${confirm.action}` as Key)}
          name={confirm.device.name}
          t={t}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            const { device, action } = confirm;
            setConfirm(null);
            void bridge
              .power(device.id, action)
              .then(() => toast("info", t("toast.done", { action: t(`verb.${action}` as Key), name: device.name })))
              .catch((e: unknown) => fail(t(`verb.${action}` as Key), device, e));
          }}
        />
      )}
      {consent && (
        <ConsentDialog
          request={consent}
          t={t}
          onDecide={(allow, rememberChoice) => void bridge.consentDecide(consent.requestId, allow, rememberChoice).catch((e: unknown) => toast("error", String(e)))}
        />
      )}
    </div>
  );
}
