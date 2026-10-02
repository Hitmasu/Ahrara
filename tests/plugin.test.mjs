import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { promises as fs } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { createServer, connect } from 'node:net';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { checkBinary, install, platform, release } from '../src/plugin/ahrara.mjs';
import opencode from '../.opencode/plugins/ahrara.mjs';

function fixture(version = '0.1.0', expires = Math.floor(Date.now() / 1000) + 3600) {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const key = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32).toString('hex');
  const binary = Buffer.from('synthetic executable, never run');
  const artifact = { url: 'https://github.com/Hitmasu/Ahrara/releases/download/v0.1.0/ahrara-linux-x86_64', sha256: createHash('sha256').update(binary).digest('hex'), size: binary.length };
  const envelope = (change = {}) => {
    const payload = Buffer.from(JSON.stringify({ schema_version: 1, expires_at: expires, channels: { direct: { version, artifacts: { 'linux-x86_64': artifact } } }, ...change }));
    return Buffer.from(JSON.stringify({ payload: payload.toString('base64'), signature: sign(null, Buffer.concat([Buffer.from('ahrara-release-v1\0'), payload]), privateKey).toString('base64') }));
  };
  return { key, binary, artifact, envelope };
}

test('six native targets; verify signatures, expiry, safe metadata and executable bytes', () => {
  for (const os of ['linux', 'darwin', 'win32']) for (const arch of ['x64', 'arm64']) assert.match(platform(os, arch), /^(linux|darwin|windows)-(x86_64|aarch64)$/);
  assert.throws(() => platform('linux', 'ia32'));
  const f = fixture();
  const bytes = f.envelope();
  const artifact = release(bytes, f.key, 'linux-x86_64');
  checkBinary(f.binary, artifact);
  assert.throws(() => checkBinary(Buffer.from('tampered'), artifact));
  assert.throws(() => release(bytes, null, 'linux-x86_64'));
  assert.throws(() => release(bytes, fixture().key, 'linux-x86_64'), /signature/);
  const changed = JSON.parse(bytes);
  changed.payload = Buffer.from('{}').toString('base64');
  assert.throws(() => release(Buffer.from(JSON.stringify(changed)), f.key, 'linux-x86_64'), /signature/);
  assert.throws(() => release(f.envelope({ expires_at: 1 }), f.key, 'linux-x86_64'), /expired/);
  assert.throws(() => release(bytes, f.key, 'windows-aarch64'), /platform/);
  for (const invalid of [
    { version: '../escape', artifacts: { 'linux-x86_64': f.artifact } },
    { version: '0.1.0', artifacts: { 'linux-x86_64': { ...f.artifact, url: 'http://example.test/binary' } } },
    { version: '0.1.0', artifacts: { 'linux-x86_64': { ...f.artifact, size: 0 } } },
  ]) assert.throws(() => release(f.envelope({ channels: { direct: invalid } }), f.key, 'linux-x86_64'));
});

test('concurrent installs publish complete binaries, reuse offline and reject corruption', async () => {
  const directory = await fs.mkdtemp(path.join(tmpdir(), 'ahrara-install-'));
  const f = fixture();
  let downloads = 0;
  const options = { directory, publicKey: f.key, target: 'linux-x86_64', fetch: async (url) => { downloads++; return url.endsWith('.json') ? f.envelope() : f.binary; } };
  try {
    const [a, b] = await Promise.all([install(options), install(options)]);
    assert.equal(a, b);
    assert.deepEqual(await fs.readFile(a), f.binary);
    assert.equal(JSON.parse(await fs.readFile(`${a}.install.json`)).method, 'plugin');
    const before = downloads;
    await fs.writeFile(path.join(directory, 'linux-x86_64.json'), f.envelope({ expires_at: 1 }));
    assert.equal(await install(options), a);
    assert.equal(downloads, before);
    const rollback = f.envelope({ channels: { direct: { version: '0.0.9', artifacts: { 'linux-x86_64': f.artifact } } } });
    await assert.rejects(install({ ...options, update: true, fetch: async () => rollback }), /rollback/);
    await fs.writeFile(a, 'corrupted');
    await assert.rejects(install(options), /checksum/);
    assert.equal(downloads, before);
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});

test('failed verification never publishes an executable or installation index', async () => {
  const directory = await fs.mkdtemp(path.join(tmpdir(), 'ahrara-invalid-install-'));
  const f = fixture();
  try {
    await assert.rejects(install({ directory, publicKey: f.key, target: 'linux-x86_64', fetch: async (url) => url.endsWith('.json') ? f.envelope() : Buffer.from('wrong') }), /checksum/);
    assert.deepEqual(await fs.readdir(directory), []);
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});

test('host manifests point at packaged resources and OpenCode preserves existing settings', async () => {
  const root = fileURLToPath(new URL('../', import.meta.url));
  const version = (await fs.readFile(path.join(root, 'src/cli/Cargo.toml'), 'utf8')).match(/^version = "([^"]+)"/m)[1];
  assert.equal((await fs.readFile(path.join(root, 'plugin.yaml'), 'utf8')).match(/^version: (.+)$/m)[1], version);
  for (const name of ['.claude-plugin/plugin.json', '.codex-plugin/plugin.json', '.github/plugin/plugin.json', '.devin-plugin/plugin.json', '.qoder-plugin/plugin.json', 'plugin.json', 'gemini-extension.json', 'package.json']) {
    const data = JSON.parse(await fs.readFile(path.join(root, name)));
    assert.equal(data.version, version, `${name} version`);
    for (const field of ['mcpServers', 'skills', 'contextFileName']) {
      if (typeof data[field] === 'string') await fs.access(path.resolve(root, data[field]));
    }
  }
  for (const name of ['src/plugin/claude-mcp.json', 'src/plugin/codex-mcp.json', 'gemini-extension.json']) {
    const { mcpServers } = JSON.parse(await fs.readFile(path.join(root, name)));
    const server = mcpServers.ahrara;
    assert.equal(server.command, 'node');
    const argument = server.args[0].replace('${CLAUDE_PLUGIN_ROOT}', root).replace('${extensionPath}', root);
    await fs.access(path.resolve(root, argument));
  }
  const hooks = await opencode();
  const config = { skills: { paths: ['/existing'] }, mcp: { other: { type: 'remote' } } };
  await hooks.config(config);
  await hooks.config(config);
  assert.equal(config.skills.paths.length, 2);
  assert.equal(config.mcp.other.type, 'remote');
  assert.equal(config.mcp.ahrara.command[0], process.execPath);
  const existing = { mcp: { ahrara: { type: 'remote', url: 'http://localhost:8787/mcp' } } };
  await hooks.config(existing);
  assert.equal(existing.mcp.ahrara.type, 'remote');
});

test('launcher speaks real MCP and releases its server after EOF', { skip: !process.env.AHRARA_TEST_BINARY, timeout: 25_000 }, async () => {
  const directory = await fs.mkdtemp(path.join(tmpdir(), 'ahrara-launcher-'));
  const socket = createServer();
  await new Promise((resolve) => socket.listen(0, '127.0.0.1', resolve));
  const port = socket.address().port;
  await new Promise((resolve) => socket.close(resolve));
  const config = path.join(directory, 'config.toml');
  await fs.writeFile(config, `server_url = "wss://localhost:1"\nidentity_path = "identity.key"\ndatabase_path = "inbox.db"\napi_token_path = "api.token"\napi_bind = "127.0.0.1:${port}"\ncheck_for_updates = false\n`);
  const child = spawn(process.execPath, [fileURLToPath(new URL('../src/plugin/ahrara.mjs', import.meta.url)), '--config', config], {
    env: { ...process.env, AHRARA_BINARY: process.env.AHRARA_TEST_BINARY },
    stdio: ['pipe', 'pipe', 'pipe'], signal: AbortSignal.timeout(20_000),
  });
  const exited = new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', (code) => resolve(code)); });
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  let id = 0;
  const write = (value) => child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', ...value })}\n`);
  const request = async (method, params) => {
    const wanted = ++id;
    write({ id: wanted, method, params });
    for (;;) {
      const { value, done } = await lines.next();
      assert.equal(done, false, 'launcher closed before answering MCP');
      const message = JSON.parse(value);
      if (message.id === wanted) { assert.equal(message.error, undefined); return message.result; }
    }
  };
  try {
    const initialized = await request('initialize', { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'launcher-test', version: '1' } });
    assert.equal(initialized.protocolVersion, '2025-03-26');
    write({ method: 'notifications/initialized' });
    const tools = await request('tools/list', {});
    assert.equal(tools.tools.length, 17);
    const address = await request('tools/call', { name: 'ahrara_create_address', arguments: { label: 'Launcher test' } });
    assert.notEqual(address.isError, true);
    assert.match(address.structuredContent.address, /@ahrara\.dev$/);
    child.stdin.end();
    assert.equal(await exited, 0);
    const reachable = () => new Promise((resolve) => {
      const client = connect(port, '127.0.0.1');
      client.once('connect', () => { client.destroy(); resolve(true); });
      client.once('error', () => resolve(false));
    });
    for (let attempt = 0; attempt < 40 && await reachable(); attempt++) await delay(50);
    assert.equal(await reachable(), false, 'agent-owned server is still running');
    await fs.access(path.join(directory, 'identity.key'));
    await fs.access(path.join(directory, 'inbox.db'));
  } finally {
    child.kill();
    await exited.catch(() => {});
    await fs.rm(directory, { recursive: true, force: true });
  }
});
