#!/usr/bin/env node
'use strict';

const childProcess = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const SUPPORTED = ['darwin-arm64', 'darwin-x64', 'linux-x64', 'linux-arm64'];
const key = `${process.platform}-${process.arch}`;
const platformPackage = `qareel-${key}`;
const version = require('../package.json').version;

function fail(message) {
  fs.writeSync(2, `qareel: ${message}\n`);
  process.exit(1);
}

function isExecutable(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

function packageRoot() {
  try {
    return path.dirname(require.resolve(`${platformPackage}/package.json`));
  } catch {
    return undefined;
  }
}

function findBinary() {
  if (!SUPPORTED.includes(key)) {
    fail(`qareel runs on macOS and Linux with arm64 or x64; this machine is ${process.platform} ${process.arch}.`);
  }
  const root = packageRoot();
  if (root !== undefined) {
    const installed = require(path.join(root, 'package.json')).version;
    if (installed !== version) {
      fail(`${platformPackage} is version ${installed} but qareel is ${version}.\nFix: npm install ${platformPackage}@${version}`);
    }
    const binary = path.join(root, 'bin', 'qareel');
    if (isExecutable(binary)) {
      return binary;
    }
    fail(`${binary} is missing or not executable.\nFix: npm install ${platformPackage}@${version}`);
  }
  const vendored = path.join(__dirname, '..', 'vendor', key, 'bin', 'qareel');
  if (isExecutable(vendored)) {
    return vendored;
  }
  fail([
    `the ${key} binary is not installed. npm installs it from the optional package ${platformPackage},`,
    'which is skipped when optional dependencies are omitted (--omit=optional) or the lockfile came from another platform.',
    `Fix: npm install ${platformPackage}@${version}`,
    `Or for a global install: npm install -g @patrick-lai/qareel@${version}`,
  ].join('\n'));
}

const binary = findBinary();
const terminalSignals = ['SIGINT', 'SIGQUIT'];
const waitForChild = () => {};
for (const signal of terminalSignals) {
  process.on(signal, waitForChild);
}
const result = childProcess.spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
for (const signal of terminalSignals) {
  process.removeListener(signal, waitForChild);
}
if (result.error) {
  fail(`could not start ${binary}: ${result.error.message}`);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
  process.exit(128 + (os.constants.signals[result.signal] || 0));
}
process.exit(result.status === null ? 1 : result.status);
