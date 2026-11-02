import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { PairingDialog } from "./PairingDialog";
import { MOCK_SNAPSHOT, createMockBridge } from "../lib/bridge.mock";
import { translate, type Key } from "../lib/i18n";

const t = (key: Key, args?: Record<string, string | number>) => translate("en", key, args);
const render = (overrides = {}) => renderToStaticMarkup(<PairingDialog bridge={createMockBridge()} t={t} status={MOCK_SNAPSHOT.status} sas={null} result={null} onClose={() => {}} {...overrides} />);

describe("pairing availability", () => {
  it("explains disconnected signaling and prevents starting a failing pairing", () => {
    const html = render({ status: { ...MOCK_SNAPSHOT.status, signalConnected: false } });
    expect(html).toContain("check your network or VPN");
    expect(html).toContain("wss://signal.example.com/v1/ws");
    expect(html).toMatch(/<button[^>]*disabled[^>]*>Show a code<\/button>/);
    expect(html).toMatch(/<button[^>]*disabled[^>]*>Enter a code<\/button>/);
  });
  it("explains how to configure a missing pairing server", () => {
    expect(render({ status: { ...MOCK_SNAPSHOT.status, signalConnected: false, signalUrl: "" } })).toContain("net.signal_url");
  });
  it("ignores a previous pairing's result when reopened", () => {
    const html = render({ result: { pairingId: 10, success: true, reason: "", peerName: "Old device" } });
    expect(html).toContain("Show a code");
    expect(html).not.toContain("Paired with Old device");
  });
});
