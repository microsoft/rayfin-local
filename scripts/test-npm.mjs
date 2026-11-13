#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { values } = parseArgs({
  options: { platform: { type: 'string' }, project: { type: 'string' } },
});
if (!values.platform || !values.project) throw new Error('Supply --platform <assembled package> and --project <installed fixture>.');
const fixture = path.resolve(values.project);
const artifacts = path.join(root, '.artifacts');
await mkdir(artifacts, { recursive: true });
const temporary = await mkdtemp(path.join(artifacts, 'npm-smoke-'));
const npm = process.platform === 'win32' ? process.execPath : 'npm';
const npmArgs = process.platform === 'win32'
  ? [path.join(path.dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js')]
  : [];

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { cwd: temporary, encoding: 'utf8', ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${program} ${args.join(' ')} failed (${result.status}):\n${result.stdout ?? ''}${result.stderr ?? ''}`);
  }
  return result.stdout;
}

try {
  const wrapper = path.join(path.dirname(path.resolve(values.platform)), 'rayfin-local-runtime');
  const pack = (directory) => {
    const result = JSON.parse(run(npm, [...npmArgs, 'pack', directory, '--json', '--ignore-scripts']));
    const packed = Array.isArray(result) ? result[0] : Object.values(result)[0];
    return path.join(temporary, packed.filename);
  };
  const platformTarball = pack(path.resolve(values.platform));
  const wrapperTarball = pack(wrapper);
  await writeFile(path.join(temporary, 'package.json'), '{"name":"rayfin-runtime-smoke","version":"0.0.0","private":true}\n');
  run(npm, [...npmArgs, 'install', '--offline', '--ignore-scripts', '--omit=optional', '--no-audit', '--no-fund', platformTarball, wrapperTarball]);
  const installed = path.join(temporary, 'node_modules/@microsoft/rayfin-local-runtime');
  run(process.execPath, ['--test', path.join(root, 'npm/rayfin-local-runtime/test/runtime.test.cjs')], {
    stdio: 'inherit',
    env: { ...process.env, RAYFIN_LOCAL_TEST_PACKAGE: installed, RAYFIN_LOCAL_TEST_PROJECT: fixture },
  });
  const source = `
import { createRuntime, type StopInfo, type ReadyInfo } from '@microsoft/rayfin-local-runtime';
import { binaryPath } from '@microsoft/rayfin-local-runtime/binary';
const runtime = createRuntime({ projectDir: '.', server: { port: 0 }, frontend: { mode: 'off' } });
const ready: Promise<ReadyInfo> = runtime.start();
const closed: Promise<StopInfo> = runtime.closed;
const binary: string = binaryPath;
void ready; void closed; void binary;
`;
  for (const extension of ['mts', 'cts']) await writeFile(path.join(temporary, `types.${extension}`), source);
  run(process.execPath, [
    path.join(fixture, 'node_modules/typescript/bin/tsc'), '--noEmit', '--strict',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', '--target', 'ES2023',
    'types.mts', 'types.cts',
  ], { stdio: 'inherit' });
} finally {
  await rm(temporary, { recursive: true, force: true });
}
