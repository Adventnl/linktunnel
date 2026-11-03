// Drives the shipped UI/bridge with a simulated native boundary; no fake video is rendered.
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { chromium } from "playwright-core";
import { createServer } from "vite";

const server = await createServer({ server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
await server.listen();
const executablePath = process.env.CHROMIUM ?? ["C:/Program Files/Google/Chrome/Application/chrome.exe", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser"].find(existsSync);
const browser = await chromium.launch({ executablePath, headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1100, height: 760 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.stack ?? error.message));
  await page.addInitScript(() => {
    const listeners = new Set();
    const callbacks = new Map(); let next = 0;
    window.calls = []; window.failConnect = false;
    const device = { id: "01", name: "Windows PC", state: "online", online: true, lastSeenMs: Date.now(), osFamily: "windows", osVersion: "Windows 11", path: "lan", activeUser: "alex", batteryPercent: null, fingerprint: "TEST", unattended: true, grantsOnPeer: ["screen", "input", "terminal"] };
    window.emit = (payload) => listeners.forEach((handler) => callbacks.get(handler)?.({ payload }));
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    window.__TAURI_INTERNALS__ = {
      transformCallback: (callback) => { callbacks.set(++next, callback); return next; },
      unregisterCallback: () => {},
      invoke: async (command, args) => {
        window.calls.push({ command, args });
        switch (command) {
          case "plugin:event|listen": listeners.add(args.handler); return args.handler;
          case "plugin:event|unlisten": listeners.delete(args.eventId); return;
          case "snapshot": return { agent: "connected", error: null, sessions: [], devices: [device, { ...device, id: "02", name: "Offline PC", state: "offline", online: false }], status: { deviceId: "03", deviceName: "Local PC", fingerprint: "TEST", agentVersion: "test", signalConnected: true, signalUrl: "", lockdown: false, pairedDevices: 2, activeSessions: 0 } };
          case "viewer_layout": case "viewer_close": case "window_action": return;
          case "terminal_open": return { terminalId: 42 };
          case "terminal_close": case "terminal_input": case "terminal_resize": return;
          case "connect_screen":
            await new Promise((resolve) => setTimeout(resolve, 100));
            if (window.failConnect) throw new Error("PC refused access");
            return;
          default: throw new Error(`Unexpected command ${command}`);
        }
      },
    };
  });
  await page.goto(server.resolvedUrls.local[0]);
  const overview = page.getByRole("article");
  await overview.getByRole("heading", { name: "Windows PC" }).waitFor();
  const windowsBefore = page.context().pages().length;
  await page.getByLabel("Quality preference").selectOption("sharp");
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await page.getByRole("button", { name: "Disconnect", exact: true }).waitFor();
  await page.waitForTimeout(200);
  assert.equal(page.context().pages().length, windowsBefore, "Connect stays in one workspace");
  const start = await page.evaluate(() => window.calls.find((call) => call.command === "connect_screen"));
  assert.equal(start.args.options.quality, "sharp");
  assert.equal(start.args.options.viewport.visible, true);
  assert.ok(start.args.options.viewport.width > 600);
  await page.getByLabel("Picture size").selectOption("fill");
  await page.waitForFunction(() => window.calls.some((call) => call.command === "viewer_layout" && call.args.viewport.scale === "fill"));
  await page.getByRole("button", { name: "Toggle sidebar" }).click();
  await page.waitForFunction(() => window.calls.some((call) => call.command === "viewer_layout" && call.args.viewport.x < 10 && call.args.viewport.visible));
  await page.keyboard.press("Control+k");
  await page.getByRole("dialog").waitFor();
  await page.waitForFunction(() => window.calls.filter((call) => call.command === "viewer_layout").at(-1)?.args.viewport.visible === false);
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.calls.filter((call) => call.command === "viewer_layout").at(-1)?.args.viewport.visible === true);
  await page.getByRole("button", { name: "Toggle fullscreen" }).click();
  assert.ok(await page.evaluate(() => window.calls.some((call) => call.command === "window_action" && call.args.action === "fullscreen")));
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await overview.waitFor();
  assert.ok(await page.evaluate(() => window.calls.some((call) => call.command === "viewer_close")));
  await overview.getByRole("button", { name: "Terminal", exact: true }).click();
  await page.waitForFunction(() => window.calls.some((call) => call.command === "terminal_open"));
  await page.waitForTimeout(200); // Allow React StrictMode's development-only setup replay.
  const terminalStarts = await page.evaluate(() => window.calls.filter((call) => call.command === "terminal_open").length);
  await page.getByRole("tab", { name: "Desktop", exact: true }).click();
  await page.getByRole("tab", { name: "Terminal", exact: true }).click();
  assert.equal(await page.evaluate(() => window.calls.filter((call) => call.command === "terminal_open").length), terminalStarts, "Switching tabs preserves the terminal session");
  await page.getByRole("button", { name: "Close terminal", exact: true }).click();
  await overview.waitFor();
  await page.getByRole("button", { name: "Toggle sidebar" }).click();
  const offline = page.getByRole("button", { name: /Offline PC.*Offline/ });
  await offline.click({ button: "right" });
  const offlineHighlight = await page.locator(".selection-highlight").getAttribute("style");
  await page.getByRole("menuitem", { name: "Unpair", exact: true }).waitFor();
  await page.keyboard.press("End");
  assert.equal(await page.evaluate(() => document.activeElement.textContent), "Unpair");
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: /Windows PC.*Online/ }).click();
  assert.notEqual(await page.locator(".selection-highlight").getAttribute("style"), offlineHighlight);
  assert.match(await page.locator(".selection-highlight").evaluate((element) => getComputedStyle(element).transitionProperty), /transform/);
  await page.evaluate(() => { window.failConnect = true; });
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await overview.getByText("The desktop could not open. Try Connect again.").waitFor();
  await page.evaluate(() => { window.failConnect = false; });
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await page.getByRole("button", { name: "Disconnect", exact: true }).waitFor();
  await page.waitForTimeout(200);
  await page.evaluate(() => window.emit({ kind: "viewer_ended", id: "01", reason: "link lost" }));
  await overview.waitFor();
  await overview.getByText("Connection details", { exact: true }).click();
  await overview.getByText("link lost", { exact: true }).waitFor();
  await page.setViewportSize({ width: 420, height: 700 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(errors, []);
  console.log("Workspace checks passed: native bounds, quality, fill, sidebar, dialog visibility, fullscreen, disconnect, keyboard/context menu, offline removal entry, failure/retry and viewer exit.");
} finally { await browser.close(); await server.close(); }
