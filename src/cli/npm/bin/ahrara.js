#!/usr/bin/env node
// Runs the Ahrara executable from the platform package npm installed beside this
// one (@ahrara/cli-<platform>-<arch>, an optional dependency selected by os/cpu).
'use strict';
const { spawn } = require('node:child_process');

const name = `@ahrara/cli-${process.platform}-${process.arch}`;
let executable;
try {
  executable = require.resolve(`${name}/ahrara${process.platform === 'win32' ? '.exe' : ''}`);
} catch {
  console.error(`Ahrara has no npm package for ${process.platform}-${process.arch}, or npm skipped ${name}.`);
  console.error('Reinstall without --omit=optional, or see https://ahrara.dev/docs/#install for other methods.');
  process.exit(1);
}

const child = spawn(executable, process.argv.slice(2), { stdio: 'inherit' });
// Pass on signals sent to this launcher, such as an agent stopping its MCP server.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(signal, () => child.kill(signal));
child.on('error', (error) => {
  console.error(`Cannot run ${executable}: ${error.message}`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (!signal) process.exit(code ?? 1);
  process.removeAllListeners(signal);
  process.kill(process.pid, signal);
});
