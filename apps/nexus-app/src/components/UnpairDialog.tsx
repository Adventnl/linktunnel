import { useRef, useState } from "react";
import type { Key } from "../lib/i18n";
import type { Bridge, Device } from "../lib/types";
import { Button, Modal } from "./Modal";

type T = (key: Key, args?: Record<string, string | number>) => string;

export function UnpairDialog({ bridge, device, t, onClose, onRemoved }: {
  bridge: Bridge;
  device: Device;
  t: T;
  onClose: () => void;
  onRemoved: (id: string) => void;
}) {
  const busy = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => { if (!busy.current) onClose(); };
  const unpair = async () => {
    if (busy.current) return;
    busy.current = true;
    setPending(true);
    setError(null);
    try {
      await bridge.unpair(device.id);
      onRemoved(device.id);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      busy.current = false;
      setPending(false);
    }
  };
  return (
    <Modal title={t("settings.unpair")} onClose={close} closeLabel={t("common.close")} danger>
      <p className="mb-4 break-words text-sm text-muted">{t("settings.unpair.confirm", { name: device.name })}</p>
      {error && <p role="alert" className="mb-4 break-words text-sm text-danger">{error}</p>}
      <div className="flex justify-end gap-2" aria-busy={pending}>
        <Button autoFocus disabled={pending} onClick={close}>{t("confirm.cancel")}</Button>
        <Button kind="danger" disabled={pending} onClick={() => void unpair()}>
          {pending ? t("settings.unpair.pending") : t("settings.unpair")}
        </Button>
      </div>
    </Modal>
  );
}
