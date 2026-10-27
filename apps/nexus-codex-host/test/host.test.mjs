import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import http from 'node:http';
import { once } from 'node:events';
import { Tasks, repositories } from '../src/tasks.mjs';
import { handler, createServer } from '../src/server.mjs';
import { CodexRpc } from '../src/rpc.mjs';
import { PowerLease } from '../src/power.mjs';

class FakeRpc extends EventEmitter {
  calls = []; sent = []; count = 0;
  async request(method, params) {
    this.calls.push({ method, params });
    if (method === 'thread/start') return { thread: { id: `thread-${++this.count}` } };
    if (method === 'turn/start') return { turn: { id: 'turn-1', status: 'inProgress' } };
    return {};
  }
  send(message) { this.sent.push(message); }
}

async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'nexus-mobile-'));
  const rpc = new FakeRpc();
  const tasks = new Tasks(rpc, [{ id: 'repo', name: 'Repo', path: directory }], join(directory, 'tasks.json'));
  t.after(async () => { await tasks.flush(); await rm(directory, { recursive: true, force: true }); });
  return { directory, rpc, tasks };
}

test('only configured repositories and bounded nonempty prompts can start work', async t => {
  const { tasks, rpc } = await fixture(t);
  await assert.rejects(tasks.start('../repo', 'hello'), { status: 400 });
  for (const prompt of ['', '  ', null, 'x'.repeat(16001)]) await assert.rejects(tasks.start('repo', prompt), { status: 400 });
  assert.equal(rpc.calls.length, 0);
  await tasks.start('repo', 'Fix the bug');
  assert.deepEqual(rpc.calls[0], { method: 'thread/start', params: { cwd: tasks.repos[0].path, approvalPolicy: 'on-request', sandbox: 'workspace-write' } });
});

test('repository policy rejects traversal and duplicate IDs', async t => {
  const { directory } = await fixture(t);
  await mkdir(join(directory, 'root')); await mkdir(join(directory, 'outside')); await mkdir(join(directory, 'outside', '.git'));
  await assert.rejects(repositories({ repositoryRoot: join(directory, 'root'), repositories: [{ id: 'outside', directory: '../outside' }] }), /escapes/);
  await mkdir(join(directory, 'root', 'repo')); await mkdir(join(directory, 'root', 'repo', '.git'));
  const configured = { repositoryRoot: join(directory, 'root'), repositories: [{ id: 'repo', directory: 'repo' }] };
  assert.equal((await repositories(configured))[0].id, 'repo');
  await assert.rejects(repositories({ ...configured, repositories: [...configured.repositories, ...configured.repositories] }), /duplicate/);
});

test('overlapping follow-ups are rejected and completed threads accept follow-ups', async t => {
  const { tasks, rpc } = await fixture(t); const task = await tasks.start('repo', 'first');
  await assert.rejects(tasks.turn(task.id, 'overlap'), { status: 409 });
  rpc.emit('message', { method: 'turn/completed', params: { threadId: task.id, turn: { id: task.turnId, status: 'completed' } } });
  await tasks.turn(task.id, 'second'); assert.equal(task.status, 'inProgress');
  await tasks.interrupt(task.id);
  assert.equal(rpc.calls.at(-1).method, 'turn/interrupt');
});

test('completion arriving before turn/start response is not overwritten', async t => {
  const { tasks, rpc } = await fixture(t);
  const original = rpc.request.bind(rpc);
  rpc.request = async (method, params) => {
    const response = await original(method, params);
    if (method === 'turn/start') rpc.emit('message', { method: 'turn/completed', params: { threadId: params.threadId, turn: { id: 'turn-1', status: 'completed' } } });
    return response;
  };
  assert.equal((await tasks.start('repo', 'fast')).status, 'completed');
});

test('approvals are scoped to a thread, explicit, and cannot be answered twice', async t => {
  const { tasks, rpc } = await fixture(t); const task = await tasks.start('repo', 'test');
  rpc.emit('message', { id: 41, method: 'item/commandExecution/requestApproval', params: { threadId: task.id, command: 'git status', reason: 'test' } });
  assert.equal(task.status, 'waiting');
  assert.throws(() => tasks.approve(task.id, 41, 'acceptForSession'), { status: 400 });
  assert.throws(() => tasks.approve(task.id, 42, 'accept'), { status: 404 });
  tasks.approve(task.id, 41, 'decline');
  assert.deepEqual(rpc.sent.at(-1), { id: 41, result: { decision: 'decline' } });
  assert.throws(() => tasks.approve(task.id, 41, 'accept'), { status: 404 });
  rpc.emit('message', { id: 42, method: 'unsupported/tool', params: { threadId: task.id } });
  assert.equal(rpc.sent.at(-1).error.code, -32601);
});

test('messages and diffs survive disconnects, with bounded output', async t => {
  const { tasks, rpc } = await fixture(t); const task = await tasks.start('repo', 'hello');
  for (let i = 0; i < 250; i++) rpc.emit('message', { method: 'item/agentMessage/delta', params: { threadId: task.id, itemId: `item-${i}`, delta: 'x'.repeat(1000) } });
  rpc.emit('message', { method: 'turn/diff/updated', params: { threadId: task.id, diff: 'patch' } });
  assert.ok(task.items.length <= 200); assert.ok(task.items.reduce((n, item) => n + item.text.length, 0) <= 160000);
  assert.equal(task.diff, 'patch');
  await tasks.flush();
  const restored = new Tasks(new FakeRpc(), tasks.repos, tasks.stateFile); await restored.restore();
  assert.equal(restored.get(task.id).status, 'interrupted'); assert.equal(restored.get(task.id).diff, 'patch');
  await restored.turn(task.id, 'continue');
  assert.equal(restored.rpc.calls[0].method, 'thread/resume'); await restored.flush();
});

test('Codex questions require all answers and cannot be mistaken for execution approvals', async t => {
  const { tasks, rpc } = await fixture(t); const task = await tasks.start('repo', 'test');
  rpc.emit('message', { id: 51, method: 'item/tool/requestUserInput', params: { threadId: task.id, isBlocking: true, questions: [{ id: 'choice', question: 'Which branch?' }] } });
  assert.equal(task.status, 'waiting');
  assert.throws(() => tasks.approve(task.id, 51, 'accept'), { status: 404 });
  assert.throws(() => tasks.answer(task.id, 51, {}), { status: 400 });
  tasks.answer(task.id, 51, { choice: 'main' });
  assert.equal(task.status, 'inProgress');
  assert.equal(JSON.stringify(rpc.sent.at(-1)), JSON.stringify({ id: 51, result: { answers: { choice: { answers: ['main'] } } } }));
});

test('RPC failure ends active tasks and drops unusable approvals', async t => {
  const { tasks, rpc } = await fixture(t); const task = await tasks.start('repo', 'test');
  rpc.emit('unavailable', new Error('gone'));
  assert.equal(task.status, 'failed'); assert.deepEqual(task.approvals, []);
});

test('persistence failures are visible rather than reported as saved', async t => {
  const { tasks, directory } = await fixture(t); tasks.stateFile = join(directory, 'missing', 'tasks.json');
  await tasks.flush(); assert.equal(tasks.storageError, true);
});

test('HTTPS listener refuses public binding and weak pairing tokens', async () => {
  assert.throws(() => createServer({}, { token: 'x'.repeat(43), bind: '0.0.0.0' }, {}), /Bind only/);
  assert.throws(() => createServer({}, { token: 'short', bind: '127.0.0.1' }, {}), /random token/);
});

test('HTTP boundary authenticates, refuses browser origins, and never exposes arbitrary RPC', async t => {
  const { tasks } = await fixture(t); const token = 't'.repeat(43);
  const server = http.createServer(handler(tasks, token)); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(() => { server.closeAllConnections(); server.close(); });
  const base = `http://127.0.0.1:${server.address().port}`;
  assert.equal((await fetch(`${base}/v1/repos`)).status, 401);
  const headers = { Authorization: `Bearer ${token}` };
  assert.equal((await fetch(`${base}/v1/repos`, { headers: { ...headers, Origin: 'https://evil.example' } })).status, 403);
  const repo = await (await fetch(`${base}/v1/repos`, { headers })).json();
  assert.deepEqual(repo.repos, [{ id: 'repo', name: 'Repo' }]); assert.equal(JSON.stringify(repo).includes(tasks.repos[0].path), false);
  assert.equal((await fetch(`${base}/rpc`, { method: 'POST', headers, body: '{}' })).status, 404);
  assert.equal((await fetch(`${base}/v1/threads`, { method: 'POST', headers, body: '{' })).status, 400);
  assert.equal((await fetch(`${base}/v1/threads`, { method: 'POST', headers, body: 'null' })).status, 400);
  assert.equal((await fetch(`${base}/v1/threads`, { method: 'POST', headers, body: 'x'.repeat(65000 + 1000) })).status, 413);
  const response = await fetch(`${base}/v1/threads`, { method: 'POST', headers, body: JSON.stringify({ repoId: 'repo', prompt: 'hello' }) });
  assert.equal(response.status, 201); const task = await response.json();
  assert.equal((await fetch(`${base}/v1/threads/${task.id}/turn`, { method: 'POST', headers, body: '{"prompt":"overlap"}' })).status, 409);
});

test('stdio transport initializes, correlates replies, and rejects pending calls on child exit', async t => {
  const { directory } = await fixture(t);
  const script = join(directory, 'fake.cjs');
  await writeFile(script, `const readline = require('node:readline'); readline.createInterface({input:process.stdin}).on('line', line => { const m=JSON.parse(line); if(m.method==='exit')process.exit(0); if(m.id!==undefined)process.stdout.write(JSON.stringify({id:m.id,result:{method:m.method}})+'\\n'); });`);
  const rpc = new CodexRpc(process.execPath, [script]); t.after(() => rpc.close());
  await rpc.initialize();
  assert.deepEqual(await rpc.request('ping'), { method: 'ping' });
  await assert.rejects(rpc.request('exit'), /stopped/);
});

test('Windows power helper remains alive for active work and releases at idle', { skip: process.platform !== 'win32', timeout: 10_000 }, async t => {
  let active = true;
  const tasks = { hasActiveTasks: () => active };
  const power = new PowerLease(tasks); clearInterval(power.timer); t.after(() => power.close());
  power.update(); const helper = power.child;
  await new Promise(resolve => setTimeout(resolve, 1200));
  assert.equal(helper.exitCode, null, 'Power helper must hold its request until stdin closes.');
  const exited = once(helper, 'exit'); active = false; power.update();
  const [code] = await exited;
  assert.equal(code, 0); assert.equal(tasks.powerError, undefined); assert.equal(power.child, undefined);
});
