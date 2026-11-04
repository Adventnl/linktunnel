import { useEffect, type RefObject } from "react";
import type { AppModel } from "./useApp";
import type { ScaleMode, Viewport } from "../lib/types";

/** Coalesce geometry updates; keep the native surface out of dialogs and inactive tabs. */
export function useViewerViewport(
  model: AppModel,
  viewport: RefObject<HTMLDivElement | null>,
  {
    deviceId,
    active,
    showTerminal,
    scale,
    overlayOpen,
  }: {
    deviceId?: string;
    active: boolean;
    showTerminal: boolean;
    scale: ScaleMode;
    overlayOpen: boolean;
  },
) {
  const { bridge } = model;
  useEffect(() => {
    const element = viewport.current;
    if (!element) return;
    let previous = "";
    let cancelled = false;
    let running = false;
    let pending: Viewport | null = null;
    const flush = async () => {
      if (running) return;
      running = true;
      try {
        while (pending && !cancelled) {
          const next = pending;
          pending = null;
          await bridge.viewerLayout(next);
        }
      } catch (error) {
        if (!cancelled) model.toast("error", String(error));
      } finally {
        running = false;
      }
    };
    const update = () => {
      const rect = element.getBoundingClientRect();
      const ratio = window.devicePixelRatio || 1;
      const bounds: Viewport = {
        x: Math.round(rect.x * ratio),
        y: Math.round(rect.y * ratio),
        width: Math.max(1, Math.round(rect.width * ratio)),
        height: Math.max(1, Math.round(rect.height * ratio)),
        visible:
          !!active &&
          !showTerminal &&
          !model.modalOpen &&
          !overlayOpen &&
          !document.hidden,
        scale,
        detached: model.detached,
      };
      model.viewport.current = bounds;
      const key = JSON.stringify(bounds);
      if (previous === key) return;
      previous = key;
      pending = bounds;
      void flush();
    };
    const observer = new ResizeObserver(update);
    observer.observe(element);
    // Aspect fitting can keep the picture's size unchanged while its parent moves it.
    // Observe the stage too so collapsing the sidebar updates the native x/y bounds.
    if (element.parentElement) observer.observe(element.parentElement);
    window.addEventListener("resize", update);
    document.addEventListener("visibilitychange", update);
    update();
    return () => {
      cancelled = true;
      observer.disconnect();
      window.removeEventListener("resize", update);
      document.removeEventListener("visibilitychange", update);
    };
  }, [
    bridge,
    model.detached,
    model.remoteSize,
    deviceId,
    active,
    showTerminal,
    scale,
    model.modalOpen,
    overlayOpen,
    model.viewport,
    model.toast,
  ]);
}
