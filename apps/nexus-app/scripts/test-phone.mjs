// Drive real React/bridge code with a controlled native boundary. Video remains native.
import assert from "node:assert/strict";
import { chromium } from "playwright-core";
import { existsSync, mkdirSync } from "node:fs";
import { createServer } from "vite";

const executablePath = process.env.CHROMIUM ?? [
  "C:/Program Files/Google/Chrome/Application/chrome.exe", "/usr/bin/chromium",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
].find(existsSync);
const server = await createServer({ server: { host: "127.0.0.1", port: 0, hmr: false, watch: { ignored: ["**/src-tauri/**", "**/shots/**"] } }, logLevel: "error" });
await server.listen();
const browser = await chromium.launch({ executablePath, headless: true });
mkdirSync("shots", { recursive: true });

async function setup(platform = "windows", missing = false) {
  const context = await browser.newContext({ viewport: { width: 1100, height: 760 } });
  const page = await context.newPage();
  page.setDefaultTimeout(12_000);
  page.setDefaultNavigationTimeout(60_000);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(({ platform, missing }) => {
    let session = { serial: null, error: null };
    let cancel;
    window.phoneCalls = [];
    window.phoneFail = false;
    window.phoneHang = false;
    window.callFailure = false;
    window.callReadFailure = false;
    window.callSnapshot = {
      available: true, bluetooth: true, packaged: true, callControl: true,
      devices: [{ id: "bt-redmi", name: "Redmi calls" }, { id: "bt-other", name: "Other Bluetooth phone" }],
      deviceId: null, canRoute: false, lines: [], calls: [],
      input: "PC microphone", output: "PC speakers", notice: null,
    };
    window.phoneDevices = missing || localStorage.getItem("phoneTestOffline") ? [] : [
      { serial: "redmi123", model: "Redmi Note 14 Pro", state: "device" },
      { serial: "other", model: "Other phone", state: "unauthorized" },
    ];
    window.phoneEnd = () => { session = { serial: null, error: "Phone disconnected" }; };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" } },
      transformCallback: () => 1, unregisterCallback() {},
      invoke: async (command, args) => {
        window.phoneCalls.push({ command, args });
        switch (command) {
          case "desktop_info": return { platform, embedded: platform === "windows", account: "Test account" };
          case "plugin:window|is_maximized": return false;
          case "plugin:event|listen": return 1;
          case "plugin:event|unlisten": return;
          case "viewer_layout": case "viewer_close": case "window_action": return;
          case "snapshot": return { agent: "unreachable", status: null, devices: [], sessions: [], error: null };
          case "phone_status": return { platform, adb: missing ? { Err: "adb not installed" } : { Ok: "adb 1.0.41" }, scrcpy: { Ok: "scrcpy 5.0.1" }, session };
          case "phone_devices": return window.phoneDevices;
          case "phone_session": return session;
          case "phone_start":
            session = { serial: null, error: null, starting: true };
            if (window.phoneHang) await new Promise((_, reject) => { cancel = () => reject(new Error("Phone connection cancelled.")); });
            else await new Promise(resolve => setTimeout(resolve, 150));
            if (window.phoneFail) { session = { serial: null, error: null }; throw new Error("Could not receive a phone picture"); }
            session = { serial: args.options.serial, error: null, width: 720, height: 1600 };
            return;
          case "phone_stop": cancel?.(); cancel = null; session = { serial: null, error: null }; return;
          case "phone_pair": case "phone_connect": case "phone_disconnect": return;
          case "phone_calls_status":
            if (window.callReadFailure) throw new Error("Bluetooth state unavailable");
            return structuredClone(window.callSnapshot);
          case "phone_calls_connect":
            if (window.callFailure) throw new Error("Windows denied call access");
            window.callSnapshot.deviceId = args.deviceId;
            window.callSnapshot.canRoute = true;
            window.callSnapshot.lines = [{ id: "redmi-sim-2", name: "SIM 2", canDial: true }];
            return structuredClone(window.callSnapshot);
          case "phone_calls_disconnect":
            window.callSnapshot.deviceId = null;
            window.callSnapshot.canRoute = false;
            window.callSnapshot.lines = []; window.callSnapshot.calls = [];
            return structuredClone(window.callSnapshot);
          case "phone_calls_action":
            if (window.callFailure) throw new Error("Call operation failed; check the existing call");
            if (args.action.kind === "dial") window.callSnapshot.calls = [{ id: "live-call", state: "dialing", onComputer: false, muted: false }];
            if (args.action.kind === "takeAudio" || args.action.kind === "answer") {
              window.callSnapshot.calls[0].state = "talking";
              window.callSnapshot.calls[0].onComputer = true;
            }
            if (args.action.kind === "mute") window.callSnapshot.calls[0].muted = args.action.muted;
            if (args.action.kind === "end") window.callSnapshot.calls = [];
            return structuredClone(window.callSnapshot);
          case "phone_calls_settings": return;
          default: throw new Error(`Unexpected command: ${command}`);
        }
      },
    };
  }, { platform, missing });
  await page.goto(server.resolvedUrls.local[0]);
  return { context, page, errors };
}

try {
  const { context, page, errors } = await setup();
  console.log("Checking shared device list and connection");
  const overview = page.getByRole("article");
  await overview.getByRole("heading", { name: "Redmi Note 14 Pro" }).waitFor();
  const sidebar = page.getByRole("complementary", { name: "Your devices" });
  assert.equal(await sidebar.getByText("Redmi Note 14 Pro", { exact: true }).count(), 1);
  assert.equal(await overview.getByRole("button", { name: "Terminal", exact: true }).count(), 0);
  assert.equal(await overview.getByLabel("Keep phone display off").isChecked(), false);
  await page.screenshot({ path: "shots/phone-workspace-light.png" });

  // Connection starts in the normal workspace and sends physical native bounds.
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await page.getByRole("button", { name: "Disconnect", exact: true }).waitFor();
  await page.waitForFunction(() => window.phoneCalls.some(c => c.command === "phone_session"));
  const start = await page.evaluate(() => window.phoneCalls.find(c => c.command === "phone_start"));
  assert.deepEqual(start.args.options, { serial: "redmi123", screenOff: false, mediaAudio: false, maxSize: 1600, maxFps: 60 });
  assert.ok(start.args.viewport.visible && !start.args.viewport.detached);
  assert.equal(await page.getByRole("dialog").count(), 0);
  await page.waitForFunction(() => { const r = document.querySelector(".viewport-active").getBoundingClientRect(); return Math.abs(r.width / r.height - 720 / 1600) < .01; });
  await page.getByRole("button", { name: "Open in separate window", exact: true }).click();
  await page.getByText("Phone is open in its own window", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Return to workspace", exact: true }).first().click();
  assert.equal(await page.evaluate(() => window.phoneCalls.filter(c => c.command === "phone_start").length), 1);
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await overview.waitFor();

  // Failures show the actual reason beside a working retry button.
  console.log("Checking failure, cancellation and timeout");
  await page.evaluate(() => { window.phoneFail = true; });
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await overview.getByRole("alert").filter({ hasText: "Could not receive a phone picture" }).waitFor();
  await page.evaluate(() => { window.phoneFail = false; window.phoneHang = true; });
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await page.getByText("Waiting for the first picture.", { exact: false }).waitFor();
  await page.screenshot({ path: "shots/phone-workspace-startup.png" });
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await overview.getByRole("button", { name: "Connect", exact: true }).waitFor({ state: "visible" });
  await page.waitForFunction(() => !document.querySelector('[aria-busy="true"]'));

  // A hung bridge cannot leave an infinite spinner; timeout also stops the process.
  await page.clock.install();
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await page.clock.fastForward(45_100);
  await overview.getByRole("alert").filter({ hasText: "Phone connection timed out" }).waitFor();
  await page.clock.resume();
  await page.evaluate(() => { window.phoneHang = false; });
  await overview.getByRole("button", { name: "Connect", exact: true }).click();
  await page.getByRole("button", { name: "Disconnect", exact: true }).waitFor();
  await page.evaluate(() => window.phoneEnd());
  await overview.getByRole("alert").filter({ hasText: "Phone disconnected" }).waitFor();

  // Offline phones remain in the same list and never offer desktop-only actions.
  console.log("Checking offline history and setup handoff");
  await page.evaluate(() => { window.phoneDevices = []; });
  await overview.getByText(/Phone is offline/).waitFor();
  assert.equal(await overview.getByRole("button", { name: "Wake", exact: true }).count(), 0);
  await page.evaluate(() => localStorage.setItem("phoneTestOffline", "true"));
  await page.reload();
  await overview.getByText(/Phone is offline/).waitFor();
  await page.evaluate(() => { localStorage.removeItem("phoneTestOffline"); window.phoneDevices = [{ serial: "redmi123", model: "Redmi Note 14 Pro", state: "device" }]; });
  await overview.getByRole("button", { name: "Connect", exact: true }).waitFor();

  // Pairing stays available without the agent; codes clear after use.
  await page.getByRole("button", { name: "Phone", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Phone", exact: true });
  await dialog.getByText("scrcpy: scrcpy 5.0.1").waitFor({ state: "attached" });
  await dialog.getByText("Wireless pairing (Android 11+)", { exact: true }).click();
  await dialog.getByLabel("Pairing IP address:port").fill("192.168.1.20:37123");
  await dialog.getByLabel("Six-digit pairing code").fill("123456");
  await dialog.getByRole("button", { name: "Pair phone", exact: true }).click();
  await dialog.getByText(/Paired. Now enter/).waitFor();
  assert.equal(await dialog.getByLabel("Six-digit pairing code").inputValue(), "");
  await dialog.getByLabel("Connection IP address:port").fill("192.168.1.20:40221");
  await dialog.getByRole("button", { name: "Connect wirelessly", exact: true }).click();
  await dialog.getByRole("button", { name: "Open phone", exact: true }).click();
  await page.getByRole("button", { name: "Disconnect", exact: true }).waitFor();
  assert.equal(await dialog.count(), 0);
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();

  console.log("Checking native cellular call selection, routing, errors and lifecycle");
  await page.getByRole("button", { name: "Phone", exact: true }).click();
  const calls = page.getByRole("region", { name: "Cellular calls on this computer" });
  await calls.getByLabel("Bluetooth phone for calls").selectOption("bt-redmi");
  await page.evaluate(() => { window.callFailure = true; });
  await calls.getByRole("button", { name: "Connect call audio", exact: true }).click();
  await calls.getByRole("alert").filter({ hasText: "Windows denied call access" }).waitFor();
  // Successful status polling must not erase a failed explicit operation.
  await page.waitForTimeout(2800);
  await calls.getByRole("alert").filter({ hasText: "Windows denied call access" }).waitFor();
  await page.evaluate(() => { window.callFailure = false; });
  await calls.getByRole("button", { name: "Connect call audio", exact: true }).click();
  await calls.getByLabel("Phone line").waitFor();
  const callConnect = await page.evaluate(() => window.phoneCalls.filter(c => c.command === "phone_calls_connect").at(-1));
  assert.deepEqual(callConnect.args, { deviceId: "bt-redmi" });
  assert.equal(await calls.getByText("Call audio: this computer", { exact: true }).count(), 0);
  await calls.getByLabel("Phone number").fill("*#06#");
  assert.equal(await calls.getByRole("button", { name: "Call using PC audio", exact: true }).isDisabled(), true);
  await calls.getByLabel("Phone number").fill("+64 21 123 456");
  await calls.getByRole("button", { name: "Call using PC audio", exact: true }).click();
  await calls.getByText("Calling…", { exact: true }).waitFor();
  const dial = await page.evaluate(() => window.phoneCalls.filter(c => c.command === "phone_calls_action"));
  assert.deepEqual(dial.map(c => c.args.action), [{ kind: "dial", lineId: "redmi-sim-2", number: "+64 21 123 456" }]);
  assert.equal(await calls.getByLabel("Phone number").inputValue(), "");
  await calls.getByText("PC audio is not confirmed.", { exact: false }).waitFor();
  await calls.getByRole("button", { name: "Take audio on PC", exact: true }).click();
  await calls.getByText("Call audio: this computer", { exact: true }).waitFor();
  await calls.getByRole("button", { name: "Mute microphone", exact: true }).click();
  await calls.getByRole("button", { name: "Unmute microphone", exact: true }).waitFor();
  await calls.scrollIntoViewIfNeeded();
  await page.screenshot({ path: "shots/phone-calls-active.png" });
  await page.evaluate(() => { window.callReadFailure = true; });
  await calls.getByRole("alert").filter({ hasText: "Bluetooth state unavailable" }).waitFor();
  assert.equal(await calls.getByText("Call audio: this computer", { exact: true }).count(), 0);
  assert.equal(await calls.getByRole("button", { name: "End call", exact: true }).isDisabled(), true);
  await page.evaluate(() => { window.callReadFailure = false; });
  await calls.getByRole("button", { name: "Refresh call status", exact: true }).last().click();
  await calls.getByText("Call audio: this computer", { exact: true }).waitFor();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  assert.equal(await page.evaluate(() => window.phoneCalls.filter(c => c.command === "phone_calls_disconnect").length), 0);
  await page.getByRole("button", { name: "Phone", exact: true }).click();
  await calls.getByRole("button", { name: "End call", exact: true }).click();
  await page.evaluate(() => { window.callSnapshot.calls = [{ id: "incoming-call", state: "incoming", onComputer: false, muted: false }]; });
  await calls.getByRole("button", { name: "Answer on PC", exact: true }).click();
  await calls.getByText("Call audio: this computer", { exact: true }).waitFor();
  await calls.getByRole("button", { name: "Disconnect call audio", exact: true }).click();
  await calls.getByRole("button", { name: "Connect call audio", exact: true }).waitFor();
  await page.evaluate(() => { window.callSnapshot.callControl = false; });
  await calls.getByRole("button", { name: "Connect call audio", exact: true }).click();
  await calls.getByText("This Windows version can request calls", { exact: false }).waitFor();
  await calls.getByRole("button", { name: "Disconnect call audio", exact: true }).click();
  await page.evaluate(() => { window.callSnapshot.packaged = false; });
  await calls.getByText("Install the call-enabled Link Tunnel package", { exact: false }).waitFor();
  assert.equal(await calls.getByRole("button", { name: "Connect call audio", exact: true }).isDisabled(), true);
  await page.evaluate(() => { window.callSnapshot.bluetooth = false; });
  await calls.getByText("Windows is not exposing a Bluetooth adapter", { exact: false }).waitFor();
  assert.equal(await calls.getByRole("button", { name: "Connect call audio", exact: true }).isDisabled(), true);
  assert.equal(await page.evaluate(() => window.phoneCalls.filter(c => c.command === "phone_calls").length), 0);
  await page.setViewportSize({ width: 420, height: 700 });
  await calls.scrollIntoViewIfNeeded();
  await page.screenshot({ path: "shots/phone-calls-compact.png" });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await dialog.getByRole("button", { name: "Close", exact: true }).click();

  await page.setViewportSize({ width: 420, height: 700 });
  await page.screenshot({ path: "shots/phone-workspace-compact.png" });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.evaluate(() => { localStorage.setItem("nexus.theme", "dark"); localStorage.setItem("nexus.locale", "zh-CN"); });
  await page.reload();
  await overview.waitFor();
  await page.screenshot({ path: "shots/phone-workspace-dark-zh.png" });
  await page.evaluate(() => {
    window.callSnapshot.deviceId = "bt-redmi";
    window.callSnapshot.canRoute = true;
    window.callSnapshot.lines = [{ id: "redmi-sim-2", name: "SIM 2", canDial: true }];
    window.callSnapshot.calls = [{ id: "live-call", state: "talking", onComputer: false, muted: false }];
  });
  await page.getByRole("button", { name: "手机", exact: true }).click();
  const chineseCalls = page.getByRole("region", { name: "在此电脑上进行蜂窝通话" });
  await chineseCalls.getByText("尚未确认电脑音频。", { exact: false }).waitFor();
  await chineseCalls.scrollIntoViewIfNeeded();
  await page.screenshot({ path: "shots/phone-calls-dark-zh.png" });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(errors, []);
  await context.close();

  const mac = await setup("macos");
  await mac.page.getByRole("article").getByRole("button", { name: "Connect", exact: true }).click();
  await mac.page.getByText("Phone is open in its own window", { exact: true }).waitFor();
  assert.deepEqual(mac.errors, []);
  await mac.context.close();

  const missing = await setup("windows", true);
  await missing.page.getByRole("button", { name: "Phone", exact: true }).click();
  await missing.page.getByText("adb: adb not installed", { exact: true }).waitFor();
  assert.equal(await missing.page.getByRole("button", { name: "Open phone", exact: true }).isDisabled(), true);
  assert.deepEqual(missing.errors, []);
  await missing.context.close();
  console.log("Phone workspace checks passed: discovery, offline history, connect, cancel, timeout, retry, pairing, pop-out, Mac fallback, missing tools and narrow/CJK themes.");
} finally { await browser.close(); await server.close(); }
