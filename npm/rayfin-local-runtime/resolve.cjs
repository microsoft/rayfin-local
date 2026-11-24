'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { version } = require('./package.json');

function platformPackage() {
  const { platform, arch } = process;
  const supported = {
    darwin: { arm64: 'darwin-arm64', x64: 'darwin-x64' },
    linux: { arm64: 'linux-arm64-gnu', x64: 'linux-x64-gnu' },
    win32: { x64: 'win32-x64-msvc' },
  };
  const target = supported[platform]?.[arch];
  if (!target || (platform === 'linux' && !process.report.getReport().header.glibcVersionRuntime)) {
    throw new Error(`Unsupported Rayfin runtime platform: ${platform}/${arch}. Linux requires glibc.`);
  }
  const name = `@microsoft/rayfin-local-runtime-${target}`;
  let manifest;
  try {
    manifest = require.resolve(`${name}/package.json`);
  } catch (cause) {
    throw new Error(`Missing ${name}@${version}. Reinstall with npm optional dependencies enabled.`, { cause });
  }
  const installed = JSON.parse(fs.readFileSync(manifest, 'utf8'));
  if (installed.version !== version) {
    throw new Error(`Rayfin runtime version mismatch: expected ${version}, found ${installed.version} in ${name}.`);
  }
  return path.dirname(manifest);
}

function artifact(name) {
  const resolved = path.resolve(platformPackage(), name);
  if (!fs.statSync(resolved, { throwIfNoEntry: false })?.isFile()) {
    throw new Error(`Rayfin runtime artifact is missing: ${resolved}. Reinstall the package.`);
  }
  return resolved;
}

exports.artifact = artifact;
