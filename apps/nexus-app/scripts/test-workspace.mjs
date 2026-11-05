// Drives the shipped UI/bridge with a simulated native boundary; no fake video is rendered.
import assert from "node:assert/strict";
import { existsSync, mkdirSync } from "node:fs";
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
    const listeners = new Map();
    const callbacks = new Map(); let next = 0;
    window.calls = []; window.failConnect = false;
    const device = { id: "01", name: "Windows PC", state: "online", online: true, lastSeenMs: Date.now(), osFamily: "windows", osVersion: "Windows 11", path: "lan", activeUser: "alex", batteryPercent: null, fingerprint: "TEST", unattended: true, grantsOnPeer: ["screen", "input", "terminal"] };
    window.maximized = false;
    window.emit = (payload) => listeners.forEach((event, handler) => { if (event === "nexus://event") callbacks.get(handler)?.({ payload }); });
    const resized = () => listeners.forEach((event, handler) => { if (event === "tauri://resize") callbacks.get(handler)?.({ payload: { width: innerWidth, height: innerHeight } }); });
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" } },
      transformCallback: (callback) => { callbacks.set(++next, callback); return next; },
      unregisterCallback: () => {},
      invoke: async (command, args) => {
        window.calls.push({ command, args });
        switch (command) {
          case "plugin:event|listen": listeners.set(args.handler, args.event); return args.handler;
          case "plugin:event|unlisten": listeners.delete(args.eventId); return;
          case "desktop_info": return { platform: "windows", embedded: true, account: "Test account" };
          case "snapshot": return { agent: "connected", error: null, sessions: [], devices: [device, { ...device, id: "02", name: "Offline PC", state: "offline", online: false }], status: { deviceId: "03", deviceName: "Local PC", fingerprint: "TEST", agentVersion: "test", signalConnected: true, signalUrl: "", lockdown: false, pairedDevices: 2, activeSessions: 0 } };
          case "set_lockdown":
            window.emit({ kind: "snapshot", snapshot: { agent: "connected", error: null, sessions: [], devices: [device], status: { deviceId: "03", deviceName: "Local PC", fingerprint: "TEST", agentVersion: "test", signalConnected: true, signalUrl: "", lockdown: args.on, pairedDevices: 1, activeSessions: 0 } } });
            return;
          case "plugin:window|is_maximized": return window.maximized;
          case "window_action":
            if (args.action === "maximize") { window.maximized = !window.maximized; resized(); }
            return;
          case "viewer_layout": case "viewer_close": return;
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
  // Reproduce the user's Windows setting, then verify the explicit app choice.
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-transparency", value: "reduce" }] });
  assert.equal(await page.evaluate(() => matchMedia("(prefers-reduced-transparency: reduce)").matches), true);
  assert.equal(await page.locator(".workspace-main").evaluate(el => getComputedStyle(el).backdropFilter), "none");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByLabel("Transparency", { exact: true }).selectOption("glass");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => getComputedStyle(document.querySelector(".workspace-main")).backdropFilter.includes("blur"));
  await page.reload();
  await overview.waitFor();
  assert.equal(await page.evaluate(() => localStorage.getItem("nexus.material")), "glass");
  assert.match(await page.locator(".workspace-main").evaluate(el => getComputedStyle(el).backdropFilter), /blur/);
  await page.waitForFunction(() => getComputedStyle(document.querySelector(".workspace-main")).getPropertyValue("--glass").trim() === "rgba(255, 255, 255, 0.35)");
  await page.getByRole("button", { name: "Maximize", exact: true }).click();
  await page.getByRole("button", { name: "Restore down", exact: true }).click();
  await page.getByRole("button", { name: "Maximize", exact: true }).waitFor();
  await page.getByRole("button", { name: "Minimize", exact: true }).click();
  assert.ok(await page.evaluate(() => window.calls.some(call => call.command === "window_action" && call.args.action === "minimize")));
  mkdirSync("../../.impeccable/review", { recursive: true });
  await page.screenshot({ animations: "disabled", path: "../../.impeccable/review/windows-glass.png" });
  await page.setViewportSize({ width: 420, height: 700 });
  for (const name of ["Minimize", "Maximize", "Close"]) {
    const bounds = await page.getByRole("button", { name, exact: true }).boundingBox();
    assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= 420, `${name} remains visible in the minimum-width window`);
  }
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.screenshot({ animations: "disabled", path: "../../.impeccable/review/windows-narrow.png" });
  await page.setViewportSize({ width: 1100, height: 760 });
  const windowsBefore = page.context().pages().length;
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByLabel("Quality preference").selectOption("sharp");
  await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).click();
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
  const beforeCollapse = await page.locator(".viewport-active").boundingBox();
  await page.getByRole("button", { name: "Toggle sidebar" }).click();
  await page.waitForFunction(() => {
    const rect = document.querySelector(".viewport-active").getBoundingClientRect();
    const bounds = window.calls.filter(call => call.command === "viewer_layout").at(-1)?.args.viewport;
    return bounds?.visible && bounds.x === Math.round(rect.x * devicePixelRatio) && bounds.width === Math.round(rect.width * devicePixelRatio);
  });
  const afterCollapse = await page.locator(".viewport-active").boundingBox();
  assert.ok(afterCollapse.x < beforeCollapse.x, "Collapsing navigation moves the native picture with the workspace");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("dialog").waitFor();
  await page.waitForFunction(() => window.calls.filter((call) => call.command === "viewer_layout").at(-1)?.args.viewport.visible === false);
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => window.calls.filter((call) => call.command === "viewer_layout").at(-1)?.args.viewport.visible === true);
  const bounds = await page.locator(".viewport-active").boundingBox();
  assert.ok(Math.abs(bounds.width / bounds.height - 1920 / 1080) < 0.01, "Embedded viewport follows the remote aspect ratio");
  await page.getByRole("button", { name: "Open in separate window", exact: true }).click();
  await page.waitForFunction(() => window.calls.filter(call => call.command === "viewer_layout").at(-1)?.args.viewport.detached === true);
  await page.getByRole("button", { name: "Return to workspace", exact: true }).first().click();
  await page.waitForFunction(() => window.calls.filter(call => call.command === "viewer_layout").at(-1)?.args.viewport.detached === false);
  assert.equal(await page.evaluate(() => window.calls.filter(call => call.command === "connect_screen").length), 1, "Pop out and return preserve the session");
  await page.evaluate(() => window.emit({ kind: "viewer_geometry", id: "01", width: 1080, height: 1920 }));
  await page.waitForFunction(() => { const r = document.querySelector(".viewport-active").getBoundingClientRect(); return Math.abs(r.width / r.height - 1080 / 1920) < .01; });
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
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("button", { name: "Dark", exact: true }).click();
  await page.waitForFunction(() => document.documentElement.dataset.theme === "dark" && !document.documentElement.classList.contains("theme-transitioning"));
  assert.equal(await page.evaluate(() => localStorage.getItem("nexus.theme")), "dark");
  await page.screenshot({ animations: "disabled", path: "../../.impeccable/review/windows-settings-dark.png" });
  await page.getByRole("button", { name: "Light", exact: true }).click();
  await page.waitForFunction(() => document.documentElement.dataset.theme === "light" && !document.documentElement.classList.contains("theme-transitioning"));
  await page.getByRole("switch", { name: "Lockdown mode" }).click();
  await page.waitForFunction(() => document.querySelector('[role="switch"]').getAttribute("aria-checked") === "true");
  await page.keyboard.press("Escape");
  assert.equal(await page.getByText("Search commands", { exact: true }).count(), 0);
  assert.equal(await page.getByRole("button", { name: "Phone", exact: true }).count(), 1);
  assert.deepEqual(errors, []);
  console.log("Workspace checks passed: glass with Windows transparency off, persisted material, caption state and narrow-window controls, native bounds, quality, fill, sidebar, dialog visibility, fullscreen, disconnect, keyboard/context menu, offline removal entry, failure/retry and viewer exit.");
} finally { await browser.close(); await server.close(); }
