import { useState } from "react";
import type { Key } from "../lib/i18n";
import type { Bridge, PairingSas } from "../lib/types";
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
  sas,
  result,
  onClose,
}: {
  bridge: Bridge;
  t: T;
  sas: PairingSas | null;
  result: { success: boolean; reason: string; peerName: string } | null;
  onClose: () => void;
}) {
  const [step, setStep] = useState<Step>({ name: "choose" });
  const currentId = sas?.pairingId ?? ("pairingId" in step ? step.pairingId : null);
  const cancel = () => {
    if (currentId !== null && !result) void bridge.pairCancel(currentId).catch(() => {});
    onClose();
  };
  const fail = (e: unknown) => setStep({ name: "failed", reason: e instanceof Error ? e.message : String(e) });

  return (
    <Modal title={t("pair.title")} onClose={cancel} closeLabel={t("common.close")}>
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
              onClick={() =>
                void bridge
                  .pairOffer()
                  .then((o) => setStep({ name: "offer", pairingId: o.pairingId, code: o.code }))
                  .catch(fail)
              }
            >
              {t("pair.show")}
            </Button>
            <Button onClick={() => setStep({ name: "join", code: "" })}>{t("pair.enter")}</Button>
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
            void bridge
              .pairJoin(step.code.trim())
              .then((j) => setStep({ name: "waiting", pairingId: j.pairingId }))
              .catch(fail);
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
            value={step.code}
            onChange={(e) => setStep({ name: "join", code: e.target.value })}
            className="w-full rounded-lg border border-line bg-panel2 px-3 py-2 font-mono"
          />
          <div className="flex justify-end gap-2">
            <Button onClick={cancel}>{t("pair.cancel")}</Button>
            <Button kind="primary" type="submit" disabled={step.code.trim().split(/\s+/).length < 6}>
              {t("pair.join")}
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
