import { describe, expect, it } from "vitest";
import { ALL_KEYS, TABLES, parseLocale, translate } from "./i18n";
import { MOCK_SNAPSHOT } from "./bridge.mock";
import { paletteEntries, verbsFor } from "./palette";
import { initialState, reduce, sortDevices } from "./state";
import { canConnect, canWake, isAsleep, seenParts } from "./status";
import type { Device } from "./types";
import { b64ToBytes, textToB64 } from "./b64";

const dev = (id: string, name: string, over: Partial<Device> = {}): Device => ({
  ...MOCK_SNAPSHOT.devices[0]!,
  id,
  name,
  ...over,
});

describe("device ordering", () => {
  it("lists online devices first, then by name", () => {
    const sorted = sortDevices([dev("1", "zed", { online: false }), dev("2", "beta"), dev("3", "Alpha")]);
    expect(sorted.map((d) => d.name)).toEqual(["Alpha", "beta", "zed"]);
  });
});

describe("who is connected", () => {
  it("follows sessions_changed, and an unreachable agent clears it", () => {
    const s = reduce(initialState, {
      kind: "sessions_changed",
      sessions: [{ id: 1, deviceId: "ab", peerName: "Sam's laptop", startedMs: 0, path: "lan", grants: ["screen"] }],
    });
    expect(s.snapshot.sessions.map((x) => x.peerName)).toEqual(["Sam's laptop"]);
    expect(reduce(s, { kind: "sessions_changed", sessions: [] }).snapshot.sessions).toHaveLength(0);
    expect(reduce(s, { kind: "agent", agent: "unreachable", error: null }).snapshot.sessions).toHaveLength(0);
  });
});

describe("reducer", () => {
  const withDevices = reduce(initialState, { kind: "snapshot", snapshot: MOCK_SNAPSHOT });
  it("loads a snapshot sorted", () => {
    expect(withDevices.snapshot.agent).toBe("connected");
    expect(withDevices.snapshot.devices[0]?.online).toBe(true);
  });
  it("updates a device in place and keeps the order rules", () => {
    const offline = dev("01", "Windows PC", { online: false, state: "offline" });
    const s = reduce(withDevices, { kind: "device_changed", device: offline });
    expect(s.snapshot.devices.filter((d) => d.id === "01")).toHaveLength(1);
    expect(s.snapshot.devices.at(-1)?.online).toBeFalsy();
  });
  it("adds and removes devices", () => {
    const s = reduce(withDevices, { kind: "device_changed", device: dev("99", "New") });
    expect(s.snapshot.devices.some((d) => d.id === "99")).toBe(true);
    const gone = reduce(s, { kind: "device_removed", id: "99" });
    expect(gone.snapshot.devices.some((d) => d.id === "99")).toBe(false);
  });
  it("tracks the signaling state", () => {
    const s = reduce(withDevices, { kind: "signal_state", connected: false });
    expect(s.snapshot.status?.signalConnected).toBe(false);
  });
  it("queues consent prompts and clears them when resolved or the agent goes away", () => {
    const req = { requestId: 5, peerName: "PC", peerFingerprint: "X", grants: ["screen"], timeoutSecs: 30, path: "lan" as const, receivedAtMs: 1 };
    let s = reduce(withDevices, { kind: "consent_request", request: req });
    s = reduce(s, { kind: "consent_request", request: req }); // duplicates collapse
    expect(s.consents).toHaveLength(1);
    expect(reduce(s, { kind: "consent_resolved", requestId: 5 }).consents).toHaveLength(0);
    expect(reduce(s, { kind: "agent", agent: "unreachable", error: "x" }).consents).toHaveLength(0);
  });
  it("an unreachable agent clears the device list", () => {
    const s = reduce(withDevices, { kind: "agent", agent: "unreachable", error: "gone" });
    expect(s.snapshot.devices).toHaveLength(0);
    expect(s.snapshot.error).toBe("gone");
  });
  it("pairing flow state", () => {
    const sas = { pairingId: 1, words: ["a", "b", "c", "d"], digits: "1", peerName: "P", peerFingerprint: "F" };
    let s = reduce(withDevices, { kind: "pairing_sas", sas });
    expect(s.pairingSas?.words).toHaveLength(4);
    s = reduce(s, { kind: "pairing_finished", pairingId: 1, success: true, reason: "", peerName: "P" });
    expect(s.pairingSas).toBeNull();
    expect(s.pairingResult?.success).toBe(true);
  });
  it("keeps at most four toasts", () => {
    let s = initialState;
    for (let i = 0; i < 9; i++) s = reduce(s, { kind: "notice", level: "info", text: String(i) });
    expect(s.toasts.length).toBeLessThanOrEqual(4);
    expect(s.toasts.at(-1)?.text).toBe("8");
  });
});

describe("what you can do to a device", () => {
  it("connectable, asleep and offline", () => {
    const up = dev("1", "up");
    const asleep = dev("2", "zz", { state: "sleeping", online: false });
    const off = dev("3", "off", { state: "offline", online: false });
    const sleepingButOnline = dev("4", "weird", { state: "sleeping", online: true });
    expect([canConnect(up), canWake(up)]).toEqual([true, false]);
    expect([isAsleep(asleep), canConnect(asleep), canWake(asleep)]).toEqual([true, false, true]);
    expect([canConnect(off), canWake(off)]).toEqual([false, true]);
    expect(canConnect(sleepingButOnline)).toBe(false);
    expect(verbsFor(up)).toContain("shutdown");
    expect(verbsFor(off)).toEqual(["wake"]);
  });
  it("relative times", () => {
    expect(seenParts(0, 5000).key).toBe("seen.never");
    expect(seenParts(1000, 3000)).toEqual({ key: "seen.now", n: 0 });
    expect(seenParts(0 + 1, 1 + 30_000)).toEqual({ key: "seen.secs", n: 30 });
    expect(seenParts(1, 1 + 5 * 60_000)).toEqual({ key: "seen.mins", n: 5 });
    expect(seenParts(1, 1 + 3 * 3_600_000)).toEqual({ key: "seen.hours", n: 3 });
    expect(seenParts(1, 1 + 2 * 86_400_000)).toEqual({ key: "seen.days", n: 2 });
  });
});

describe("command palette", () => {
  const label = (v: string) => v[0]!.toUpperCase() + v.slice(1);
  const devices = [dev("1", "Windows PC"), dev("2", "Old tower", { state: "sleeping", online: false })];
  it("matches every word, in any order", () => {
    const hits = paletteEntries(devices, "term win", label);
    expect(hits).toHaveLength(1);
    expect(hits[0]?.verb).toBe("terminal");
    expect(paletteEntries(devices, "win term", label)).toHaveLength(1);
  });
  it("offers wake only where it makes sense", () => {
    const wakes = paletteEntries(devices, "wake", label);
    expect(wakes.map((e) => e.device.name)).toEqual(["Old tower"]);
  });
  it("an empty query lists everything", () => {
    expect(paletteEntries(devices, "", label).length).toBe(verbsFor(devices[0]!).length + verbsFor(devices[1]!).length);
  });
  it("nothing matches nonsense", () => {
    expect(paletteEntries(devices, "zzz", label)).toEqual([]);
  });
});

describe("i18n", () => {
  it("English and Chinese have exactly the same keys and placeholders", () => {
    const ph = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
    for (const key of ALL_KEYS) {
      expect(TABLES["zh-CN"][key], key).toBeTruthy();
      expect(ph(TABLES["zh-CN"][key]), key).toEqual(ph(TABLES.en[key]));
    }
  });
  it("substitutes arguments and leaves unknown placeholders visible", () => {
    expect(translate("en", "seen.secs", { n: 7 })).toBe("7s ago");
    expect(translate("zh-CN", "seen.secs", { n: 7 })).toBe("7 秒前");
    expect(translate("en", "toast.wake_sent", {})).toContain("{name}");
  });
  it("parses locale tags like the CLI does", () => {
    expect(parseLocale("zh-CN")).toBe("zh-CN");
    expect(parseLocale("zh_Hans_CN")).toBe("zh-CN");
    expect(parseLocale("zh-TW")).toBe("en");
    expect(parseLocale("zh-Hant")).toBe("en");
    expect(parseLocale("fr-FR")).toBe("en");
    expect(parseLocale(undefined)).toBe("en");
  });
});

describe("terminal byte encoding", () => {
  it("round-trips UTF-8 keystrokes and binary output", () => {
    const text = "echo héllo ✓ 你好\r";
    expect(new TextDecoder().decode(b64ToBytes(textToB64(text)))).toBe(text);
    const bytes = Uint8Array.from([0, 255, 27, 91, 72]);
    let bin = "";
    for (const b of bytes) bin += String.fromCharCode(b);
    expect([...b64ToBytes(btoa(bin))]).toEqual([...bytes]);
  });
});
