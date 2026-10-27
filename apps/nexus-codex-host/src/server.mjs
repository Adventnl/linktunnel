import https from 'node:https';
import { createHash, timingSafeEqual } from 'node:crypto';
import { TaskError } from './tasks.mjs';

const digest = token => createHash('sha256').update(token).digest();
const MAX_BODY = 64 * 1024;

async function body(req) {
  let size = 0; const chunks = [];
  for await (const chunk of req) {
    size += chunk.length;
    if (size > MAX_BODY) throw new TaskError(413, 'Request too large.');
    chunks.push(chunk);
  }
  try {
    const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    if (!input || typeof input !== 'object' || Array.isArray(input)) throw new Error('Object required.');
    return input;
  }
  catch { throw new TaskError(400, 'Invalid JSON.'); }
}

export function handler(tasks, token) {
  const expected = digest(`Bearer ${token}`);
  return async (req, res) => {
    res.setHeader('Content-Type', 'application/json; charset=utf-8');
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('X-Content-Type-Options', 'nosniff');
    const reply = (status, data) => { res.writeHead(status); res.end(JSON.stringify(data)); };
    try {
      if (!timingSafeEqual(expected, digest(req.headers.authorization || ''))) throw new TaskError(401, 'Pair this phone again.');
      // Native client only. Refuse browser origins, including cross-origin mutation requests.
      if (req.headers.origin) throw new TaskError(403, 'Browser requests are disabled.');
      const url = new URL(req.url, 'https://localhost');
      const path = url.pathname.split('/').filter(Boolean);
      if (req.method === 'GET' && url.pathname === '/v1/repos') return reply(200, { repos: tasks.repos.map(({ id, name }) => ({ id, name })), storageError: !!tasks.storageError, powerError: !!tasks.powerError });
      if (req.method === 'GET' && url.pathname === '/v1/threads') return reply(200, { threads: tasks.list() });
      if (req.method === 'POST' && url.pathname === '/v1/threads') {
        const input = await body(req); return reply(201, await tasks.start(input.repoId, input.prompt));
      }
      if (path.length >= 3 && path[0] === 'v1' && path[1] === 'threads') {
        const id = decodeURIComponent(path[2]);
        if (req.method === 'GET' && path.length === 3) return reply(200, tasks.get(id));
        if (req.method === 'POST' && path.length === 4) {
          const input = await body(req);
          if (path[3] === 'turn') return reply(200, await tasks.turn(id, input.prompt));
          if (path[3] === 'interrupt') return reply(200, await tasks.interrupt(id));
          if (path[3] === 'approval') return reply(200, tasks.approve(id, input.requestId, input.decision));
          if (path[3] === 'answer') return reply(200, tasks.answer(id, input.requestId, input.answers));
        }
      }
      throw new TaskError(404, 'Endpoint not found.');
    } catch (error) {
      reply(error.status || 502, { error: error.status ? error.message : 'Codex could not finish the request. Check Codex sign-in and host configuration.' });
    }
  };
}

export function createServer(tasks, config, tls) {
  if (!/^[A-Za-z0-9_-]{40,}$/.test(config.token)) throw new Error('Use a random token with at least 40 characters.');
  if (config.bind !== '127.0.0.1' && !/^100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.(?:\d{1,3})\.(?:\d{1,3})$/.test(config.bind)) {
    throw new Error('Bind only to loopback or the PC Tailscale IPv4 address.');
  }
  const server = https.createServer({ ...tls, minVersion: 'TLSv1.2', maxHeaderSize: 8192 }, handler(tasks, config.token));
  server.requestTimeout = 40_000; server.headersTimeout = 10_000;
  server.maxConnections = 16;
  return server;
}
