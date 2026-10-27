// Integration checks against the real UI/bridge with a simulated native boundary.
// Starts a temporary local Vite server; optionally set PHONE_TEST_URL and CHROMIUM.
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
const server = process.env.PHONE_TEST_URL ? null : await createServer({ server: { host: "127.0.0.1", port: 0 }, logLevel: "error" });
await server?.listen();
const base = process.env.PHONE_TEST_URL ?? server.resolvedUrls.local[0];
const shots = fileURLToPath(new URL("../shots/", import.meta.url));
mkdirSync(shots, { recursive: true });

async function setup(platform, missing = false) {
  const context = await browser.newContext({ viewport: { width: 1100, height: 760 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(({ platform, missing }) => {
    let session = { serial: null, error: null };
    window.phoneTestCalls = [];
    window.phoneTestClose = () => { session = { serial: null, error: "Phone disconnected" }; };
    window.phoneTestFail = false;
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      unregisterCallback: () => {},
      invoke: async (command, args) => {
        window.phoneTestCalls.push({ command, args });
        switch (command) {
          case "plugin:event|listen": return 1;
          case "plugin:event|unlisten": return;
          case "snapshot": return { agent: "unreachable", status: null, devices: [], sessions: [], error: null };
          case "phone_status": return { platform, adb: missing ? { Err: "adb not installed" } : { Ok: "Android Debug Bridge version 1.0.41" }, scrcpy: { Ok: "scrcpy 4.0" }, session };
          case "phone_devices": return [
            { serial: "redmi123", model: "Redmi Note 14 Pro", state: "device" },
            { serial: "other", model: "Other phone", state: "unauthorized" },
          ];
          case "phone_session": return session;
          case "phone_start":
            await new Promise((resolve) => setTimeout(resolve, 100));
            if (window.phoneTestFail) throw new Error("Could not receive a phone picture");
            session = { serial: args.options.serial, error: null }; return;
          case "phone_stop": session = { serial: null, error: null }; return;
          case "phone_pair": return;
          case "phone_connect": return;
          case "phone_disconnect": return;
          case "phone_calls": return;
          default: throw new Error(`Unexpected command: ${command}`);
        }
      },
    };
  }, { platform, missing });
  await page.goto(base);
  await page.getByRole("button", { name: "Phone", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Phone", exact: true });
  await dialog.getByText("scrcpy: scrcpy 4.0").waitFor({ state: "attached" });
  return { context, page, dialog, errors };
}

try {
  const { context, page, dialog, errors } = await setup("windows");
  // Phone control remains available with an unreachable desktop agent.
  await page.getByText("The linktunnel agent is not running", { exact: true }).waitFor();
  const open = dialog.getByRole("button", { name: "Open phone", exact: true });
  await open.waitFor();
  await open.click({ trial: true });
  assert.equal(await open.isEnabled(), true);
  assert.equal(await dialog.getByRole("option", { name: /Other phone/ }).evaluate((option) => option.disabled), true);
  assert.equal(await dialog.getByLabel("Keep phone display off").isChecked(), true);
  assert.equal(await dialog.getByLabel(/Play media audio/).isChecked(), false);
  await open.click();
  await dialog.getByText("Phone window active: redmi123", { exact: true }).waitFor();
  const options = await page.evaluate(() => window.phoneTestCalls.find((c) => c.command === "phone_start").args.options);
  assert.deepEqual(options, { serial: "redmi123", screenOff: true, mediaAudio: false, maxSize: 1600, maxFps: 60 });
  await dialog.getByRole("button", { name: "Stop phone control" }).click();
  await dialog.getByText("No phone window is running.", { exact: true }).waitFor();
  await page.evaluate(() => { window.phoneTestFail = true; });
  await open.click();
  await dialog.getByRole("alert").filter({ hasText: "Could not receive a phone picture" }).waitFor();
  await page.evaluate(() => { window.phoneTestFail = false; });
  await open.click();
  await dialog.getByText("Phone window active: redmi123", { exact: true }).waitFor();
  await page.evaluate(() => window.phoneTestClose());
  await dialog.getByRole("alert").filter({ hasText: "Phone disconnected" }).waitFor();
  await dialog.getByText("Wireless pairing (Android 11+)", { exact: true }).click();
  await dialog.getByLabel("Pairing IP address:port").fill("192.168.1.20:37123");
  await dialog.getByLabel("Six-digit pairing code").fill("123456");
  await dialog.getByRole("button", { name: "Pair phone", exact: true }).click();
  await dialog.getByText("Paired. Now enter the connection address from the main Wireless debugging screen.", { exact: true }).waitFor();
  assert.equal(await dialog.getByLabel("Six-digit pairing code").inputValue(), "");
  await dialog.getByLabel("Connection IP address:port").fill("192.168.1.20:40221");
  await dialog.getByRole("button", { name: "Connect wirelessly", exact: true }).click();
  await dialog.getByRole("button", { name: "Open Windows Phone Link", exact: true }).click();
  await dialog.getByText(/Phone Link launch requested/).waitFor();
  const calls = await page.evaluate(() => window.phoneTestCalls);
  assert.deepEqual(calls.find((c) => c.command === "phone_pair").args, { address: "192.168.1.20:37123", code: "123456" });
  assert.deepEqual(calls.find((c) => c.command === "phone_connect").args, { address: "192.168.1.20:40221" });
  assert.ok(calls.some((c) => c.command === "phone_calls"));
  await page.screenshot({ path: `${shots}/phone-windows.png` });
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  assert.deepEqual(errors, []);
  await context.close();

  const mac = await setup("macos");
  await mac.dialog.getByText(/Cellular-call audio handoff for Android on Mac is not implemented/).waitFor();
  assert.equal(await mac.dialog.getByRole("button", { name: "Open Windows Phone Link" }).count(), 0);
  await mac.page.setViewportSize({ width: 420, height: 650 });
  await mac.page.screenshot({ path: `${shots}/phone-mac-compact.png` });
  assert.deepEqual(mac.errors, []);
  await mac.context.close();

  const missing = await setup("windows", true);
  assert.equal(await missing.dialog.getByRole("button", { name: "Open phone", exact: true }).isDisabled(), true);
  await missing.dialog.getByText("adb: adb not installed", { exact: true }).waitFor();
  assert.deepEqual(missing.errors, []);
  await missing.context.close();
  console.log("Phone UI integration checks passed (Windows, Mac, unavailable tools, offline agent, pairing, lifecycle and failures).");
} finally { await browser.close(); await server?.close(); }
