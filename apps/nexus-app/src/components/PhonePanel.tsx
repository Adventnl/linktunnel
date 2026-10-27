import { useCallback, useEffect, useRef, useState } from "react";
import { Button, Modal } from "./Modal";
import type { Key } from "../lib/i18n";
import type { Bridge, PhoneDevice, PhoneSession, PhoneStatus } from "../lib/types";

type Translate = (key: Key, args?: Record<string, string | number>) => string;
const field = "w-full rounded-lg border border-line bg-panel2 px-3 py-2 text-sm text-fg";

/** Local Android control works even when the desktop agent is unreachable. */
export function PhonePanel({ bridge, t, onClose }: { bridge: Bridge; t: Translate; onClose: () => void }) {
  const [status, setStatus] = useState<PhoneStatus | null>(null);
  const [devices, setDevices] = useState<PhoneDevice[]>([]);
  const [session, setSession] = useState<PhoneSession>({ serial: null, error: null });
  const [serial, setSerial] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [screenOff, setScreenOff] = useState(true);
  const [mediaAudio, setMediaAudio] = useState(false);
  const [quality, setQuality] = useState("1600");
  const [pairAddress, setPairAddress] = useState("");
  const [code, setCode] = useState("");
  const [address, setAddress] = useState("");
  const alive = useRef(true);
  const pending = useRef(false);
  const refresh = useCallback(async () => {
    const next = await bridge.phoneStatus();
    if (!alive.current) return;
    setStatus(next); setSession(next.session);
    const list = "Ok" in next.adb ? await bridge.phoneDevices() : [];
    if (!alive.current) return;
    setDevices(list);
    setSerial((old) => list.some((d) => d.serial === old) ? old : list.find((d) => d.state === "device")?.serial ?? "");
  }, [bridge]);
  const run = useCallback(async (action: () => Promise<void>) => {
    if (pending.current) return;
    pending.current = true; setBusy(true); setError(""); setMessage("");
    try { await action(); }
    catch (e) { if (alive.current) setError(e instanceof Error ? e.message : String(e)); }
    finally { pending.current = false; if (alive.current) setBusy(false); }
  }, []);
  useEffect(() => {
    alive.current = true;
    void run(refresh);
    let polling = false;
    const timer = setInterval(() => {
      if (pending.current || polling) return;
      polling = true;
      void bridge.phoneSession().then((next) => {
        if (alive.current && !pending.current) setSession(next);
      }).catch((e: unknown) => {
        if (alive.current) setError(String(e));
      }).finally(() => { polling = false; });
    }, 2000);
    return () => { alive.current = false; clearInterval(timer); };
  }, [bridge, refresh, run]);
  const ready = !!status && "Ok" in status.adb && "Ok" in status.scrcpy;
  const selected = devices.find((d) => d.serial === serial);
  const adbReady = !!status && "Ok" in status.adb;
  const tool = (name: "adb" | "scrcpy") => {
    const value = status?.[name];
    return <p className="break-words text-xs text-muted">{name}: {value ? "Ok" in value ? value.Ok : value.Err : t("phone.checking")}</p>;
  };
  return (
    <Modal title={t("phone.title")} onClose={onClose} closeLabel={t("common.close")} wide>
      <div className="max-h-[65vh] space-y-5 overflow-y-auto pr-1 text-sm">
        <p className="text-muted">{t("phone.intro")}</p>
        <details open={!ready} className="rounded-lg border border-line p-3">
          <summary className="cursor-pointer font-semibold">{t("phone.setup")}</summary>
          <div className="mt-3 space-y-2">
            <p>{t("phone.install")}</p><p>{t("phone.usb")}</p><p className="text-muted">{t("phone.redmi")}</p>
            {tool("adb")}{tool("scrcpy")}
          </div>
        </details>
        <section className="space-y-3">
          <h3 className="font-semibold">{t("phone.control")}</h3>
          <label className="block space-y-1">{t("phone.device")}
            <select className={field} value={serial} disabled={busy || !!session.serial} onChange={(e) => setSerial(e.target.value)}>
              <option value="">{t("phone.choose")}</option>
              {devices.map((d) => <option key={d.serial} value={d.serial} disabled={d.state !== "device"}>{d.model} · {d.serial} · {d.state}</option>)}
            </select>
          </label>
          {devices.length === 0 && <p className="text-muted">{t("phone.empty")}</p>}
          <fieldset disabled={busy || !!session.serial} className="flex flex-wrap gap-x-5 gap-y-2 disabled:opacity-50">
            <label className="flex items-center gap-2"><input type="checkbox" checked={screenOff} onChange={(e) => setScreenOff(e.target.checked)} />{t("phone.screen_off")}</label>
            <label className="flex items-center gap-2"><input type="checkbox" checked={mediaAudio} onChange={(e) => setMediaAudio(e.target.checked)} />{t("phone.media_audio")}</label>
            <label className="flex items-center gap-2">{t("phone.quality")}
              <select className="rounded border border-line bg-panel2 p-1" value={quality} onChange={(e) => setQuality(e.target.value)}>
                <option value="1080">1080 · 60 fps</option><option value="1600">1600 · 60 fps</option><option value="2560">2560 · 60 fps</option>
              </select>
            </label>
          </fieldset>
          <div className="flex flex-wrap gap-2">
            <Button disabled={busy} onClick={() => void run(refresh)}>{t("phone.refresh")}</Button>
            <Button kind="primary" disabled={busy || !ready || selected?.state !== "device" || !!session.serial} onClick={() => void run(async () => {
              await bridge.phoneStart({ serial, screenOff, mediaAudio, maxSize: Number(quality), maxFps: 60 });
              const next = await bridge.phoneSession(); if (alive.current) setSession(next);
            })}>{busy ? t("phone.working") : t("phone.open")}</Button>
            <Button kind="danger" disabled={busy || !session.serial} onClick={() => void run(async () => {
              await bridge.phoneStop(); const next = await bridge.phoneSession(); if (alive.current) setSession(next);
            })}>{t("phone.stop")}</Button>
          </div>
          <p role="status" className="text-muted">{session.serial ? t("phone.active", { serial: session.serial }) : t("phone.inactive")}</p>
          <p className="text-xs text-muted">{t("phone.shortcuts")}</p>
          <p className="text-xs text-muted">{t("phone.limits")}</p>
          <p className="text-xs text-muted">{t("phone.lifecycle")}</p>
        </section>
        <details className="rounded-lg border border-line p-3">
          <summary className="cursor-pointer font-semibold">{t("phone.wireless")}</summary>
          <div className="mt-3 space-y-3">
            <p className="text-muted">{t("phone.wireless_help")}</p>
            <form className="space-y-2" onSubmit={(e) => { e.preventDefault(); void run(async () => {
              const pairingCode = code; setCode(""); await bridge.phonePair(pairAddress.trim(), pairingCode);
              if (alive.current) setMessage(t("phone.paired"));
            }); }}>
              <label className="block space-y-1">{t("phone.pair_address")}<input className={field} placeholder="192.168.1.20:37123" value={pairAddress} disabled={busy} onChange={(e) => setPairAddress(e.target.value)} autoComplete="off" /></label>
              <label className="block space-y-1">{t("phone.code")}<input className={field} type="password" inputMode="numeric" pattern="[0-9]{6}" maxLength={6} value={code} disabled={busy} onChange={(e) => setCode(e.target.value)} autoComplete="off" /></label>
              <Button type="submit" disabled={busy || !adbReady || !pairAddress || !/^[0-9]{6}$/.test(code)}>{t("phone.pair")}</Button>
            </form>
            <form className="space-y-2" onSubmit={(e) => { e.preventDefault(); void run(async () => { await bridge.phoneConnect(address.trim()); await refresh(); }); }}>
              <label className="block space-y-1">{t("phone.connect_address")}<input className={field} placeholder="192.168.1.20:40221" value={address} disabled={busy} onChange={(e) => setAddress(e.target.value)} autoComplete="off" /></label>
              <div className="flex gap-2">
                <Button type="submit" disabled={busy || !adbReady || !address}>{t("phone.connect")}</Button>
                <Button disabled={busy || !adbReady || !address} onClick={() => void run(async () => { await bridge.phoneDisconnect(address.trim()); await refresh(); })}>{t("phone.disconnect")}</Button>
              </div>
            </form>
            <p className="text-xs text-muted">{t("phone.network")}</p>
          </div>
        </details>
        <section className="space-y-2 rounded-lg border border-line p-3">
          <h3 className="font-semibold">{t("phone.calls")}</h3><p>{t("phone.calls_help")}</p>
          {status?.platform === "windows" ? <>
            <p className="text-muted">{t("phone.calls_windows")}</p>
            <Button disabled={busy} onClick={() => void run(async () => { await bridge.phoneCalls(); if (alive.current) setMessage(t("phone.calls_opened")); })}>{t("phone.calls_open")}</Button>
          </> : <p className="text-muted">{t("phone.calls_mac")}</p>}
        </section>
        {(error || session.error) && <pre role="alert" className="whitespace-pre-wrap break-words rounded-lg border border-danger p-3 text-xs text-danger">{error || session.error}</pre>}
        {message && <p role="status">{message}</p>}
      </div>
    </Modal>
  );
}
