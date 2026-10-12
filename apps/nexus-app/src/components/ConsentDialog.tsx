import { useEffect, useState } from "react";
import type { Key } from "../lib/i18n";
import type { ConsentRequest } from "../lib/types";
import { Button, Modal } from "./Modal";

type T = (key: Key, args?: Record<string, string | number>) => string;

/** Attended access: a paired device wants in and a person at this machine decides. */
export function ConsentDialog({ request, t, onDecide }: { request: ConsentRequest; t: T; onDecide: (allow: boolean, remember: boolean) => void }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(id);
  }, []);
  const left = Math.max(0, request.timeoutSecs - Math.floor((now - request.receivedAtMs) / 1000));
  return (
    <Modal title={t("consent.title", { name: request.peerName })} onClose={() => onDecide(false, false)} closeLabel={t("consent.deny")} danger>
      <p className="mb-3 text-sm text-muted">{t("consent.body", { fingerprint: request.peerFingerprint, grants: request.grants.join(", ") })}</p>
      <p className="mb-4 text-xs text-muted" aria-live="polite">
        {t("consent.expires", { secs: left })}
      </p>
      <div className="flex flex-wrap justify-end gap-2">
        <Button autoFocus onClick={() => onDecide(false, false)}>
          {t("consent.deny")}
        </Button>
        <Button onClick={() => onDecide(true, true)}>{t("consent.always")}</Button>
        <Button kind="primary" onClick={() => onDecide(true, false)}>
          {t("consent.allow")}
        </Button>
      </div>
    </Modal>
  );
}
