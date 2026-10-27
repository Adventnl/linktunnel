// Optional integration gate: runs one tiny inference in a disposable repository.
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CodexRpc } from '../src/rpc.mjs';
import { Tasks } from '../src/tasks.mjs';
import { once } from 'node:events';

const directory = await mkdtemp(join(tmpdir(), 'nexus-codex-smoke-'));
await mkdir(join(directory, '.git'));
const rpc = new CodexRpc(process.execPath, [process.argv[2]]);
let tasks;
try {
  await rpc.initialize();
  const account = await rpc.request('account/read', { refreshToken: false });
  assert.ok(account.account, 'Sign in with codex login first.');
  tasks = new Tasks(rpc, [{ id: 'smoke', name: 'Smoke', path: directory }], join(directory, 'tasks.json'));
  const task = await tasks.start('smoke', 'Create nexus-mobile-smoke.txt in the current directory containing the single line NEXUS_MOBILE_SMOKE_OK. Do not modify any other files. Reply exactly NEXUS_MOBILE_SMOKE_OK when done.');
  async function completed() {
    const deadline = Date.now() + 90_000;
    while (['starting', 'inProgress', 'waiting'].includes(task.status) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 300));
    assert.equal(task.status, 'completed', task.error);
  }
  await completed();
  assert.ok(task.items.some(item => item.type === 'agentMessage' && item.text.trim() === 'NEXUS_MOBILE_SMOKE_OK'));
  assert.equal((await readFile(join(directory, 'nexus-mobile-smoke.txt'), 'utf8')).trim(), 'NEXUS_MOBILE_SMOKE_OK');
  await tasks.turn(task.id, 'Append NEXUS_MOBILE_FOLLOWUP_OK as a second line in nexus-mobile-smoke.txt. Do not modify any other files. Reply exactly NEXUS_MOBILE_FOLLOWUP_OK when done.');
  await completed();
  assert.equal((await readFile(join(directory, 'nexus-mobile-smoke.txt'), 'utf8')).trim().replaceAll('\r', ''), 'NEXUS_MOBILE_SMOKE_OK\nNEXUS_MOBILE_FOLLOWUP_OK');
  console.log('PASS: real Codex sign-in, workspace file creation, follow-up edit, streamed agent replies and completion.');
} finally {
  const exited = rpc.child.exitCode === null ? once(rpc.child, 'exit') : Promise.resolve();
  rpc.close(); await exited; if (tasks) await tasks.flush();
  await rm(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
