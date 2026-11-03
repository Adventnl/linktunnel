import type { Bridge } from "./lib/types";
import { useApp } from "./hooks/useApp";
import { AppShell } from "./components/AppShell";
import { AppDialogs } from "./components/AppDialogs";

export function App({ bridge }: { bridge: Bridge }) {
  const model = useApp(bridge);
  return (
    <>
      <AppShell model={model} />
      <AppDialogs model={model} />
    </>
  );
}
