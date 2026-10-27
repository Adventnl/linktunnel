import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';

const MAX_LINE = 8 * 1024 * 1024;

/** Private stdio connection. Never expose the raw Codex RPC socket to a phone. */
export class CodexRpc extends EventEmitter {
  pending = new Map();
  nextId = 1;
  buffer = '';
  closed = false;

  constructor(command, args = []) {
    super();
    this.child = spawn(command, [...args, 'app-server', '--listen', 'stdio://'], {
      windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'], shell: false,
    });
    this.child.stdout.setEncoding('utf8');
    this.child.stdout.on('data', chunk => this.read(chunk));
    // Codex diagnostics can contain local paths. Keep them off the network and logs.
    this.child.stderr.resume();
    this.child.on('error', () => this.fail(new Error('Codex could not start. Check codexCommand.')));
    this.child.on('exit', () => this.fail(new Error('Codex stopped. Restart the mobile host.')));
    this.child.stdin.on('error', () => this.fail(new Error('Codex input closed.')));
  }

  read(chunk) {
    this.buffer += chunk;
    for (let end; (end = this.buffer.indexOf('\n')) >= 0;) {
      const line = this.buffer.slice(0, end);
      this.buffer = this.buffer.slice(end + 1);
      if (line.length > MAX_LINE) return this.fail(new Error('Codex message too large.'));
      let message;
      try { message = JSON.parse(line); }
      catch { return this.fail(new Error('Invalid Codex protocol message.')); }
      if (message.method) this.emit('message', message);
      else {
        const pending = this.pending.get(message.id);
        if (!pending) continue;
        this.pending.delete(message.id);
        clearTimeout(pending.timer);
        if (message.error) pending.reject(new Error(message.error.message));
        else pending.resolve(message.result);
      }
    }
    if (this.buffer.length > MAX_LINE) this.fail(new Error('Codex message too large.'));
  }

  send(message) {
    if (this.closed) throw new Error('Codex is unavailable. Restart the mobile host.');
    this.child.stdin.write(`${JSON.stringify(message)}\n`);
  }

  request(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Codex did not answer ${method}.`));
      }, 30_000);
      this.pending.set(id, { resolve, reject, timer });
      try { this.send({ id, method, params }); }
      catch (error) { clearTimeout(timer); this.pending.delete(id); reject(error); }
    });
  }

  async initialize() {
    await this.request('initialize', {
      clientInfo: { name: 'nexus_mobile', title: 'Nexus Mobile', version: '0.1.0' },
    });
    this.send({ method: 'initialized' });
  }

  fail(error) {
    if (this.closed) return;
    this.closed = true;
    for (const { reject, timer } of this.pending.values()) { clearTimeout(timer); reject(error); }
    this.pending.clear();
    this.child.stdin.destroy();
    this.child.kill();
    this.emit('unavailable', error);
  }

  close() { this.fail(new Error('Mobile host stopped.')); }
}
