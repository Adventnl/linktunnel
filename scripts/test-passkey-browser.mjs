// Actual browser WebAuthn ceremonies against the verifier; temporary virtual authenticator only.
// Run after: cargo build -p nexus-auth --example passkey_smoke; (cd apps/nexus-app && npm ci)
import { chromium } from '../apps/nexus-app/node_modules/playwright-core/index.mjs';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdir } from 'node:fs/promises';
import { once } from 'node:events';
const child = spawn('./target/debug/examples/passkey_smoke', [], { stdio: ['ignore','pipe','pipe'] });
const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
let errors = ''; child.stderr.on('data', d => errors += d);
const exit = once(child, 'exit');
const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
try {
  const context = await browser.newContext({ viewport: { width: 1000, height: 720 } });
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  await cdp.send('WebAuthn.enable');
  await cdp.send('WebAuthn.addVirtualAuthenticator', { options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true } });
  await mkdir('/tmp/nexus-auth-qa', { recursive: true });
  const first = await lines.next();
  if (first.done || !first.value.startsWith('http://localhost:')) throw Error(errors || 'missing prompt');
  await page.goto(first.value);
  await page.getByRole('button', { name: 'Create passkey' }).waitFor({ state: 'visible' });
  await page.screenshot({ path: '/tmp/nexus-auth-qa/desktop.png' });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: '/tmp/nexus-auth-qa/mobile.png' });
  await page.getByRole('button', { name: 'Create passkey' }).click();
  const second = await lines.next();
  if (second.done || !second.value.startsWith('http://localhost:')) throw Error(errors || 'registration verification failed');
  await page.goto(second.value);
  await page.getByRole('button', { name: 'Use passkey' }).click();
  const result = await lines.next();
  if (!result.value?.includes('authentication verified')) throw Error(errors || 'authentication did not verify');
  const [code] = await exit;
  if (code !== 0) throw Error(errors);
  console.log('Browser registration and authentication verified; desktop/mobile screenshots in /tmp/nexus-auth-qa.');
} finally { child.kill(); await browser.close(); }
