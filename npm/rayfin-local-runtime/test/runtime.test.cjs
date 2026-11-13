'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const { test } = require('node:test');
const { Worker } = require('node:worker_threads');
const { once } = require('node:events');
const { createRequire } = require('node:module');
const { setTimeout: delay } = require('node:timers/promises');

const installed = process.env.RAYFIN_LOCAL_TEST_PACKAGE;
assert.ok(installed, 'Run against the packed, installed package: set RAYFIN_LOCAL_TEST_PACKAGE.');
const packageRequire = createRequire(path.join(installed, 'package.json'));
const { createRuntime, RayfinRuntimeError } = packageRequire('@microsoft/rayfin-local-runtime');
const fixture = process.env.RAYFIN_LOCAL_TEST_PROJECT;
assert.ok(fixture, 'Set RAYFIN_LOCAL_TEST_PROJECT to an installed fixture; native smoke tests must not skip.');

function options(stateDir) {
  return {
    projectDir: fixture,
    stateDir,
    frontend: { mode: 'off' },
    functions: { enabled: false },
    server: { host: '127.0.0.1', port: 0 },
  };
}

test('packed ESM and CommonJS exports agree, and binary does not load the addon', async () => {
  const binary = packageRequire('@microsoft/rayfin-local-runtime/binary');
  assert.ok(path.isAbsolute(binary.binaryPath));
  const esm = await import(pathToFileURL(path.join(installed, 'binary.mjs')));
  assert.equal(esm.binaryPath, binary.binaryPath);
  assert.equal(spawnSync(binary.binaryPath, ['--version'], { encoding: 'utf8' }).status, 0);
  const result = spawnSync(process.execPath, ['-e', `
    require.extensions['.node'] = () => { throw new Error('addon must not load'); };
    const { binaryPath } = require(${JSON.stringify(path.join(installed, 'binary.cjs'))});
    if (!require('node:fs').existsSync(binaryPath)) process.exit(1);
  `], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  assert.equal((await import(pathToFileURL(path.join(installed, 'index.mjs')))).createRuntime, createRuntime);
  const esmExports = spawnSync(process.execPath, ['--input-type=module', '-e', `
    import { createRuntime } from '@microsoft/rayfin-local-runtime';
    import { binaryPath } from '@microsoft/rayfin-local-runtime/binary';
    if (typeof createRuntime !== 'function' || typeof binaryPath !== 'string') process.exit(1);
  `], { cwd: installed, encoding: 'utf8' });
  assert.equal(esmExports.status, 0, esmExports.stderr);
});

test('close before start is idempotent and stopped handles cannot restart', async () => {
  const runtime = createRuntime(options());
  assert.equal(runtime.state, 'created');
  assert.equal((await runtime.status()).state, 'created');
  await assert.rejects(runtime.reload(), (error) => error.code === 'not-running');
  assert.equal(runtime.close(), runtime.close());
  const stopped = await runtime.closed;
  assert.deepEqual(stopped, { reason: 'closed', error: null, cleanupErrors: [] });
  await assert.rejects(runtime.start(), (error) => error.code === 'runtime-stopped');
});

test('failed startup is structured, isolated, and closed settles', async () => {
  const projectDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rayfin-missing-'));
  try {
    const runtime = createRuntime({ ...options(), projectDir });
    const events = [];
    runtime.onEvent((event) => events.push(event));
    await assert.rejects(runtime.start(), (error) => error instanceof RayfinRuntimeError && error.code === 'project-not-found');
    const stopped = await runtime.closed;
    assert.equal(stopped.error.code, 'project-not-found');
    assert.equal(runtime.state, 'stopped');
    assert.equal(events.at(-1).type, 'stopped');
  } finally {
    fs.rmSync(projectDir, { recursive: true, force: true });
  }
});

test('already aborted startup stops without leaving resources', async () => {
  const runtime = createRuntime(options());
  await assert.rejects(runtime.start({ signal: AbortSignal.abort() }), (error) => error.code === 'aborted');
  assert.deepEqual((await runtime.closed).cleanupErrors, []);
});

test('invalid and unknown native configuration produces structured errors', () => {
  for (const invalid of [
    { server: { port: -1 } },
    { frontend: { mode: 'unexpected' } },
    { frontend: { mode: 'dev', port: 0 } },
    { unknownOption: true },
    { server: { unknownOption: true } },
  ]) {
    assert.throws(() => createRuntime({ ...options(), ...invalid }),
      (error) => error instanceof RayfinRuntimeError && error.code === 'config-invalid');
  }
});

test('cancellation during startup releases the state lock before rejection', { timeout: 120_000 }, async () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'rayfin-cancel-'));
  const runtime = createRuntime(options(temporary));
  const controller = new AbortController();
  const events = [];
  runtime.onEvent((event) => {
    events.push(event);
    if (event.type === 'starting') controller.abort();
  });
  let replacement;
  try {
    await assert.rejects(runtime.start({ signal: controller.signal }), (error) => error.code === 'aborted');
    assert.ok(events.some((event) => event.type === 'starting'));
    assert.ok(!events.some((event) => event.type === 'ready'));
    assert.deepEqual((await runtime.closed).cleanupErrors, []);
    replacement = createRuntime(options(temporary));
    await replacement.start();
  } finally {
    await Promise.all([runtime.close(), replacement?.close()]);
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});

test('worker teardown releases its runtime without stopping the main environment', { timeout: 120_000 }, async () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'rayfin-worker-'));
  const main = createRuntime(options(path.join(temporary, 'main')));
  const workerOptions = options(path.join(temporary, 'worker'));
  const worker = new Worker(`
    const { parentPort, workerData } = require('node:worker_threads');
    const { createRuntime } = require(workerData.installed);
    const runtime = createRuntime(workerData.options);
    runtime.start().then((ready) => parentPort.postMessage(ready));
  `, { eval: true, workerData: { installed, options: workerOptions } });
  const workerReady = once(worker, 'message');
  let replacement;
  try {
    const [mainReady, [ready]] = await Promise.all([main.start(), workerReady]);
    assert.notEqual(ready.apiUrl, mainReady.apiUrl);
    await worker.terminate();
    const deadline = Date.now() + 10_000;
    for (;;) {
      replacement = createRuntime(workerOptions);
      try {
        await replacement.start();
        break;
      } catch (error) {
        await replacement.close();
        if (error.code !== 'already-serving' || Date.now() >= deadline) throw error;
        await delay(50);
      }
    }
    assert.equal((await fetch(new URL('/.rayfin-local/health', mainReady.apiUrl))).status, 200);
  } finally {
    await worker.terminate();
    await Promise.all([main.close(), replacement?.close()]);
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});

test('closing a running runtime lets a host exit without forced process termination', { timeout: 120_000 }, () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'rayfin-host-'));
  try {
    const result = spawnSync(process.execPath, ['-e', `
      const { createRuntime } = require(${JSON.stringify(installed)});
      const runtime = createRuntime(${JSON.stringify(options(temporary))});
      runtime.start().then(() => runtime.close()).catch((error) => {
        console.error(error);
        process.exitCode = 1;
      });
    `], { encoding: 'utf8', timeout: 60_000 });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, '', 'embedded services must not write CLI output');
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});

test('native runtime owns Vite and functions and serves the real SDK', { timeout: 120_000 }, async () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'rayfin-services-'));
  const runtime = createRuntime({
    ...options(temporary),
    frontend: { mode: 'dev' },
    functions: { enabled: true },
  });
  const events = [];
  runtime.onEvent((event) => events.push(event));
  try {
    const ready = await runtime.start();
    assert.ok(events.some((event) => event.type === 'functions' && event.state === 'ready'),
      JSON.stringify(events.filter((event) => event.type === 'functions')));
    assert.ok(events.some((event) => event.type === 'vite' && event.state === 'ready'));
    assert.equal((await fetch(ready.appUrl)).status, 200);
    const config = await (await fetch(new URL('/rayfin.config.json', ready.apiUrl))).json();
    const fixtureRequire = createRequire(path.join(fixture, 'package.json'));
    const { RayfinClient } = await import(pathToFileURL(fixtureRequire.resolve('@microsoft/rayfin-client')));
    const client = new RayfinClient({
      baseUrl: ready.apiUrl,
      publishableKey: config.publishableKey,
      authStorage: false,
      persistSession: false,
      autoRefreshToken: false,
      multiTabSync: false,
    });
    assert.deepEqual(await client.data.Todo.select(['id', 'title']).execute(), []);
    await client.auth.signUp({ email: 'native@example.test', password: 'Native-test-password-42!' });
    assert.equal(await client.functions.ping.invoke(), 'pong');
    assert.deepEqual((await runtime.close()).cleanupErrors, []);
    await assert.rejects(fetch(ready.appUrl, { signal: AbortSignal.timeout(2000) }));
    await assert.rejects(fetch(new URL('/.rayfin-local/health', ready.apiUrl), { signal: AbortSignal.timeout(2000) }));
  } finally {
    await runtime.close();
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});

test('two actual native runtimes isolate state, locks, events, and reload', { timeout: 120_000 }, async () => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'rayfin-native-'));
  const first = createRuntime(options(path.join(temporary, 'one')));
  const second = createRuntime(options(path.join(temporary, 'two')));
  const oneEvents = [];
  const twoEvents = [];
  first.onEvent((event) => oneEvents.push(event));
  second.onEvent((event) => twoEvents.push(event));
  const cwd = process.cwd();
  const token = process.env.RAYFIN_LOCAL_CONTROL_TOKEN;
  const controller = new AbortController();
  try {
    const starting = first.start({ signal: controller.signal });
    assert.equal(first.start(), starting);
    const [one, two] = await Promise.all([starting, second.start()]);
    assert.notEqual(one.apiUrl, two.apiUrl);
    assert.notEqual(one.dataDir, two.dataDir);
    for (const ready of [one, two]) {
      assert.equal((await fetch(new URL('/.rayfin-local/health', ready.apiUrl))).status, 200);
      assert.equal((await fetch(new URL('/rayfin.config.json', ready.apiUrl))).status, 200);
    }
    controller.abort();
    assert.equal(first.state, 'running', 'startup signal is detached after readiness');
    const conflict = createRuntime(options(path.join(temporary, 'one')));
    await assert.rejects(conflict.start(), (error) => error.code === 'already-serving');
    await conflict.closed;
    const result = await first.reload();
    assert.equal(result.schema.type, 'schema');
    assert.equal((await first.status()).state, 'running');
    assert.equal(process.cwd(), cwd);
    assert.equal(process.env.RAYFIN_LOCAL_CONTROL_TOKEN, token);
    await first.close();
    await assert.rejects(first.start(), (error) => error.code === 'runtime-stopped');
    assert.deepEqual((await first.closed).cleanupErrors, []);
    assert.equal(second.state, 'running');
    const replacement = createRuntime(options(path.join(temporary, 'one')));
    await replacement.start();
    await replacement.close();
    assert.ok(oneEvents.some((event) => event.type === 'ready'));
    assert.ok(twoEvents.some((event) => event.type === 'ready'));
    assert.ok(oneEvents.filter((event) => event.type === 'ready').every((event) => event.dataDir === one.dataDir));
    assert.ok(twoEvents.filter((event) => event.type === 'ready').every((event) => event.dataDir === two.dataDir));
  } finally {
    await Promise.all([first.close(), second.close()]);
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});
