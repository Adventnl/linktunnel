import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

/** Keep Windows awake only while Codex has active work; never turn its display on. */
export class PowerLease {
  child = undefined;
  failed = false;
  constructor(tasks) {
    this.tasks = tasks;
    if (process.platform === 'win32') {
      this.timer = setInterval(() => this.update(), 1000);
      this.timer.unref();
    }
  }
  update() {
    if (this.failed) return;
    if (this.tasks.hasActiveTasks() && !this.child) {
      const child = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', fileURLToPath(new URL('./power-lease.ps1', import.meta.url))], { windowsHide: true, stdio: ['pipe', 'ignore', 'ignore'] });
      this.child = child;
      const failed = () => { this.failed = true; this.tasks.powerError = true; this.child = undefined; };
      child.on('error', failed);
      child.stdin.on('error', () => {}); // Closing an already-exited helper needs no retry.
      child.on('exit', code => {
        if (code !== 0 && this.child === child) failed();
        if (this.child === child) this.child = undefined;
      });
    } else if (!this.tasks.hasActiveTasks() && this.child) {
      this.child.stdin.end('\n'); this.child = undefined;
    }
  }
  close() {
    clearInterval(this.timer);
    this.child?.stdin.end('\n'); this.child = undefined;
  }
}
