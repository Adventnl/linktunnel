import { useCallback, useEffect, useState } from "react";
import type { Bridge, Device, PhoneDevice, PhoneSession } from "../lib/types";

type KnownPhone = PhoneDevice & { lastSeenMs: number };
const storageKey = "nexus.phones";

function savedPhones(): KnownPhone[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(storageKey) ?? "[]");
    if (!Array.isArray(value)) return [];
    return value.filter((p): p is KnownPhone => p && typeof p.serial === "string" &&
      typeof p.model === "string" && typeof p.lastSeenMs === "number")
      .map(p => ({ ...p, state: "offline" }));
  } catch { return []; }
}

/** Android discovery stays available independently of the desktop agent. */
export function usePhones(bridge: Bridge) {
  const [known, setKnown] = useState<KnownPhone[]>(savedPhones);
  const [session, setSession] = useState<PhoneSession>({ serial: null, error: null });
  const [discoveryError, setDiscoveryError] = useState("");
  const refreshSession = useCallback(async () => {
    const next = await bridge.phoneSession();
    setSession(next);
    return next;
  }, [bridge]);
  useEffect(() => {
    let disposed = false;
    let polling = false;
    const refresh = async () => {
      if (polling) return;
      polling = true;
      // Device enumeration may take longer than the session query. Never block
      // session lifecycle updates on a slow adb command (or the reverse).
      await Promise.allSettled([
        bridge.phoneDevices().then(list => {
          if (disposed) return;
          setDiscoveryError("");
          setKnown(previous => {
            const current = new Map(list.map(phone => [phone.serial, phone]));
            const phones = previous.map(phone => current.has(phone.serial)
              ? { ...current.get(phone.serial)!, lastSeenMs: Date.now() }
              : { ...phone, state: "offline" });
            for (const phone of list) {
              if (!previous.some(p => p.serial === phone.serial)) phones.push({ ...phone, lastSeenMs: Date.now() });
            }
            return phones;
          });
        }).catch(error => {
          if (!disposed) {
            setDiscoveryError(String(error));
            setKnown(previous => previous.map(phone => ({ ...phone, state: "offline" })));
          }
        }),
        bridge.phoneSession().then(next => { if (!disposed) setSession(next); }),
      ]);
      polling = false;
    };
    void refresh();
    const timer = setInterval(() => void refresh(), 2500);
    return () => { disposed = true; clearInterval(timer); };
  }, [bridge]);
  useEffect(() => {
    try { localStorage.setItem(storageKey, JSON.stringify(known)); } catch { /* Optional offline history. */ }
  }, [known]);
  const devices: Device[] = known.map(phone => ({
    id: `phone:${phone.serial}`, phoneSerial: phone.serial, phoneState: phone.state,
    name: phone.model, osFamily: "android", osVersion: "Android",
    state: phone.state === "device" ? "online" : phone.state === "offline" ? "offline" : "unknown",
    online: phone.state === "device", lastSeenMs: phone.lastSeenMs, path: "lan",
    activeUser: "", batteryPercent: null, fingerprint: "", unattended: false,
    grantsOnPeer: ["screen", "input"],
  }));
  return { devices, session, discoveryError, refreshSession };
}
