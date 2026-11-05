import { useCallback, useEffect, useRef, useState } from "react";
import type { Bridge, PhoneCallAction, PhoneCallsSnapshot, Translator } from "../lib/types";
import { Button } from "./Modal";

const field = "w-full rounded-lg border border-line bg-panel2 px-3 py-2 text-sm text-fg";

/** Native Bluetooth calling stays connected when the setup panel is closed. */
export function PhoneCalls({ bridge, t, platform }: { bridge: Bridge; t: Translator; platform?: string }) {
  const [status, setStatus] = useState<PhoneCallsSnapshot | null>(null);
  const [deviceId, setDeviceId] = useState("");
  const [lineId, setLineId] = useState("");
  const [number, setNumber] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [readError, setReadError] = useState("");
  const [notice, setNotice] = useState("");
  const pending = useRef(false);
  const polling = useRef(false);
  const revision = useRef(0);
  const alive = useRef(true);

  const apply = useCallback((next: PhoneCallsSnapshot) => {
    if (!alive.current) return;
    setStatus(next);
    setDeviceId(old => next.deviceId ?? (next.devices.some(d => d.id === old) ? old : ""));
    setLineId(old => next.lines.some(line => line.id === old) ? old : next.lines[0]?.id ?? "");
    setReadError("");
    if (next.notice) setNotice(next.notice);
  }, []);
  const run = useCallback(async (operation: () => Promise<PhoneCallsSnapshot | void>) => {
    if (pending.current) return;
    pending.current = true;
    revision.current += 1;
    setError("");
    setNotice("");
    if (alive.current) setBusy(true);
    try {
      const next = await operation();
      if (next) apply(next);
    } catch (e) {
      if (alive.current) setError(e instanceof Error ? e.message : String(e));
    } finally {
      pending.current = false;
      if (alive.current) setBusy(false);
    }
  }, [apply]);

  useEffect(() => {
    alive.current = true;
    if (platform !== "windows") return () => { alive.current = false; };
    const refresh = () => {
      if (pending.current || polling.current) return;
      polling.current = true;
      const current = revision.current;
      void bridge.phoneCallsStatus().then(next => {
        if (current === revision.current) apply(next);
      }).catch(e => {
        if (alive.current && current === revision.current) setReadError(String(e));
      }).finally(() => { polling.current = false; });
    };
    refresh();
    const timer = setInterval(refresh, 2500);
    return () => { alive.current = false; clearInterval(timer); };
  }, [bridge, platform, apply]);

  const action = (action: PhoneCallAction) => void run(() => bridge.phoneCallsAction(action));
  const connected = !!status?.deviceId;
  const line = status?.lines.find(line => line.id === lineId);
  const disabled = busy || !!readError;
  const dialable = connected && status?.canRoute && line?.canDial && status.input && status.output
    && !status.calls.some(call => call.state !== "ended") && /^\+?[0-9 ().-]+$/.test(number.trim())
    && (number.match(/[0-9]/g)?.length ?? 0) >= 1 && (number.match(/[0-9]/g)?.length ?? 0) <= 20;

  return <section className="space-y-3 border-t border-line pt-4" aria-label={t("phone.calls")}>
    <h3 className="font-semibold">{t("phone.calls")}</h3>
    {platform !== "windows" ? <p className="text-muted">{platform ? t("phone.calls_mac") : t("phone.checking")}</p> : <>
      <p className="text-muted">{t("phone.calls_help")}</p>
      {!status && !error && !readError && <p role="status">{t("phone.checking")}</p>}
      {status && !status.available && <p role="status">{t("phone.calls_unavailable")}</p>}
      {status?.available && <>
        {!status.bluetooth && <p role="status" className="text-muted">{t("phone.calls_no_bluetooth")}</p>}
        {!status.packaged && <p role="status" className="text-muted">{t("phone.calls_permissions")}</p>}
        <label className="block space-y-1">{t("phone.calls_device")}
          <select className={field} value={deviceId} disabled={busy || connected} onChange={e => { setDeviceId(e.target.value); setNumber(""); }}>
            <option value="">{t("phone.calls_choose")}</option>
            {status.devices.map(device => <option key={device.id} value={device.id}>{device.name}</option>)}
          </select>
        </label>
        {status.bluetooth && !status.devices.length && <p className="text-muted">{t("phone.calls_empty")}</p>}
        <div className="flex flex-wrap gap-2">
          {connected ? <Button disabled={busy} onClick={() => void run(() => bridge.phoneCallsDisconnect())}>{t("phone.calls_disconnect")}</Button>
            : <Button kind={status.packaged && status.bluetooth ? "primary" : "secondary"} disabled={busy || !deviceId || !status.packaged || !status.bluetooth} onClick={() => void run(() => bridge.phoneCallsConnect(deviceId))}>{t(busy ? "phone.working" : "phone.calls_connect")}</Button>}
          <Button disabled={busy} onClick={() => void run(() => bridge.phoneCallsStatus())}>{t("phone.calls_refresh")}</Button>
          <Button disabled={busy} onClick={() => void run(() => bridge.phoneCallsSettings(false))}>{t("phone.calls_bluetooth")}</Button>
        </div>
        {connected && <>
          <p role="status" className="text-muted">{t(status.canRoute ? "phone.calls_ready" : "phone.calls_no_route")}</p>
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs text-muted">
            <dt>{t("phone.calls_input")}</dt><dd className="break-words">{status.input ?? t("phone.calls_missing_audio")}</dd>
            <dt>{t("phone.calls_output")}</dt><dd className="break-words">{status.output ?? t("phone.calls_missing_audio")}</dd>
          </dl>
          <Button disabled={busy} onClick={() => void run(() => bridge.phoneCallsSettings(true))}>{t("phone.calls_audio_settings")}</Button>
          {!status.callControl && <p className="text-xs text-muted">{t("phone.calls_legacy")}</p>}
          {!status.lines.length && <p role="status" className="text-muted">{t("phone.calls_wait_line")}</p>}
          {status.lines.length > 0 && <form className="space-y-2" onSubmit={e => {
            e.preventDefault();
            if (!dialable || disabled) return;
            void run(async () => {
              const next = await bridge.phoneCallsAction({ kind: "dial", lineId, number: number.trim() });
              if (alive.current) setNumber("");
              return next;
            });
          }}>
            <label className="block space-y-1">{t("phone.calls_line")}
              <select className={field} value={lineId} disabled={disabled} onChange={e => setLineId(e.target.value)}>
                {status.lines.map((line, index) => <option key={line.id} value={line.id} disabled={!line.canDial}>{line.name || t("phone.calls_line_fallback", { number: index + 1 })}</option>)}
              </select>
            </label>
            <label className="block space-y-1">{t("phone.calls_number")}
              <input className={field} type="tel" autoComplete="off" maxLength={80} value={number} disabled={disabled} onChange={e => setNumber(e.target.value)} />
            </label>
            <Button type="submit" kind="primary" disabled={disabled || !dialable}>{t(status.callControl ? "phone.calls_dial" : "phone.calls_dial_legacy")}</Button>
          </form>}
          {status.calls.map(call => <div key={call.id} className="space-y-2 border-t border-line pt-3">
            <p role="status" className="font-medium">{t(`phone.call_state_${call.state}`)}</p>
            <p className="text-xs text-muted">{t(!readError && call.onComputer ? "phone.calls_on_pc" : "phone.calls_on_phone")}</p>
            <div className="flex flex-wrap gap-2">
              {call.state === "incoming" && <Button kind="primary" disabled={disabled || !status.canRoute || !status.input || !status.output} onClick={() => action({ kind: "answer", callId: call.id })}>{t("phone.calls_answer")}</Button>}
              {!call.onComputer && call.state !== "incoming" && call.state !== "ended" && <Button disabled={disabled || !status.canRoute} onClick={() => action({ kind: "takeAudio", callId: call.id })}>{t("phone.calls_take_audio")}</Button>}
              {call.state === "talking" && <Button disabled={disabled} onClick={() => action({ kind: "mute", callId: call.id, muted: !call.muted })}>{t(call.muted ? "phone.calls_unmute" : "phone.calls_mute")}</Button>}
              {call.state !== "ended" && <Button kind="danger" disabled={disabled} onClick={() => action({ kind: "end", callId: call.id })}>{t("phone.calls_end")}</Button>}
            </div>
          </div>)}
          <p className="text-xs text-muted">{t("phone.calls_keep")}</p>
        </>}
      </>}
      {notice && <p role="status" className="break-words text-muted">{notice}</p>}
      {(error || readError) && <div role="alert" className="space-y-2 break-words text-danger"><p>{error || readError}</p>
        <Button disabled={busy} onClick={() => void run(() => bridge.phoneCallsStatus())}>{t("phone.calls_refresh")}</Button>
      </div>}
    </>}
  </section>;
}
