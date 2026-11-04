import { PhonePanel } from "./PhonePanel";
import { ConfirmDialog } from "./ConfirmDialog";
import { ConsentDialog } from "./ConsentDialog";
import { PairingDialog } from "./PairingDialog";
import { UnpairDialog } from "./UnpairDialog";
import { SettingsDialog } from "./SettingsDialog";
import type { Key } from "../lib/i18n";
import type { AppModel } from "../hooks/useApp";

export function AppDialogs({ model }: { model: AppModel }) {
  const {
    bridge,
    state,
    dispatch,
    locale,
    setLocale,
    theme,
    setTheme,
    settings,
    setSettings,
    pairing,
    setPairing,
    unpairing,
    setUnpairing,
    confirm,
    setConfirm,
    terminal,
    setTerminal,
    setWaking,
    t,
    toast,
    fail,
    status,
    devices,
    consent,
  } = model;
  const remember = model.remember;
  return (
    <>
      {model.phone && <PhonePanel bridge={bridge} t={t} onClose={() => model.setPhone(false)} />}
      {settings && (
        <SettingsDialog
          bridge={bridge}
          t={t}
          status={status}
          devices={devices}
          onLockdown={model.toggleLockdown}
          quality={model.quality}
          onQuality={model.setQuality}
          theme={theme}
          locale={locale}
          onTheme={(v) => {
            setTheme(v);
            remember("nexus.theme", v);
          }}
          onLocale={(v) => {
            setLocale(v);
            remember("nexus.locale", v);
          }}
          onClose={() => setSettings(false)}
          onError={(text) => toast("error", text)}
          onUnpair={(device) => {
            setSettings(false);
            setUnpairing(device);
          }}
        />
      )}
      {pairing && (
        <PairingDialog
          bridge={bridge}
          t={t}
          status={status}
          sas={state.pairingSas}
          result={state.pairingResult}
          onClose={() => setPairing(false)}
        />
      )}
      {unpairing && (
        <UnpairDialog
          bridge={bridge}
          device={unpairing}
          t={t}
          onClose={() => setUnpairing(null)}
          onRemoved={(id) => {
            dispatch({ kind: "device_removed", id });
            if (terminal?.id === id) setTerminal(null);
            setWaking(({ [id]: _drop, ...rest }) => rest);
          }}
        />
      )}
      {confirm && (
        <ConfirmDialog
          action={t(`verb.${confirm.action}` as Key)}
          name={confirm.device.name}
          t={t}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            const { device, action } = confirm;
            setConfirm(null);
            void bridge
              .power(device.id, action)
              .then(() =>
                toast(
                  "info",
                  t("toast.done", {
                    action: t(`verb.${action}` as Key),
                    name: device.name,
                  }),
                ),
              )
              .catch((e: unknown) =>
                fail(t(`verb.${action}` as Key), device, e),
              );
          }}
        />
      )}
      {consent && (
        <ConsentDialog
          request={consent}
          t={t}
          onDecide={(allow, rememberChoice) =>
            void bridge
              .consentDecide(consent.requestId, allow, rememberChoice)
              .catch((e: unknown) => toast("error", String(e)))
          }
        />
      )}
    </>
  );
}
