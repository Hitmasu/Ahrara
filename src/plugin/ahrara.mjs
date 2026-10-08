#!/usr/bin/env node
// Shared installer and launcher. Node built-ins only; no package installation hooks.
import { createHash, createPublicKey, verify } from 'node:crypto';
import { execFile, spawn } from 'node:child_process';
import { constants, promises as fs } from 'node:fs';
import { get } from 'node:https';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execute = promisify(execFile);
const root = path.dirname(fileURLToPath(import.meta.url));
const manifestURL = 'https://github.com/Hitmasu/Ahrara/releases/latest/download/stable.json';
const manifestLimit = 128 * 1024;
const binaryLimit = 256 * 1024 * 1024;

export function platform(os = process.platform, arch = process.arch) {
  const system = { linux: 'linux', darwin: 'darwin', win32: 'windows' }[os];
  const cpu = { x64: 'x86_64', arm64: 'aarch64' }[arch];
  if (!system || !cpu) throw new Error('Ahrara supports Linux, macOS and Windows on x64 and ARM64.');
  return `${system}-${cpu}`;
}

function httpsURL(value) {
  const url = new URL(value);
  if (url.protocol !== 'https:' || url.username || url.password || url.hash) {
    throw new Error('Release URLs must use HTTPS without credentials or fragments.');
  }
  return url;
}

export function release(bytes, publicKey, target) {
  if (bytes.length > manifestLimit) throw new Error('Release manifest is too large.');
  if (!/^[0-9a-f]{64}$/i.test(publicKey ?? '')) {
    throw new Error('This plugin has no release verification key. The maintainer must publish src/plugin/release-key.json first.');
  }
  const envelope = JSON.parse(bytes);
  const decode = (value) => {
    if (typeof value !== 'string') throw new Error('Invalid release encoding.');
    const buffer = Buffer.from(value, 'base64');
    if (buffer.toString('base64') !== value) throw new Error('Invalid release encoding.');
    return buffer;
  };
  const payload = decode(envelope.payload);
  const key = createPublicKey({
    key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), Buffer.from(publicKey, 'hex')]),
    format: 'der', type: 'spki',
  });
  if (!verify(null, Buffer.concat([Buffer.from('ahrara-release-v1\0'), payload]), key, decode(envelope.signature))) {
    throw new Error('Release signature verification failed.');
  }
  const manifest = JSON.parse(payload);
  if (manifest.schema_version !== 1) throw new Error('Invalid release manifest.');
  const selected = manifest.channels?.direct;
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(selected?.version ?? '')) {
    throw new Error('Invalid stable release version.');
  }
  const artifact = selected.artifacts?.[target];
  if (!artifact || !/^[0-9a-f]{64}$/i.test(artifact.sha256 ?? '') ||
      !Number.isSafeInteger(artifact.size) || artifact.size <= 0 || artifact.size > binaryLimit) {
    throw new Error('No valid release binary for this platform.');
  }
  httpsURL(artifact.url);
  return { ...artifact, version: selected.version };
}

export function checkBinary(bytes, artifact) {
  if (bytes.length !== artifact.size || createHash('sha256').update(bytes).digest('hex') !== artifact.sha256.toLowerCase()) {
    throw new Error('Release binary checksum or size does not match.');
  }
}

export function download(value, limit, redirects = 0) {
  return new Promise((resolve, reject) => {
    const request = get(httpsURL(value), { headers: { 'User-Agent': 'Ahrara-Plugin' } }, async (response) => {
      try {
        if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
          response.resume();
          if (redirects >= 5 || !response.headers.location) throw new Error('Too many release redirects.');
          resolve(download(new URL(response.headers.location, value).href, limit, redirects + 1));
          return;
        }
        if (response.statusCode !== 200) throw new Error(`Release download failed (HTTP ${response.statusCode}).`);
        const chunks = [];
        let size = 0;
        for await (const chunk of response) {
          size += chunk.length;
          if (size > limit) throw new Error('Release download is too large.');
          chunks.push(chunk);
        }
        resolve(Buffer.concat(chunks));
      } catch (error) {
        response.destroy();
        reject(error);
      }
    });
    // Binaries may be large on slow links: fail stalled sockets, and keep a
    // total deadline only for the small manifest.
    request.setTimeout(30_000, () => request.destroy(new Error('Release download stalled.')));
    if (limit <= manifestLimit) {
      const deadline = setTimeout(() => request.destroy(new Error('Release download timed out.')), 120_000);
      request.on('close', () => clearTimeout(deadline));
    }
    request.on('error', reject);
  });
}

// Spawning a bare name on Windows runs a planted ahrara.exe from the current
// directory before PATH, and agents often start in an untrusted repository.
// Resolve absolute PATH entries only.
export async function command(name, search = process.env.PATH ?? '') {
  const file = process.platform === 'win32' ? `${name}.exe` : name;
  for (const directory of search.split(path.delimiter).filter((entry) => path.isAbsolute(entry))) {
    try {
      await fs.access(path.join(directory, file), constants.X_OK);
      return path.join(directory, file);
    } catch {}
  }
}

async function compatible(binary) {
  try {
    const { stdout } = await execute(binary, ['mcp', '--help'], { timeout: 5000, maxBuffer: 64 * 1024, windowsHide: true });
    return stdout.includes('--transport') && stdout.includes('stdio');
  } catch { return false; }
}

function cacheDirectory() {
  const data = process.platform === 'win32'
    ? process.env.LOCALAPPDATA || path.join(homedir(), 'AppData', 'Local')
    : process.platform === 'darwin'
      ? path.join(homedir(), 'Library', 'Application Support')
      : process.env.XDG_DATA_HOME || path.join(homedir(), '.local', 'share');
  return path.join(data, 'ahrara', 'agent');
}

export async function install({ directory = cacheDirectory(), publicKey, target = platform(), fetch = download, update = false } = {}) {
  publicKey ??= JSON.parse(await fs.readFile(path.join(root, 'release-key.json'), 'utf8')).ed25519;
  if (!/^[0-9a-f]{64}$/i.test(publicKey ?? '')) {
    throw new Error('Automatic installation is not published yet: src/plugin/release-key.json needs the public release key.');
  }
  const index = path.join(directory, `${target}.json`);
  const name = target.startsWith('windows-') ? 'ahrara.exe' : 'ahrara';
  const location = (artifact) => path.join(directory, `${artifact.version}-${target}-${artifact.sha256.toLowerCase()}`, name);
  let previous;
  try {
    const bytes = await fs.readFile(index);
    previous = release(bytes, publicKey, target);
    checkBinary(await fs.readFile(location(previous)), previous);
    if (!update) return location(previous); // Installed binaries also work offline after manifest expiry.
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  const envelope = await fetch(manifestURL, manifestLimit);
  const artifact = release(envelope, publicKey, target);
  if (previous) {
    const a = artifact.version.split('.').map(BigInt);
    const b = previous.version.split('.').map(BigInt);
    const difference = a.findIndex((part, i) => part !== b[i]);
    if (difference >= 0 && a[difference] < b[difference] || difference === -1 && artifact.sha256 !== previous.sha256) {
      throw new Error('Refusing a release rollback or changed binary at the same version.');
    }
  }
  const binary = await fetch(artifact.url, artifact.size);
  checkBinary(binary, artifact);
  await fs.mkdir(directory, { recursive: true, mode: 0o700 });
  const stage = await fs.mkdtemp(path.join(directory, '.install-'));
  const destination = location(artifact);
  try {
    await fs.writeFile(path.join(stage, name), binary, { mode: 0o700 });
    await fs.writeFile(path.join(stage, `${name}.install.json`), '{"schema_version":1,"method":"plugin"}\n', { mode: 0o600 });
    try {
      await fs.rename(stage, path.dirname(destination));
    } catch (error) {
      if (!['EEXIST', 'ENOTEMPTY'].includes(error.code)) throw error;
      checkBinary(await fs.readFile(destination), artifact);
    }
    // Another installer may finish concurrently; every referenced directory is complete.
    const temporary = `${stage}.json`;
    try {
      await fs.writeFile(temporary, envelope, { mode: 0o600, flag: 'wx' });
      await fs.rename(temporary, index);
    } finally { await fs.rm(temporary, { force: true }); }
  } finally { await fs.rm(stage, { recursive: true, force: true }); }
  return destination;
}

async function main(args) {
  if (args.includes('--help') && args[0] !== '--run') {
    console.log('Ahrara plugin: node src/plugin/ahrara.mjs [--install | --update | --run <CLI arguments> | <MCP options>]');
    return;
  }
  let binary = process.env.AHRARA_BINARY;
  if (binary) {
    if (!path.isAbsolute(binary) || !await compatible(binary)) throw new Error('AHRARA_BINARY must name an absolute path to an Ahrara binary with stdio support.');
  } else {
    const local = args[0] === '--update' ? undefined : await command('ahrara');
    binary = local && await compatible(local) ? local : await install({ update: args[0] === '--update' });
  }
  if (['--install', '--update'].includes(args[0])) {
    console.log(binary);
    return;
  }
  const child = spawn(binary, args[0] === '--run' ? args.slice(1) : ['mcp', '--transport', 'stdio', ...args], {
    stdio: ['pipe', 'pipe', 'inherit'], windowsHide: true,
  });
  process.stdin.pipe(child.stdin);
  child.stdout.pipe(process.stdout);
  child.stdin.on('error', () => process.stdin.unpipe(child.stdin));
  process.stdout.on('error', () => child.kill());
  const stop = (signal) => child.kill(signal);
  process.on('SIGINT', stop);
  process.on('SIGTERM', stop);
  try {
    await new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('exit', (code, signal) => { process.exitCode = code ?? (signal ? 1 : 0); resolve(); });
    });
  } finally {
    process.stdin.destroy();
    process.off('SIGINT', stop);
    process.off('SIGTERM', stop);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).catch((error) => { console.error(`Ahrara: ${error.message}`); process.exitCode = 1; });
}
