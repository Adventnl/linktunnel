import { useEffect, useRef, useState } from "react";
import type { Key } from "../lib/i18n";
import type { AgentStatus, Bridge, PairingSas } from "../lib/types";
import { Button, Modal } from "./Modal";

type T = (key: Key, args?: Record<string, string | number>) => string;

type Step =
  | { name: "choose" }
  | { name: "offer"; pairingId: number; code: string }
  | { name: "join"; code: string }
  | { name: "waiting"; pairingId: number }
  | { name: "failed"; reason: string };

/** The pairing wizard: show a code or type one, then compare four words on both screens. */
export function PairingDialog({
  bridge,
  t,
  status,
  sas: latestSas,
  result: latestResult,
  onClose,
}: {
  bridge: Bridge;
  t: T;
  status: AgentStatus | null;
  sas: PairingSas | null;
  result: { pairingId: number; success: boolean; reason: string; peerName: string } | null;
  onClose: () => void;
}) {
  const [step, setStep] = useState<Step>({ name: "choose" });
  const [pending, setPending] = useState(false);
  const busy = useRef(false);
  const closed = useRef(false);
  useEffect(() => {
    closed.current = false;
    return () => { closed.current = true; };
  }, []);
  const currentId = "pairingId" in step ? step.pairingId : null;
  const sas = latestSas?.pairingId === currentId ? latestSas : null;
  const result = latestResult?.pairingId === currentId ? latestResult : null;
  const ready = !!status?.signalConnected && !status.lockdown;
  const cancel = () => {
    closed.current = true;
    if (currentId !== null && !result) void bridge.pairCancel(currentId).catch(() => {});
    onClose();
  };
  const fail = (e: unknown) => setStep({ name: "failed", reason: e instanceof Error ? e.message : String(e) });
  const start = async (code?: string) => {
    if (busy.current || !ready) return;
    busy.current = true;
    setPending(true);
    try {
      const started = code === undefined ? await bridge.pairOffer() : await bridge.pairJoin(code);
      if (closed.current) {
        await bridge.pairCancel(started.pairingId).catch(() => {});
      } else if ("code" in started && typeof started.code === "string") {
        setStep({ name: "offer", pairingId: started.pairingId, code: started.code });
      } else {
        setStep({ name: "waiting", pairingId: started.pairingId });
      }
    } catch (e) {
      if (!closed.current) fail(e);
    } finally {
      busy.current = false;
      if (!closed.current) setPending(false);
    }
  };

  return (
    <Modal title={t("pair.title")} onClose={cancel} closeLabel={t("common.close")}>
      {!ready && !result && <div role="status" className="mb-4 space-y-2 text-sm text-muted">
        <p>{t(!status ? "agent.unreachable.help" : status.lockdown ? "lockdown.disable" : status.signalUrl ? "pair.offline" : "pair.unconfigured")}</p>
        {status?.signalUrl && !status.signalConnected && <p className="break-words text-xs">{status.signalUrl}</p>}
      </div>}
      {result ? (
        <div className="space-y-4">
          <p role="status" className={result.success ? "text-ok" : "text-danger"}>
            {result.success ? t("pair.success", { name: result.peerName }) : t("pair.failure", { reason: result.reason })}
          </p>
          <div className="flex justify-end">
            <Button kind="primary" autoFocus onClick={onClose}>
              {t("pair.close")}
            </Button>
          </div>
        </div>
      ) : sas ? (
        <div className="space-y-4">
          <p className="text-sm text-muted">{t("pair.compare")}</p>
          <p className="rounded-lg bg-panel2 p-4 text-center font-mono text-xl tracking-wide" aria-label={sas.words.join(" ")}>
            {sas.words.join("  ")}
          </p>
          <p className="text-center text-xs text-muted">
            {sas.peerName} · {sas.peerFingerprint}
          </p>
          <div className="flex justify-end gap-2">
            <Button kind="danger" onClick={() => void bridge.pairConfirm(sas.pairingId, false).catch(fail)}>
              {t("pair.mismatch")}
            </Button>
            <Button kind="primary" autoFocus onClick={() => void bridge.pairConfirm(sas.pairingId, true).catch(fail)}>
              {t("pair.match")}
            </Button>
          </div>
        </div>
      ) : step.name === "choose" ? (
        <div className="space-y-4">
          <p className="text-sm text-muted">{t("pair.intro")}</p>
          <div className="flex gap-2">
            <Button
              kind="primary"
              autoFocus
              disabled={!ready || pending}
              onClick={() => void start()}
            >
              {t(pending ? "pair.starting" : "pair.show")}
            </Button>
            <Button disabled={!ready || pending} onClick={() => setStep({ name: "join", code: "" })}>{t("pair.enter")}</Button>
          </div>
        </div>
      ) : step.name === "offer" ? (
        <div className="space-y-3">
          <p className="text-sm text-muted">{t("pair.code_label")}</p>
          <p className="select-all rounded-lg bg-panel2 p-4 text-center font-mono text-lg" data-testid="pair-code">
            {step.code}
          </p>
          <p className="text-xs text-muted pulse">{t("pair.waiting")}</p>
          <div className="flex justify-end">
            <Button onClick={cancel}>{t("pair.cancel")}</Button>
          </div>
        </div>
      ) : step.name === "join" ? (
        <form
          className="space-y-3"
          onSubmit={(e) => {
            e.preventDefault();
            void start(step.code.trim());
          }}
        >
          <label className="block text-sm text-muted" htmlFor="pair-code-input">
            {t("pair.enter_label")}
          </label>
          <input
            id="pair-code-input"
            data-autofocus
            autoComplete="off"
            spellCheck={false}
            disabled={pending}
            value={step.code}
            onChange={(e) => setStep({ name: "join", code: e.target.value })}
            className="w-full rounded-lg border border-line bg-panel2 px-3 py-2 font-mono"
          />
          <div className="flex justify-end gap-2">
            <Button onClick={cancel}>{t("pair.cancel")}</Button>
            <Button kind="primary" type="submit" disabled={!ready || pending || step.code.trim().split(/\s+/).length < 6}>
              {t(pending ? "pair.starting" : "pair.join")}
            </Button>
          </div>
        </form>
      ) : step.name === "waiting" ? (
        <p className="text-sm text-muted pulse">{t("pair.waiting")}</p>
      ) : (
        <div className="space-y-4">
          <p role="alert" className="text-danger">
            {t("pair.failure", { reason: step.reason })}
          </p>
          <div className="flex justify-end">
            <Button autoFocus onClick={() => setStep({ name: "choose" })}>
              {t("common.back")}
            </Button>
          </div>
        </div>
      )}
    </Modal>
  );
}
