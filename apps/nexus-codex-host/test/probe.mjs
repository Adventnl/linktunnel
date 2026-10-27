import https from 'node:https';
import { readFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { X509Certificate, createHash } from 'node:crypto';
import assert from 'node:assert/strict';

const configPath = resolve(process.argv[2]);
const config = JSON.parse((await readFile(configPath, 'utf8')).replace(/^\uFEFF/, ''));
const cert = await readFile(resolve(dirname(configPath), config.tlsCert));
const pairing = JSON.parse((await readFile(resolve(dirname(configPath), 'pairing.json'), 'utf8')).replace(/^\uFEFF/, ''));
assert.equal(createHash('sha256').update(new X509Certificate(cert).raw).digest('hex'), pairing.certificateSha256);
function get(path, token) {
  return new Promise((resolve, reject) => {
    https.get(`${pairing.endpoint}${path}`, { ca: cert, headers: token ? { Authorization: `Bearer ${token}` } : {} }, response => {
      let body = ''; response.setEncoding('utf8'); response.on('data', chunk => { body += chunk; });
      response.on('end', () => resolve({ status: response.statusCode, body: JSON.parse(body) }));
    }).on('error', reject);
  });
}
assert.equal((await get('/v1/repos')).status, 401);
const result = await get('/v1/repos', config.token);
assert.equal(result.status, 200); assert.ok(result.body.repos.length);
console.log(`PASS: TLS certificate/pairing pin, unauthenticated rejection, authenticated repository listing (${result.body.repos.length} repositories).`);
