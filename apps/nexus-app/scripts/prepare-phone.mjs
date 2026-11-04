// Build-time only. End users receive these files in the installer; no runtime download.
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmod, cp, mkdir, mkdtemp, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const manifest = JSON.parse(await readFile(new URL('./phone-tools.json', import.meta.url), 'utf8'));
const destination = join(root, 'apps/nexus-app/src-tauri/resources/phone-tools');
const cache = join(root, 'target/phone-downloads');
function run(program, args) {
  const result = spawnSync(program, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${program}: ${result.stderr}`);
  return result.stdout;
}
const host = run('rustc', ['-vV']).match(/^host: (.+)$/m)?.[1];
const target = process.argv[2] || process.env.TAURI_ENV_TARGET_TRIPLE || host;
const targets = target === 'universal-apple-darwin'
  ? ['aarch64-apple-darwin', 'x86_64-apple-darwin'] : [target];
for (const triple of targets) {
  if (!manifest.archives[triple]) throw new Error(`No bundled phone tools for ${triple}; add a verified distribution before shipping this target.`);
}
await mkdir(cache, { recursive: true });
await mkdir(dirname(destination), { recursive: true });
const stage = await mkdtemp(join(dirname(destination), '.phone-tools-'));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
try {
  for (const triple of targets) {
    const [archive, sha256] = manifest.archives[triple];
    const saved = join(cache, archive);
    let bytes = await readFile(saved).catch(error => { if (error.code !== 'ENOENT') throw error; });
    if (!bytes || digest(bytes) !== sha256) {
      const url = `https://github.com/Genymobile/scrcpy/releases/download/v${manifest.version}/${archive}`;
      console.log(`Downloading bundled phone tools: ${archive}`);
      const response = await fetch(url, { signal: AbortSignal.timeout(180_000) });
      if (!response.ok) throw new Error(`Phone tools download failed: ${response.status} ${url}`);
      bytes = Buffer.from(await response.arrayBuffer());
      if (digest(bytes) !== sha256) throw new Error(`SHA-256 mismatch for ${archive}; refusing to package it.`);
      await writeFile(saved, bytes);
    }
    const extracted = await mkdtemp(join(cache, '.extract-'));
    try {
      if (archive.endsWith('.zip')) {
        if (process.platform === 'win32') {
          // Paths travel through environment variables rather than PowerShell interpolation.
          const result = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
            'Expand-Archive -LiteralPath $env:NEXUS_PHONE_ARCHIVE -DestinationPath $env:NEXUS_PHONE_EXTRACT'],
          { env: { ...process.env, NEXUS_PHONE_ARCHIVE: saved, NEXUS_PHONE_EXTRACT: extracted }, stdio: 'inherit' });
          if (result.error || result.status !== 0) throw result.error || new Error('Expand-Archive failed');
        } else run('unzip', ['-q', saved, '-d', extracted]);
      } else run('tar', ['-xzf', saved, '-C', extracted]);
      const contents = join(extracted, archive.replace(/\.tar\.gz$|\.zip$/, ''));
      const suffix = triple.includes('windows') ? '.exe' : '';
      // Validate the payload, including adb and the Android server. Keep adjacent DLLs,
      // images and upstream LICENSE; the portable distributions rely on that layout.
      for (const file of [`adb${suffix}`, `scrcpy${suffix}`, 'scrcpy-server', suffix ? 'LICENSE.txt' : 'LICENSE']) {
        await readFile(join(contents, file));
      }
      const folder = join(stage, triple);
      await cp(contents, folder, { recursive: true });
      if (!suffix) for (const file of ['adb', 'scrcpy']) await chmod(join(folder, file), 0o755);
      const identity = process.env.APPLE_SIGNING_IDENTITY || process.env.APP_SIGN_ID;
      if (identity && triple.includes('apple') && process.platform === 'darwin') {
        for (const file of ['adb', 'scrcpy']) run('codesign', ['--force', '--options', 'runtime', '--timestamp', '--sign', identity, join(folder, file)]);
      }
      await writeFile(join(folder, 'nexus-origin.json'), JSON.stringify({ source: manifest.source, archive, sha256 }, null, 2) + '\n');
    } finally { await rm(extracted, { recursive: true, force: true }); }
  }
  await rm(destination, { recursive: true, force: true });
  await rename(stage, destination);
  console.log(`Prepared bundled phone tools for ${targets.join(', ')}`);
} finally { await rm(stage, { recursive: true, force: true }); }
