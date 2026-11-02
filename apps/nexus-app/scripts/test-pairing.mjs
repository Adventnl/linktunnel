// Real UI and Tauri bridge, with a simulated native boundary for network failures.
import assert from "node:assert/strict";
import { chromium } from "playwright-core";
import { existsSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";

const executablePath = process.env.CHROMIUM ?? [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/usr/bin/chromium", "/usr/bin/chromium-browser",
].find(existsSync);
const browser = await chromium.launch({ executablePath, headless: true });
const server = await createServer({ server: { host: "127.0.0.1", port: 0 }, logLevel: "error" });
await server.listen();
const shots = fileURLToPath(new URL("../shots/", import.meta.url));
mkdirSync(shots, { recursive: true });

try {
  for (const width of [1100, 420]) {
    const context = await browser.newContext({ viewport: { width, height: 900 } });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.addInitScript(() => {
      const handlers = new Map();
      let callbackId = 0;
      let pairingId = 0;
      let listener;
      const device = { id: "01", name: "Offline PC", state: "offline", online: false, lastSeenMs: 0,
        osFamily: "windows", osVersion: "Windows 11", path: "none", activeUser: "", batteryPercent: null,
        fingerprint: "TEST", unattended: true, grantsOnPeer: ["screen", "terminal"] };
      window.testCalls = [];
      window.testFailUnpair = false;
      window.testEmit = (payload) => handlers.get(listener)?.({ payload });
      window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
      window.__TAURI_INTERNALS__ = {
        transformCallback: (cb) => { handlers.set(++callbackId, cb); return callbackId; },
        unregisterCallback: () => {},
        invoke: async (command, args) => {
          window.testCalls.push({ command, args });
          switch (command) {
            case "plugin:event|listen": listener = args.handler; return args.handler;
            case "plugin:event|unlisten": return;
            case "snapshot": return { agent: "connected", error: null, sessions: [], devices: [device],
              status: { deviceId: "02", deviceName: "Mac", fingerprint: "TEST", agentVersion: "test",
                signalConnected: false, signalUrl: "ws://100.64.0.1:8443/v1/ws", lockdown: false,
                pairedDevices: 1, activeSessions: 0 } };
            case "pair_offer":
              await new Promise((r) => setTimeout(r, 250));
              return { pairingId: ++pairingId, code: "orbit candle harvest meadow tunnel silver" };
            case "pair_cancel": return;
            case "pair_confirm":
              window.testEmit({ kind: "pairing_finished", pairingId: args.pairingId, success: true, reason: "", peerName: "New PC" });
              return;
            case "unpair":
              await new Promise((r) => setTimeout(r, 250));
              if (window.testFailUnpair) throw new Error("Could not save the device removal");
              return; // Deliberately omit the event: the acknowledgement must also remove the card.
            default: throw new Error(`Unexpected command: ${command}`);
          }
        },
      };
    });
    await page.goto(server.resolvedUrls.local[0]);
    const card = page.getByRole("article");
    await card.getByText("Offline PC", { exact: true }).waitFor();
    await page.getByRole("button", { name: /Pair a device/ }).first().click();
    const pairing = page.getByRole("dialog", { name: "Pair a device" });
    assert.equal(await pairing.getByRole("button", { name: "Show a code" }).isDisabled(), true);
    assert.equal(await pairing.getByRole("button", { name: "Enter a code" }).isDisabled(), true);
    await pairing.getByText(/check your network or VPN/).waitFor();
    await page.screenshot({ path: `${shots}pairing-offline-${width}.png` });
    await page.evaluate(() => window.testEmit({ kind: "signal_state", connected: true }));
    await pairing.getByRole("button", { name: "Show a code" }).click();
    assert.equal(await pairing.getByRole("button", { name: "Starting pairing…" }).isDisabled(), true);
    await pairing.getByTestId("pair-code").waitFor();
    await page.evaluate(() => window.testEmit({ kind: "pairing_sas", sas: { pairingId: 1,
      words: ["one", "two", "three", "four"], digits: "1234", peerName: "New PC", peerFingerprint: "TEST" } }));
    await pairing.getByRole("button", { name: "They match" }).click();
    await pairing.getByText("Paired with New PC.").waitFor();
    await pairing.getByRole("button", { name: "Done" }).click();
    // A completed pairing must not trap subsequent attempts on the previous result.
    await page.getByRole("button", { name: /Pair a device/ }).first().click();
    await pairing.getByRole("button", { name: "Show a code" }).click();
    await pairing.getByRole("button", { name: "Close", exact: true }).click();
    await page.waitForFunction(() => window.testCalls.some((c) => c.command === "pair_cancel" && c.args.pairingId === 2));
    // Unpair remains available offline, cancellation keeps the device, and failures allow retry.
    await page.evaluate(() => window.testEmit({ kind: "signal_state", connected: false }));
    await card.getByRole("button", { name: "Unpair", exact: true }).click();
    const removal = page.getByRole("dialog", { name: "Unpair", exact: true });
    await removal.getByRole("button", { name: "Cancel" }).click();
    assert.equal(await card.count(), 1);
    await card.getByRole("button", { name: "Unpair", exact: true }).click();
    await page.evaluate(() => { window.testFailUnpair = true; });
    await removal.getByRole("button", { name: "Unpair", exact: true }).click();
    await removal.getByRole("alert").filter({ hasText: "Could not save" }).waitFor();
    assert.equal(await card.count(), 1);
    await page.screenshot({ path: `${shots}unpair-error-${width}.png` });
    await page.evaluate(() => { window.testFailUnpair = false; });
    await removal.getByRole("button", { name: "Unpair", exact: true }).click();
    assert.equal(await removal.getByRole("button", { name: "Unpairing…" }).isDisabled(), true);
    await card.waitFor({ state: "detached" });
    await removal.waitFor({ state: "detached" });
    assert.equal(await page.evaluate(() => window.testCalls.filter((c) => c.command === "unpair").length), 2);
    assert.deepEqual(errors, []);
    await context.close();
  }
  console.log("Pairing recovery, repeat pairing, cancellation and offline unpair passed at 1100px and 420px.");
} finally {
  await browser.close();
  await server.close();
}
