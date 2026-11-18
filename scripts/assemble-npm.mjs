#!/usr/bin/env node
// Copies final native artifacts verbatim: signing, when configured, happens before assembly.
import { copyFile, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({
  options: {
    target: { type: 'string' },
    binary: { type: 'string' },
    addon: { type: 'string' },
    out: { type: 'string', default: '.artifacts/npm' },
  },
});
const targets = {
  'aarch64-apple-darwin': ['darwin-arm64', 'darwin', 'arm64'],
  'x86_64-apple-darwin': ['darwin-x64', 'darwin', 'x64'],
  'aarch64-unknown-linux-gnu': ['linux-arm64-gnu', 'linux', 'arm64', 'glibc'],
  'x86_64-unknown-linux-gnu': ['linux-x64-gnu', 'linux', 'x64', 'glibc'],
  'x86_64-pc-windows-msvc': ['win32-x64-msvc', 'win32', 'x64'],
};
if (!targets[values.target] || !values.binary || !values.addon) {
  throw new Error('Usage: node scripts/assemble-npm.mjs --target <Rust target> --binary <executable> --addon <cdylib> [--out <directory>]');
}
const [suffix, os, cpu, libc] = targets[values.target];
const source = path.join(root, 'npm/rayfin-local-runtime');
const manifest = JSON.parse(await readFile(path.join(source, 'package.json'), 'utf8'));
const workspace = await readFile(path.join(root, 'Cargo.toml'), 'utf8');
const rustVersion = workspace.match(/\[workspace\.package\][\s\S]*?\nversion = "([^"]+)"/)?.[1];
if (rustVersion !== manifest.version || Object.values(manifest.optionalDependencies).some((version) => version !== rustVersion)) {
  throw new Error('Rust, wrapper, and optional platform package versions must match exactly.');
}
const output = path.resolve(values.out);
const wrapper = path.join(output, 'rayfin-local-runtime');
const platform = path.join(output, `rayfin-local-runtime-${suffix}`);
await mkdir(wrapper, { recursive: true });
await mkdir(platform, { recursive: true });
for (const name of await readdir(source)) {
  if (name === 'package.json' || name === 'README.md' || /\.(cjs|mjs|cts|mts)$/.test(name)) {
    await copyFile(path.join(source, name), path.join(wrapper, name));
  }
}
await copyFile(path.join(root, 'LICENSE'), path.join(wrapper, 'LICENSE'));
await copyFile(path.join(root, 'LICENSE'), path.join(platform, 'LICENSE'));
await copyFile(path.resolve(values.binary), path.join(platform, os === 'win32' ? 'rayfin-local.exe' : 'rayfin-local'));
await copyFile(path.resolve(values.addon), path.join(platform, 'rayfin_local.node'));
await writeFile(path.join(platform, 'package.json'), `${JSON.stringify({
  name: `${manifest.name}-${suffix}`,
  version: manifest.version,
  description: `Native Rayfin local runtime for ${suffix}.`,
  license: manifest.license,
  os: [os],
  cpu: [cpu],
  ...(libc ? { libc: [libc] } : {}),
  engines: manifest.engines,
  files: ['rayfin_local.node', 'rayfin-local', 'rayfin-local.exe', 'LICENSE'],
}, null, 2)}\n`);
console.log(JSON.stringify({ wrapper, platform }));
