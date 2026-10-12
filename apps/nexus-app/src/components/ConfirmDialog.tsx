import type { Key } from "../lib/i18n";
import { Button, Modal } from "./Modal";

type T = (key: Key, args?: Record<string, string | number>) => string;

/** Destructive power actions always ask first. */
export function ConfirmDialog({ action, name, t, onConfirm, onCancel }: { action: string; name: string; t: T; onConfirm: () => void; onCancel: () => void }) {
  return (
    <Modal title={t("confirm.title", { action, name })} onClose={onCancel} closeLabel={t("common.close")} danger>
      <p className="mb-4 text-sm text-muted">{t("confirm.body", { name })}</p>
      <div className="flex justify-end gap-2">
        <Button autoFocus onClick={onCancel}>
          {t("confirm.cancel")}
        </Button>
        <Button kind="danger" onClick={onConfirm}>
          {t("confirm.yes", { action })}
        </Button>
      </div>
    </Modal>
  );
}
