// Renders the UI (with the mock bridge) in headless Chromium and saves screenshots to ./shots.
// Usage: npm run build && npm run screenshots
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import { chromium } from "playwright-core";

const PORT = 4173;
const out = new URL("../shots/", import.meta.url).pathname;
mkdirSync(out, { recursive: true });
const server = spawn("npx", ["vite", "preview", "--port", String(PORT), "--strictPort", "--host", "127.0.0.1"], { stdio: "ignore" });
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
for (let i = 0; i < 50; i++) {
  try { if ((await fetch(`http://127.0.0.1:${PORT}/`)).ok) break; } catch {}
  await wait(200);
}
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? "/opt/pw-browsers/chromium-1194/chrome-linux/chrome", args: ["--no-sandbox"] });
const shoot = async (name, { w = 1280, h = 800, scheme = "dark", lang = "en-US", steps = async () => {} } = {}) => {
  const ctx = await browser.newContext({ viewport: { width: w, height: h }, colorScheme: scheme, locale: lang });
  const page = await ctx.newPage();
  page.on("pageerror", (e) => console.error("pageerror", name, e.message));
  page.on("console", (m) => m.type() === "error" && console.error("console", name, m.text()));
  await page.goto(`http://127.0.0.1:${PORT}/`);
  await page.waitForSelector("article");
  await steps(page);
  await page.screenshot({ path: `${out}${name}.png` });
  await ctx.close();
  console.log("saved", name);
};
try {
  await shoot("dashboard-dark");
  await shoot("dashboard-light", { scheme: "light" });
  await shoot("dashboard-zh", { lang: "zh-CN" });
  await shoot("mobile", { w: 390, h: 844 });
  await shoot("palette", { steps: async (p) => { await p.keyboard.press("Control+k"); await p.keyboard.type("term"); await wait(150); } });
  await shoot("confirm", { steps: async (p) => { await p.locator("article", { hasText: "Windows PC" }).getByRole("button", { name: "More actions" }).click(); await p.getByRole("menuitem", { name: "Shut down" }).click(); await wait(100); } });
  await shoot("pairing", { steps: async (p) => { await p.getByRole("button", { name: /Pair a device/ }).first().click(); await p.getByRole("button", { name: "Show a code" }).click(); await wait(1700); } });
  await shoot("terminal", { w: 1400, steps: async (p) => { await p.locator("article", { hasText: "Windows PC" }).getByRole("button", { name: /Terminal/ }).click(); await wait(800); await p.keyboard.type("echo hello"); await wait(200); } });
  await shoot("settings", { steps: async (p) => { await p.getByRole("button", { name: "Settings" }).click(); await wait(100); } });
} finally {
  await browser.close();
  server.kill();
}
