import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { promises as fs } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const host = `${process.platform}-${process.arch}`;
const executable = process.platform === 'win32' ? 'ahrara.exe' : 'ahrara';

// Unpacks npm tarballs where npm would install them: ahrara and @ahrara/cli-<host>.
async function install(tarballs, prefix) {
  for (const tarball of tarballs) {
    const name = JSON.parse(execFileSync('tar', ['-xzOf', tarball, 'package/package.json'])).name;
    const target = path.join(prefix, 'node_modules', name);
    await fs.mkdir(target, { recursive: true });
    execFileSync('tar', ['-xzf', tarball, '-C', target, '--strip-components=1']);
  }
  return path.join(prefix, 'node_modules/ahrara/bin/ahrara.js');
}

test('npm packages install one executable per platform with an npm receipt', { skip: !process.env.AHRARA_TEST_BINARY || host !== 'linux-x64' }, async () => {
  const directory = await fs.mkdtemp(path.join(tmpdir(), 'ahrara-npm-'));
  try {
    const dist = path.join(directory, 'dist');
    await fs.mkdir(dist);
    for (const target of ['linux-x86_64', 'linux-aarch64', 'darwin-x86_64', 'darwin-aarch64', 'windows-x86_64.exe', 'windows-aarch64.exe']) {
      await fs.writeFile(path.join(dist, `ahrara-${target}`), 'synthetic executable');
    }
    await fs.copyFile(process.env.AHRARA_TEST_BINARY, path.join(dist, 'ahrara-linux-x86_64'));
    await fs.writeFile(path.join(dist, 'ahrara-third-party-licenses.md'), 'licenses');
    const output = path.join(directory, 'packages');
    execFileSync(process.execPath, [path.join(root, 'scripts/npm-cli.mjs'), dist, output]);

    const { version } = JSON.parse(await fs.readFile(path.join(root, 'src/cli/npm/package.json')));
    const tarballs = (await fs.readdir(output)).sort();
    assert.equal(tarballs.length, 7);
    const manifest = (tarball) => JSON.parse(execFileSync('tar', ['-xzOf', path.join(output, tarball), 'package/package.json']));
    const launcher = manifest(`ahrara-${version}.tgz`);
    assert.equal(launcher.version, version);
    assert.deepEqual(Object.values(launcher.optionalDependencies), Array(6).fill(version));
    const linux = manifest(`ahrara-cli-linux-x64-${version}.tgz`);
    assert.deepEqual([linux.os, linux.cpu], [['linux'], ['x64']]);
    assert.ok(manifest(`ahrara-cli-win32-arm64-${version}.tgz`).files.includes('ahrara.exe.install.json'));

    const bin = await install([path.join(output, `ahrara-${version}.tgz`), path.join(output, `ahrara-cli-linux-x64-${version}.tgz`)], directory);
    assert.equal(execFileSync(process.execPath, [bin, '--version']).toString().trim(), `ahrara ${version}`);
    const receipt = path.join(directory, 'node_modules/@ahrara/cli-linux-x64/ahrara.install.json');
    assert.deepEqual(JSON.parse(await fs.readFile(receipt)), { schema_version: 1, method: 'npm' });
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});

test('the launcher passes its exit code and the signals it receives to the executable', { skip: process.platform === 'win32' }, async () => {
  const directory = await fs.mkdtemp(path.join(tmpdir(), 'ahrara-launcher-'));
  try {
    const launcher = path.join(directory, 'node_modules/ahrara/bin');
    const platform = path.join(directory, `node_modules/@ahrara/cli-${host}`);
    await fs.mkdir(launcher, { recursive: true });
    await fs.mkdir(platform, { recursive: true });
    await fs.copyFile(path.join(root, 'src/cli/npm/bin/ahrara.js'), path.join(launcher, 'ahrara.js'));
    // Like the CLI, it exits on its own terms when stopped; any argument fails fast.
    await fs.writeFile(path.join(platform, executable), '#!/bin/sh\n[ $# -eq 0 ] || exit 3\ntrap "exit 7" TERM\necho ready\nwhile :; do sleep 0.05; done\n', { mode: 0o755 });

    const run = (...args) => spawn(process.execPath, [path.join(launcher, 'ahrara.js'), ...args], { stdio: ['ignore', 'pipe', 'inherit'] });
    const exit = (child) => new Promise((resolve) => child.on('exit', (code, signal) => resolve({ code, signal })));

    assert.deepEqual(await exit(run('--fail')), { code: 3, signal: null });
    const child = run();
    await new Promise((resolve) => child.stdout.once('data', resolve));
    child.kill('SIGTERM');
    assert.deepEqual(await exit(child), { code: 7, signal: null }, 'SIGTERM reached the executable');
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});
