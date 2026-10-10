#!/usr/bin/env node
// Packs the CLI for npm from the release binaries in <dist>: the `ahrara` launcher
// and one `@ahrara/cli-<platform>-<arch>` package per target, which npm installs
// by os/cpu. Each executable ships with its npm installation receipt.
// Usage: node scripts/npm-cli.mjs [dist] [output]
import { chmodSync, copyFileSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import path from 'node:path';

// Release target, npm os and cpu, and a readable name.
export const TARGETS = [
  ['linux-x86_64', 'linux', 'x64', 'Linux x64'],
  ['linux-aarch64', 'linux', 'arm64', 'Linux ARM64'],
  ['darwin-x86_64', 'darwin', 'x64', 'macOS x64'],
  ['darwin-aarch64', 'darwin', 'arm64', 'macOS ARM64'],
  ['windows-x86_64', 'win32', 'x64', 'Windows x64'],
  ['windows-aarch64', 'win32', 'arm64', 'Windows ARM64'],
];

const [dist = 'dist', output = 'dist/packages/npm-cli'] = process.argv.slice(2);
const root = path.resolve(import.meta.dirname, '..');
const launcher = JSON.parse(readFileSync(path.join(root, 'src/cli/npm/package.json')));
const { version, license, homepage, repository, bugs } = launcher;
const stage = path.join(output, 'stage');
rmSync(output, { recursive: true, force: true });

const pack = (directory, manifest) => {
  writeFileSync(path.join(directory, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  execFileSync('npm', ['pack', '--silent', '--pack-destination', path.resolve(output)], { cwd: directory, stdio: ['ignore', 'ignore', 'inherit'] });
};

const optionalDependencies = {};
for (const [target, os, cpu, label] of TARGETS) {
  const name = `@ahrara/cli-${os}-${cpu}`;
  const executable = os === 'win32' ? 'ahrara.exe' : 'ahrara';
  const directory = path.join(stage, `cli-${os}-${cpu}`);
  mkdirSync(directory, { recursive: true });
  copyFileSync(path.join(dist, `ahrara-${target}${os === 'win32' ? '.exe' : ''}`), path.join(directory, executable));
  chmodSync(path.join(directory, executable), 0o755);
  // `ahrara update` reads the receipt beside the real executable.
  writeFileSync(path.join(directory, `${executable}.install.json`), '{"schema_version":1,"method":"npm"}\n');
  copyFileSync(path.join(root, 'LICENSE'), path.join(directory, 'LICENSE'));
  copyFileSync(path.join(dist, 'ahrara-third-party-licenses.md'), path.join(directory, 'LICENSE-THIRD-PARTY.md'));
  pack(directory, {
    name,
    version,
    description: `The Ahrara CLI for ${label}; install the ahrara package instead`,
    license,
    homepage,
    repository,
    bugs,
    os: [os],
    cpu: [cpu],
    files: [executable, `${executable}.install.json`, 'LICENSE', 'LICENSE-THIRD-PARTY.md'],
    preferUnplugged: true,
    publishConfig: { access: 'public' },
  });
  optionalDependencies[name] = version;
}

const directory = path.join(stage, 'ahrara');
cpSync(path.join(root, 'src/cli/npm'), directory, { recursive: true });
copyFileSync(path.join(root, 'LICENSE'), path.join(directory, 'LICENSE'));
pack(directory, { ...launcher, files: [...launcher.files, 'LICENSE'], optionalDependencies });
rmSync(stage, { recursive: true });
