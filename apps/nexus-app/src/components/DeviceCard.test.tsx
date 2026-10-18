import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { DeviceCard } from "./DeviceCard";
import { MOCK_SNAPSHOT, createMockBridge } from "../lib/bridge.mock";
import { translate, type Key } from "../lib/i18n";

const t = (key: Key, args?: Record<string, string | number>) => translate("en", key, args);
const device = MOCK_SNAPSHOT.devices[0]!;
const card = (overrides = {}) => renderToStaticMarkup(<DeviceCard device={device} now={Date.now()} wakingSince={null} connecting={false} t={t} onAction={() => {}} {...overrides} />);

describe("connection feedback", () => {
  it("offers Connect for an online PC without a wake action", () => {
    const html = card();
    expect(html).toContain("Connect");
    expect(html).not.toContain("Wake");
  });
  it("disables Connect while waiting for the actual desktop", () => {
    expect(card({ connecting: true })).toMatch(/<button[^>]*disabled[^>]*>Connecting…<\/button>/);
  });
  it("keeps failures and a retry instruction visible on the device", () => {
    const html = card({ connectionError: "PC refused access" });
    expect(html).toContain("The desktop could not open. Try Connect again.");
    expect(html).toContain("PC refused access");
    expect(html).toContain("Connection details");
  });
  it("explains what Wake does for sleeping devices", () => {
    expect(card({ device: { ...device, online: false, state: "sleeping" } })).toContain("Wake-on-LAN");
  });
  it("the browser demo never pretends to open a remote desktop", async () => {
    await expect(createMockBridge().connectScreen(device.id)).rejects.toThrow("installed Nexus Remote app");
  });
});
