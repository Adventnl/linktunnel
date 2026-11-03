import { chromium } from "playwright-core";
import { existsSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";

const out = fileURLToPath(new URL("../../../.impeccable/review/", import.meta.url));
mkdirSync(out, { recursive: true });
const server = await createServer({ root: fileURLToPath(new URL("../", import.meta.url)), configFile: fileURLToPath(new URL("../vite.config.ts", import.meta.url)), server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
await server.listen();
const executablePath = process.env.CHROMIUM ?? ["C:/Program Files/Google/Chrome/Application/chrome.exe", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser"].find(existsSync);
const browser = await chromium.launch({ executablePath, headless: true });
try {
  for (const [name, width, scheme] of process.argv.includes("--before") ? [["before", 1280, "light"]] : [["desktop", 1440, "light"], ["desktop-dark", 1280, "dark"], ["mobile", 390, "light"]]) {
    const page = await browser.newPage({ viewport: { width, height: 900 }, colorScheme: scheme });
    await page.addInitScript(theme => localStorage.setItem("nexus.theme", theme), scheme);
    await page.goto(server.resolvedUrls.local[0]);
    await page.getByRole("article").first().waitFor();
    await page.waitForTimeout(800);

    await page.screenshot({ animations: "disabled", path: `${out}${name}.png` });
    if (name === "desktop") {
      await page.getByRole("button", { name: "More actions", exact: true }).click();
      await page.screenshot({ animations: "disabled", path: `${out}context-menu.png` });
      await page.keyboard.press("Escape");
      await page.getByRole("button", { name: "Settings", exact: true }).click();
      await page.getByRole("dialog").waitFor();
      await page.screenshot({ animations: "disabled", path: `${out}settings.png` });
      await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).click();
      await page.getByRole("button", { name: "Pair a device", exact: true }).click();
      await page.getByRole("dialog").waitFor();
      await page.screenshot({ animations: "disabled", path: `${out}pairing.png` });
      await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).click();
      await page.getByRole("article").getByRole("button", { name: "Terminal", exact: true }).click();
      await page.waitForTimeout(800);
      await page.screenshot({ animations: "disabled", path: `${out}terminal.png` });
    }
    if (name === "mobile") {
      await page.getByRole("article").getByRole("button", { name: "Connect", exact: true }).click();
      await page.getByRole("alert").waitFor();
      await page.screenshot({ animations: "disabled", path: `${out}mobile-error.png` });
    }
    if (name === "desktop-dark") {
      await page.getByRole("article").getByRole("button", { name: "Terminal", exact: true }).click();
      await page.waitForTimeout(800);
      await page.screenshot({ animations: "disabled", path: `${out}terminal-dark.png` });
    }
    await page.close();
  }
} finally { await browser.close(); await server.close(); }
