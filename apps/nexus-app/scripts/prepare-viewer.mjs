// Package the native viewer with the dashboard; Finder does not inherit the shell's PATH.
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const binaries = fileURLToPath(new URL('../src-tauri/binaries/', import.meta.url));

function run(program, args) {
  const result = spawnSync(program, args, {
    cwd: root,
    stdio: ['ignore', 'pipe', 'inherit'],
    encoding: 'utf8',
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${program} failed (${result.status})`);
  return result.stdout;
}

const host = run('rustc', ['-vV']).match(/^host: (.+)$/m)?.[1];
if (!host) throw new Error('rustc did not report a host target');
const target = process.env.TAURI_ENV_TARGET_TRIPLE || host;
mkdirSync(binaries, { recursive: true });

function prepare(triple) {
  const args = ['build', '--locked', '--release', '-p', 'nexus-cli', '--message-format=json-render-diagnostics'];
  if (triple !== host) args.push('--target', triple);
  const artifacts = run('cargo', args).trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
  const executable = artifacts.findLast(message =>
    message.reason === 'compiler-artifact' && message.target?.name === 'nexus' && message.executable,
  )?.executable;
  if (!executable) throw new Error(`cargo did not produce the nexus viewer for ${triple}`);
  const destination = join(binaries, `nexus-${triple}${triple.includes('windows') ? '.exe' : ''}`);
  copyFileSync(executable, destination);
  chmodSync(destination, 0o755);
  console.log(`Prepared ${destination}`);
  return destination;
}

if (target === 'universal-apple-darwin') {
  const slices = ['aarch64-apple-darwin', 'x86_64-apple-darwin'].map(prepare);
  run('lipo', ['-create', ...slices, '-output', join(binaries, `nexus-${target}`)]);
} else {
  prepare(target);
}
