import { readFile, writeFile, rename, realpath, stat } from 'node:fs/promises';
import { join, relative, isAbsolute } from 'node:path';

const MAX_TEXT = 64_000;
const MAX_TASK_TEXT = 160_000;
const ACTIVE = new Set(['starting', 'inProgress', 'waiting']);
const APPROVALS = new Set(['item/commandExecution/requestApproval', 'item/fileChange/requestApproval']);
const text = value => String(value ?? '').slice(-MAX_TEXT);

function trimItems(task) {
  task.items = task.items.slice(-200);
  let remaining = MAX_TASK_TEXT;
  for (let i = task.items.length - 1; i >= 0; i--) {
    const item = task.items[i];
    item.text = item.text.slice(-Math.max(0, remaining));
    if (remaining === 0) item.text = '';
    remaining = Math.max(0, remaining - item.text.length);
  }
}

export class TaskError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}

/** Explicit repository allowlist; a phone never supplies a filesystem path or command. */
export async function repositories(config) {
  const root = await realpath(config.repositoryRoot);
  const ids = new Set();
  return Promise.all(config.repositories.map(async repo => {
    if (!/^[a-zA-Z0-9_-]{1,64}$/.test(repo.id) || ids.has(repo.id)) throw new Error('Invalid or duplicate repository ID.');
    ids.add(repo.id);
    const path = await realpath(join(root, repo.directory));
    const rel = relative(root, path);
    if (rel === '..' || rel.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || isAbsolute(rel)) {
      throw new Error('Repository escapes repositoryRoot.');
    }
    await stat(join(path, '.git'));
    return { id: repo.id, name: repo.name || repo.id, path };
  }));
}

export class Tasks {
  tasks = new Map();
  busy = new Set();
  loaded = new Set();
  saving = Promise.resolve();

  constructor(rpc, repos, stateFile) {
    this.rpc = rpc; this.repos = repos; this.stateFile = stateFile;
    rpc.on('message', message => this.event(message));
    rpc.on('unavailable', () => {
      for (const task of this.tasks.values()) if (ACTIVE.has(task.status)) {
        task.status = 'failed'; task.error = 'Codex stopped. Restart the mobile host.'; task.approvals = [];
      }
      this.persist();
    });
  }

  async restore() {
    let saved;
    try { saved = JSON.parse(await readFile(this.stateFile, 'utf8')); }
    catch (error) { if (error.code === 'ENOENT') return; throw error; }
    for (const task of saved) {
      if (ACTIVE.has(task.status)) task.status = 'interrupted';
      task.approvals = [];
      if (this.repos.some(repo => repo.id === task.repoId)) this.tasks.set(task.id, task);
    }
  }

  persist() {
    // Serialize writes and coalesce stream deltas. A disconnect does not cancel the PC task.
    if (this.saveTimer) return;
    this.saveTimer = setTimeout(() => { this.saveTimer = undefined; this.flush(); }, 250);
    this.saveTimer.unref();
  }

  flush() {
    clearTimeout(this.saveTimer); this.saveTimer = undefined;
    const data = JSON.stringify([...this.tasks.values()]);
    this.saving = this.saving.then(async () => {
      await writeFile(`${this.stateFile}.tmp`, data, { mode: 0o600 });
      await rename(`${this.stateFile}.tmp`, this.stateFile);
      this.storageError = false;
    }).catch(() => { this.storageError = true; });
    return this.saving;
  }

  get(id) {
    const task = this.tasks.get(id);
    if (!task) throw new TaskError(404, 'Conversation not found.');
    return task;
  }

  list() {
    return [...this.tasks.values()].sort((a, b) => b.updatedAt - a.updatedAt).map(({ id, repoId, title, status, updatedAt }) => ({ id, repoId, title, status, updatedAt }));
  }

  hasActiveTasks() { return [...this.tasks.values()].some(task => ACTIVE.has(task.status)); }

  async start(repoId, prompt) {
    const repo = this.repos.find(repo => repo.id === repoId);
    if (!repo) throw new TaskError(400, 'Choose a configured repository.');
    if (this.tasks.size >= 100) throw new TaskError(409, 'Conversation limit reached. Archive host state locally before starting more.');
    this.validatePrompt(prompt);
    const { thread } = await this.rpc.request('thread/start', {
      cwd: repo.path, approvalPolicy: 'on-request', sandbox: 'workspace-write',
    });
    const task = { id: thread.id, repoId, title: prompt.slice(0, 100), status: 'idle', items: [], approvals: [], diff: '', updatedAt: Date.now() };
    this.tasks.set(task.id, task); this.loaded.add(task.id);
    await this.turn(task.id, prompt);
    return task;
  }

  validatePrompt(prompt) {
    if (typeof prompt !== 'string' || !prompt.trim() || prompt.length > 16_000) throw new TaskError(400, 'Prompt must contain 1–16000 characters.');
  }

  async turn(id, prompt) {
    this.validatePrompt(prompt);
    const task = this.get(id);
    if (this.busy.has(id) || ACTIVE.has(task.status)) throw new TaskError(409, 'Wait for this task or stop it before sending a follow-up.');
    this.busy.add(id);
    task.status = 'starting'; task.turnId = null; task.error = ''; task.diff = ''; this.persist();
    try {
      const repo = this.repos.find(repo => repo.id === task.repoId);
      if (!repo) throw new TaskError(400, 'Repository is no longer configured.');
      if (!this.loaded.has(id)) {
        await this.rpc.request('thread/resume', { threadId: id, cwd: repo.path, approvalPolicy: 'on-request', sandbox: 'workspace-write' });
        this.loaded.add(id);
      }
      task.items.push({ id: `prompt-${Date.now()}`, type: 'userMessage', text: prompt });
      trimItems(task);
      const { turn } = await this.rpc.request('turn/start', { threadId: id, input: [{ type: 'text', text: prompt }] });
      task.turnId = turn.id;
      // A short turn may complete before the request response reaches us.
      if (task.status === 'starting') task.status = turn.status;
    } catch (error) {
      task.status = 'failed'; task.error = error.message; throw error;
    } finally { this.busy.delete(id); this.persist(); }
    return task;
  }

  async interrupt(id) {
    const task = this.get(id);
    if (!task.turnId || !ACTIVE.has(task.status)) throw new TaskError(409, 'No running turn.');
    await this.rpc.request('turn/interrupt', { threadId: id, turnId: task.turnId });
    return task;
  }

  approve(id, requestId, decision) {
    const task = this.get(id);
    const approval = task.approvals.find(a => String(a.id) === String(requestId));
    if (!approval || !APPROVALS.has(approval.method)) throw new TaskError(404, 'Approval expired.');
    if (!['accept', 'decline'].includes(decision)) throw new TaskError(400, 'Invalid approval decision.');
    this.rpc.send({ id: approval.id, result: { decision } });
    task.approvals = task.approvals.filter(a => a !== approval);
    task.status = task.approvals.length ? 'waiting' : 'inProgress'; this.persist();
    return task;
  }

  answer(id, requestId, answers) {
    const task = this.get(id);
    const request = task.approvals.find(a => String(a.id) === String(requestId) && a.method === 'item/tool/requestUserInput');
    if (!request) throw new TaskError(404, 'Question expired.');
    const result = Object.create(null);
    for (const question of request.questions) {
      const value = answers?.[question.id];
      if (typeof value !== 'string' || !value.trim() || value.length > 16_000) throw new TaskError(400, 'Answer every question.');
      result[question.id] = { answers: [value] };
    }
    this.rpc.send({ id: request.id, result: { answers: result } });
    task.approvals = task.approvals.filter(a => a !== request);
    task.status = task.approvals.length ? 'waiting' : 'inProgress'; this.persist();
    return task;
  }

  event(message) {
    const { method, params: p = {} } = message;
    const task = this.tasks.get(p.threadId);
    if (!task) {
      if (message.id !== undefined) this.rpc.send({ id: message.id, error: { code: -32601, message: 'Unsupported request.' } });
      return;
    }
    task.updatedAt = Date.now();
    if (message.id !== undefined) {
      if (method === 'item/tool/requestUserInput') {
        task.approvals.push({ id: message.id, method, questions: p.questions });
        if (p.isBlocking !== false) task.status = 'waiting';
      } else if (APPROVALS.has(method)) {
        task.approvals.push({ id: message.id, method, reason: text(p.reason), command: text(p.command), itemId: p.itemId });
        task.status = 'waiting';
      } else {
        // Never hang a turn on an unsupported interactive tool or silently approve it.
        this.rpc.send({ id: message.id, error: { code: -32601, message: 'This interaction is not supported by linktunnel. Send the answer as a follow-up.' } });
        task.error = 'Codex requested an unsupported interaction. Send the answer as a follow-up.';
      }
    } else if (method === 'turn/started') {
      task.turnId = p.turn.id; task.status = p.turn.status;
    } else if (method === 'turn/completed') {
      task.status = p.turn.status; task.error = p.turn.error?.message || task.error; task.approvals = [];
    } else if (method === 'turn/diff/updated') task.diff = text(p.diff);
    else if (method === 'error') task.error = text(p.error?.message);
    else if (method === 'serverRequest/resolved') {
      task.approvals = task.approvals.filter(a => a.id !== p.requestId);
      if (!task.approvals.length && task.status === 'waiting') task.status = 'inProgress';
    }
    else if (method === 'item/started' || method === 'item/completed') {
      const item = p.item;
      const view = { id: item.id, type: item.type, text: text(item.text || (item.type === 'fileChange' ? item.changes?.map(change => `${change.path}\n${change.diff || ''}`).join('\n') : '') || item.aggregatedOutput || item.command || item.content?.map(c => c.text || '').join('\n') || ''), status: item.status };
      if (item.type === 'userMessage') {
        const synthetic = task.items.findLast(i => i.id.startsWith('prompt-') && i.text === view.text);
        if (synthetic) synthetic.id = item.id;
      }
      const prior = task.items.find(i => i.id === item.id);
      if (prior) Object.assign(prior, view);
      else task.items.push(view);
    } else if (method === 'item/agentMessage/delta' || method === 'item/commandExecution/outputDelta') {
      let item = task.items.find(i => i.id === p.itemId);
      if (!item) { item = { id: p.itemId, type: method.includes('agentMessage') ? 'agentMessage' : 'commandExecution', text: '' }; task.items.push(item); }
      item.text = text(item.text + p.delta);
    }
    trimItems(task);
    this.persist();
  }
}
