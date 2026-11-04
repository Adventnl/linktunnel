import {
  useCallback,
  useEffect,
  useReducer,
  useRef,
  useState,
} from "react";
import type { Theme } from "../components/SettingsDialog";
import { DESTRUCTIVE, type Verb } from "../lib/palette";
import { parseLocale, translate, type Key, type Locale } from "../lib/i18n";
import { dismissToast, initialState, pushToast, reduce } from "../lib/state";
import { canConnect, isAsleep } from "../lib/status";
import type {
  AppEvent,
  Bridge,
  Device,
  DesktopInfo,
  PowerAction,
  Quality,
  Viewport,
} from "../lib/types";
/** How long "wake and connect" waits for the machine before giving up. */
const WAKE_TIMEOUT_MS = 120_000;

function stored<T extends string>(
  key: string,
  allowed: readonly T[],
  fallback: T,
): T {
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

export function useApp(bridge: Bridge) {
  const [state, dispatch] = useReducer(
    (
      s: typeof initialState,
      a:
        | AppEvent
        | { kind: "toast"; level: "info" | "error"; text: string }
        | { kind: "dismiss"; id: number },
    ) =>
      a.kind === "toast"
        ? pushToast(s, a.level, a.text)
        : a.kind === "dismiss"
          ? dismissToast(s, a.id)
          : reduce(s, a),
    initialState,
  );
  const [locale, setLocale] = useState<Locale>(() =>
    stored<Locale>(
      "nexus.locale",
      ["en", "zh-CN"],
      parseLocale(typeof navigator === "undefined" ? "en" : navigator.language),
    ),
  );
  const [theme, setTheme] = useState<Theme>(() =>
    stored<Theme>("nexus.theme", ["auto", "light", "dark"], "light"),
  );
  const themeTransition = useRef<ViewTransition | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [settings, setSettings] = useState(false);
  const [phone, setPhone] = useState(false);
  const [pairing, setPairing] = useState(false);
  const [unpairing, setUnpairing] = useState<Device | null>(null);
  const [selectedId, selectDevice] = useState<string | null>(null);
  const [connectRequest, setConnectRequest] = useState<string | null>(null);
  const [desktopInfo, setDesktopInfo] = useState<DesktopInfo>({
    platform: /Win/.test(navigator.userAgent) ? "windows" : "native",
    embedded: bridge.mock || /Win/.test(navigator.userAgent), account: null,
  });
  const [windowDetached, setDetached] = useState(false);
  const detached = windowDetached || !desktopInfo.embedded;
  const [remoteSize, setRemoteSize] = useState({ width: 1920, height: 1080 });
  useEffect(() => {
    let cancelled = false;
    void bridge.desktopInfo().then((info) => {
      if (!cancelled) { setDesktopInfo(info); }
    }).catch(() => { /* Account is optional; agent functionality does not depend on Tailscale. */ });
    return () => { cancelled = true; };
  }, [bridge]);
  const [screen, setScreen] = useState<Device | null>(null);
  const viewport = useRef<Viewport | null>(null);
  const [viewerStats, setViewerStats] = useState<{
    id: string;
    text: string;
  } | null>(null);
  const [quality, setQuality] = useState<Quality>("auto");
  const [confirm, setConfirm] = useState<{
    device: Device;
    action: PowerAction;
  } | null>(null);
  const [terminal, setTerminal] = useState<Device | null>(null);
  const [waking, setWaking] = useState<Record<string, number>>({});
  const t = useCallback(
    (key: Key, args?: Record<string, string | number>) =>
      translate(locale, key, args),
    [locale],
  );
  const pendingConnections = useRef(new Set<string>());
  const [connecting, setConnecting] = useState<Record<string, boolean>>({});
  const [connectionErrors, setConnectionErrors] = useState<
    Record<string, string>
  >({});

  const toast = useCallback(
    (level: "info" | "error", text: string) =>
      dispatch({ kind: "toast", level, text }),
    [],
  );
  const fail = useCallback(
    (what: string, device: Device, e: unknown) =>
      toast(
        "error",
        t("toast.failed", {
          action: what,
          name: device.name,
          reason: e instanceof Error ? e.message : String(e),
        }),
      ),
    [t, toast],
  );

  const connect = useCallback(
    async (device: Device) => {
      if (pendingConnections.current.size > 0) return;
      pendingConnections.current.add(device.id);
      setConnecting((c) => ({ ...c, [device.id]: true }));
      setConnectionErrors(({ [device.id]: _drop, ...rest }) => rest);
      try {
        if (screen) await bridge.viewerClose();
        setRemoteSize({ width: 1920, height: 1080 });
        setScreen(device);
        selectDevice(device.id);
        await bridge.connectScreen(
          device.id,
          viewport.current
            ? { viewport: { ...viewport.current, visible: true, detached }, quality }
            : undefined,
        );
      } catch (e) {
        setScreen(null);
        const reason = e instanceof Error ? e.message : String(e);
        setConnectionErrors((errors) => ({ ...errors, [device.id]: reason }));
      } finally {
        pendingConnections.current.delete(device.id);
        setConnecting(({ [device.id]: _drop, ...rest }) => rest);
      }
    },
    [bridge, quality, screen, detached],
  );

  // ---- bridge wiring ------------------------------------------------------------------------
  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    void bridge
      .onEvent((e) => {
        if (e.kind === "viewer_ended") {
          if (!pendingConnections.current.has(e.id)) {
            setScreen((current) => (current?.id === e.id ? null : current));
            if (e.reason !== "closed")
              setConnectionErrors((errors) => ({
                ...errors,
                [e.id]: e.reason,
              }));
          }
        } else if (e.kind === "viewer_geometry") {
          if (e.width > 0 && e.height > 0) setRemoteSize({ width: e.width, height: e.height });
        } else if (e.kind === "viewer_stats") {
          setViewerStats({ id: e.id, text: e.text });
        } else if (e.kind === "connect_requested") {
          selectDevice(e.id);
          setConnectRequest(e.id);
        } else dispatch(e);
      })
      .then((u) => (cancelled ? u() : (off = u)))
      .catch((e: unknown) => {
        if (!cancelled) toast("error", String(e));
      });
    void bridge
      .snapshot()
      .then((snapshot) => {
        if (!cancelled) dispatch({ kind: "snapshot", snapshot });
      })
      .catch((e: unknown) =>
        dispatch({ kind: "agent", agent: "unreachable", error: String(e) }),
      );
    return () => {
      cancelled = true;
      off?.();
    };
  }, [bridge, toast]);

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    document.documentElement.lang = locale;
    document.title = translate(locale, "app.title");
    if (theme === "auto")
      document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", theme);
  }, [locale, theme]);

  // ---- wake and connect ---------------------------------------------------------------------
  useEffect(() => {
    for (const [id, since] of Object.entries(waking)) {
      const d = state.snapshot.devices.find((x) => x.id === id);
      if (!d) {
        setWaking(({ [id]: _drop, ...rest }) => rest);
      } else if (canConnect(d)) {
        setWaking(({ [id]: _drop, ...rest }) => rest);
        void connect(d);
      } else if (Date.now() - since > WAKE_TIMEOUT_MS) {
        setWaking(({ [id]: _drop, ...rest }) => rest);
        toast(
          "error",
          t("toast.wake_failed", { name: d.name, reason: "timeout" }),
        );
      }
    }
  }, [state.snapshot.devices, waking, connect, t, toast, now]);

  // ---- actions ------------------------------------------------------------------------------
  const run = useCallback(
    (device: Device, verb: Verb | "files" | "settings") => {
      switch (verb) {
        case "screen":
          if (isAsleep(device)) {
            setWaking((w) => ({ ...w, [device.id]: Date.now() }));
            void bridge.wake(device.id).catch((e: unknown) => {
              setWaking(({ [device.id]: _drop, ...rest }) => rest);
              toast(
                "error",
                t("toast.wake_failed", {
                  name: device.name,
                  reason: String(e),
                }),
              );
            });
          } else {
            void connect(device);
          }
          break;
        case "terminal":
          selectDevice(device.id);
          setTerminal(device);
          break;
        case "wake":
          void bridge
            .wake(device.id)
            .then(() =>
              toast("info", t("toast.wake_sent", { name: device.name })),
            )
            .catch((e: unknown) =>
              toast(
                "error",
                t("toast.wake_failed", {
                  name: device.name,
                  reason: e instanceof Error ? e.message : String(e),
                }),
              ),
            );
          break;
        case "lock":
        case "sleep":
        case "restart":
        case "shutdown":
          if (DESTRUCTIVE.has(verb)) setConfirm({ device, action: verb });
          else
            void bridge
              .power(device.id, verb)
              .then(() =>
                toast(
                  "info",
                  t("toast.done", {
                    action: t(`verb.${verb}` as Key),
                    name: device.name,
                  }),
                ),
              )
              .catch((e: unknown) => fail(t(`verb.${verb}` as Key), device, e));
          break;
        default:
          break;
      }
    },
    [bridge, connect, fail, t, toast],
  );

  const status = state.snapshot.status;
  const devices = state.snapshot.devices;
  const lockdown = status?.lockdown ?? false;
  const consent = state.consents[0];


  useEffect(() => {
    if (!connectRequest) return;
    const device = devices.find((device) => device.id === connectRequest);
    if (device) {
      setConnectRequest(null);
      void connect(device);
    }
  }, [connectRequest, devices, connect]);
  const selected =
    devices.find((d) => d.id === selectedId) ?? devices[0] ?? null;
  const closeScreen = useCallback(async () => {
    try {
      await bridge.viewerClose();
      setScreen(null);
    } catch (error) {
      toast("error", String(error));
    }
  }, [bridge, toast]);
  const windowAction = useCallback(
    (action: Parameters<Bridge["windowAction"]>[0]) => {
      void bridge
        .windowAction(action)
        .catch((error: unknown) => toast("error", String(error)));
    },
    [bridge, toast],
  );
  const toggleLockdown = () =>
    void bridge
      .setLockdown(!lockdown)
      .catch((error: unknown) => toast("error", String(error)));
  const disconnectIncoming = () =>
    void bridge
      .disconnectSessions()
      .catch((error: unknown) => toast("error", String(error)));
  const panic = () =>
    void bridge
      .panic()
      .catch((error: unknown) => toast("error", String(error)));
  const modalOpen =
    phone ||
    settings ||
    pairing ||
    !!unpairing ||
    !!confirm ||
    !!consent;
  return {
    desktopInfo,
    detached,
    setDetached,
    remoteSize,
    windowAction,
    toggleLockdown,
    disconnectIncoming,
    panic,
    bridge,
    state,
    dispatch,
    locale,
    setLocale,
    theme,
    setTheme: (next: Theme) => {
      const apply = () => {
        if (next === "auto") document.documentElement.removeAttribute("data-theme");
        else document.documentElement.setAttribute("data-theme", next);
        setTheme(next);
      };
      if (document.startViewTransition && !matchMedia("(prefers-reduced-motion: reduce)").matches) {
        themeTransition.current?.skipTransition();
        document.documentElement.classList.add("theme-transitioning");
        const transition = document.startViewTransition(apply);
        themeTransition.current = transition;
        void transition.finished.finally(() => {
          if (themeTransition.current === transition) {
            themeTransition.current = null;
            document.documentElement.classList.remove("theme-transitioning");
          }
        });
      } else apply();
    },
    now,
    settings,
    setSettings,
    phone,
    setPhone,
    pairing,
    setPairing,
    unpairing,
    setUnpairing,
    confirm,
    setConfirm,
    terminal,
    setTerminal,
    waking,
    setWaking,
    t,
    toast,
    fail,
    connecting,
    connectionErrors,
    run,
    status,
    devices,
    lockdown,
    consent,
    selected,
    selectDevice,
    screen,
    closeScreen,
    viewport,
    quality,
    setQuality,
    viewerStats,
    modalOpen,
    remember,
  };
}
export type AppModel = ReturnType<typeof useApp>;
