import { readFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { CodexRpc } from './rpc.mjs';
import { Tasks, repositories } from './tasks.mjs';
import { createServer } from './server.mjs';
import { PowerLease } from './power.mjs';

async function main() {
  const configFile = resolve(process.argv[2] || 'mobile-host.json');
  const config = JSON.parse((await readFile(configFile, 'utf8')).replace(/^\uFEFF/, ''));
  const repos = await repositories(config);
  const rpc = new CodexRpc(config.codexCommand, config.codexArgs || []);
  try {
    await rpc.initialize();
    const tasks = new Tasks(rpc, repos, resolve(dirname(configFile), 'tasks.json'));
    await tasks.restore();
    const tls = { key: await readFile(resolve(dirname(configFile), config.tlsKey)), cert: await readFile(resolve(dirname(configFile), config.tlsCert)) };
    const server = createServer(tasks, config, tls);
    const power = new PowerLease(tasks);
    server.on('error', () => { console.error('Mobile listener failed. Check its address and port.'); power.close(); rpc.close(); process.exitCode = 1; });
    server.listen(config.port || 9443, config.bind, () => console.log(`linktunnel host ready on https://${config.bind}:${config.port || 9443}`));
    const stop = async () => { power.close(); rpc.close(); server.close(); server.closeAllConnections(); await tasks.flush(); };
    process.once('SIGINT', stop); process.once('SIGTERM', stop);
  } catch (error) { rpc.close(); throw error; }
}

main().catch(() => { console.error('Mobile host could not start. Check configuration, repository paths and Codex installation.'); process.exitCode = 1; });
