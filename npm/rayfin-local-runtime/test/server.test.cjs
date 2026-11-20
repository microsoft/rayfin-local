'use strict';

const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const { PassThrough } = require('node:stream');
const childProcess = require('node:child_process');
const { test } = require('node:test');
const path = require('node:path');
const { setImmediate: tick } = require('node:timers/promises');

const { startRayfinLocal } = require('../server.cjs');
const binaryModule = require.resolve('../binary.cjs');
const ready = {
  v: 1, type: 'ready', openSignIn: false, appUrl: null, embedUrl: null,
  apiUrl: 'http://localhost:12345', sdk: { supported: true }, signInAs: { kind: 'none' }, features: {},
};

function fixture(t) {
  const cached = require.cache[binaryModule];
  require.cache[binaryModule] = { exports: { binaryPath: '/packaged/rayfin-local' } };
  t.after(() => {
    if (cached) require.cache[binaryModule] = cached;
    else delete require.cache[binaryModule];
  });
  const child = new EventEmitter();
  child.stdin = new PassThrough();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  child.kill = t.mock.fn(() => { child.emit('close', null, 'SIGKILL'); return true; });
  const spawn = t.mock.method(childProcess, 'spawn', () => child);
  const warnings = t.mock.method(process, 'emitWarning', () => {});
  const event = (value) => child.stdout.write(`${JSON.stringify({ v: 1, ...value })}\n`);
  const close = (code = 0) => child.emit('close', code, null);
  t.after(() => close());
  return { child, spawn, event, close, warnings };
}

test('defaults to loopback backend and closed sign-in without overriding CLI identity or state', async (t) => {
  const f = fixture(t);
  const old = process.env.RAYFIN_LOCAL_CONTROL_TOKEN;
  const oldOpen = process.env.RAYFIN_LOCAL_OPEN_SIGN_IN;
  const oldState = process.env.RAYFIN_LOCAL_STATE_DIR;
  process.env.RAYFIN_LOCAL_CONTROL_TOKEN = 'do-not-inherit';
  process.env.RAYFIN_LOCAL_OPEN_SIGN_IN = 'true';
  process.env.RAYFIN_LOCAL_STATE_DIR = 'inherited-state';
  t.after(() => {
    if (old === undefined) delete process.env.RAYFIN_LOCAL_CONTROL_TOKEN;
    else process.env.RAYFIN_LOCAL_CONTROL_TOKEN = old;
    if (oldOpen === undefined) delete process.env.RAYFIN_LOCAL_OPEN_SIGN_IN;
    else process.env.RAYFIN_LOCAL_OPEN_SIGN_IN = oldOpen;
    if (oldState === undefined) delete process.env.RAYFIN_LOCAL_STATE_DIR;
    else process.env.RAYFIN_LOCAL_STATE_DIR = oldState;
  });
  const starting = startRayfinLocal({ projectDir: 'project with spaces' });
  const [binary, args, options] = f.spawn.mock.calls[0].arguments;
  assert.equal(binary, '/packaged/rayfin-local');
  assert.deepEqual(args, ['serve', '--json',
    '-C', path.resolve('project with spaces'), '--host', '127.0.0.1', '--port', '0',
    '--no-frontend']);
  assert.equal(options.env.RAYFIN_LOCAL_CONTROL_TOKEN, undefined);
  assert.equal(options.env.RAYFIN_LOCAL_OPEN_SIGN_IN, undefined);
  assert.equal(options.env.RAYFIN_LOCAL_STATE_DIR, 'inherited-state');
  assert.equal(options.env.RAYFIN_LOCAL_NODE, process.execPath);
  assert.equal(options.shell, undefined);
  f.child.stdout.write(JSON.stringify(ready).slice(0, 20));
  f.child.stdout.write(`${JSON.stringify(ready).slice(20)}\n`);
  const running = await starting;
  assert.deepEqual(Object.keys(running).sort(), ['apiUrl', 'closed', 'ready', 'stop']);
  assert.deepEqual(running.ready, ready);
  let stopped = false;
  const stop = running.stop();
  assert.equal(stop, running.stop());
  void stop.then(() => { stopped = true; });
  await tick();
  assert.equal(stopped, false);
  assert(f.child.stdin.writableEnded);
  f.close();
  await stop;
  await running.closed;
  assert.equal(stopped, true);
});

test('invalid input never spawns', async (t) => {
  const f = fixture(t);
  for (const options of [
    undefined, {}, { projectDir: '' }, { projectDir: '.', binary: 'bad' },
    ...[{ host: '0.0.0.0' }, { host: 'example.com' }, { port: -1 }, { port: 65536 },
      { port: 1.5 }, { frontend: 'static' }, { openSignIn: 'true' }, { signInAs: 'user:' },
      { signInAs: 'user:name\ninjection' }, { signInAs: 'admin' }, { stateDir: '' },
      { stateDir: 1 }, { forceSchema: 'true' }, { reset: 1 }, { onEvent: true }]
      .map((option) => ({ projectDir: '.', ...option })),
  ]) {
    await assert.rejects(startRayfinLocal(options), { code: 'config-invalid' });
  }
  assert.equal(f.spawn.mock.callCount(), 0);
});

test('state and destructive flags are explicit and unspecified identity accepts CLI configuration', async (t) => {
  const f = fixture(t);
  const starting = startRayfinLocal({
    projectDir: '.', stateDir: 'state with spaces', forceSchema: true, reset: true,
  });
  assert.deepEqual(f.spawn.mock.calls[0].arguments[1], [
    'serve', '--json', '-C', path.resolve('.'), '--host', '127.0.0.1', '--port', '0',
    '--state-dir', path.resolve('state with spaces'), '--force-schema', '--reset', '--no-frontend',
  ]);
  f.event({ ...ready, signInAs: { kind: 'user', username: 'configured@example.test' } });
  const running = await starting;
  const stopped = running.stop();
  f.close();
  await stopped;
});

test('honors caller-selected frontend, loopback host, port and explicit sign-in policy', async (t) => {
  const f = fixture(t);
  const starting = startRayfinLocal({
    projectDir: '.', frontend: true, host: '::1', port: 54321,
    signInAs: 'user:example@example.test', openSignIn: true,
  });
  const args = f.spawn.mock.calls[0].arguments[1];
  assert.deepEqual(args, ['serve', '--json', '-C', path.resolve('.'),
    '--host', '::1', '--port', '54321', '--sign-in-as', 'user:example@example.test', '--open-sign-in']);
  const chosen = {
    ...ready, apiUrl: 'http://[::1]:54321', appUrl: 'http://localhost:5173',
    openSignIn: true, signInAs: { kind: 'user', username: 'example@example.test' },
  };
  f.event(chosen);
  const server = await starting;
  assert.equal(server.apiUrl, chosen.apiUrl);
  assert.deepEqual(server.ready, chosen);
  const stopped = server.stop();
  f.close();
  await stopped;
});

test('fatal startup errors reject only after cleanup, without copying child diagnostics', async (t) => {
  const f = fixture(t);
  const starting = startRayfinLocal({ projectDir: '.', signInAs: 'none' });
  let settled = false;
  const rejection = assert.rejects(starting, (error) => {
    assert.equal(error.code, 'already-serving');
    assert.equal(error.fatal, true);
    assert(!error.message.includes('secret'));
    settled = true;
    return true;
  });
  f.event({ type: 'error', fatal: true, code: 'already-serving', message: 'secret' });
  await tick();
  assert.equal(settled, false);
  assert(f.child.stdin.writableEnded);
  f.close(1);
  await rejection;
});

test('invalid readiness, malformed events and app build failures never resolve startup', async (t) => {
  for (const value of [
    { ...ready, apiUrl: 'https://localhost:12345' },
    { ...ready, apiUrl: 'http://localhost.evil:12345' },
    { ...ready, apiUrl: 'http://localhost:12345/path' },
    { ...ready, apiUrl: 'http://user:secret@localhost:12345' },
    { ...ready, openSignIn: true },
    { ...ready, signInAs: { kind: 'developer' } },
    { ...ready, v: 2 },
    { ...ready, appUrl: 'http://localhost:5173' },
    { ...ready, sdk: { supported: false } },
    { ...ready, features: { functions: { enabled: true } } },
    { type: 'analysis', ok: false },
    { type: 'schema', result: 'refused' },
    { type: 'functions', state: 'failed' },
    'malformed',
  ]) {
    await t.test(JSON.stringify(value), async (t) => {
      const f = fixture(t);
      const starting = startRayfinLocal({ projectDir: '.', signInAs: 'none' });
      const rejected = assert.rejects(starting, { name: 'RayfinServerError' });
      if (value === 'malformed') f.child.stdout.write('not json\n');
      else f.event(value);
      assert(f.child.stdin.writableEnded);
      f.close();
      await rejected;
    });
  }
});

test('spawn failure and early exit are structured', async (t) => {
  const f = fixture(t);
  const starting = startRayfinLocal({ projectDir: '.' });
  const rejected = assert.rejects(starting, { code: 'spawn-failed' });
  f.child.emit('error', new Error('ENOENT'));
  f.close(-1);
  await rejected;
});

test('unexpected exit rejects closed even if observed later and stop rejects with the same error', async (t) => {
  const f = fixture(t);
  const starting = startRayfinLocal({ projectDir: '.' });
  f.event(ready);
  const running = await starting;
  f.close(1);
  await tick(); // An unobserved rejection would fail the Node test runner here.
  await assert.rejects(running.closed, {
    name: 'RayfinServerError', code: 'backend-exited', exitCode: 1, signal: null,
  });
  assert.equal(running.stop(), running.closed);
  await assert.rejects(running.stop(), { code: 'backend-exited', exitCode: 1 });
  assert.equal(f.warnings.mock.callCount(), 0);
});

test('fatal runtime error rejects closed only after exit and preserves fatal code and exit metadata', async (t) => {
  const f = fixture(t);
  const starting = startRayfinLocal({ projectDir: '.' });
  f.event(ready);
  const running = await starting;
  let closed = false;
  const rejected = assert.rejects(running.closed, {
    name: 'RayfinServerError', code: 'runtime-failed', fatal: true, exitCode: null, signal: 'SIGTERM',
  }).then(() => { closed = true; });
  f.event({ type: 'error', fatal: true, code: 'runtime-failed' });
  await tick();
  assert.equal(closed, false);
  assert(f.child.stdin.writableEnded);
  f.child.emit('close', null, 'SIGTERM');
  await rejected;
});

test('startup and shutdown timeouts clean up before rejection', async (t) => {
  const f = fixture(t);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const starting = startRayfinLocal({ projectDir: '.' });
  const rejected = assert.rejects(starting, { code: 'startup-timeout' });
  t.mock.timers.tick(120_000);
  assert(f.child.stdin.writableEnded);
  assert.equal(f.child.kill.mock.callCount(), 0);
  t.mock.timers.tick(30_000);
  await rejected;
  assert.equal(f.child.kill.mock.callCount(), 1);
});

test('parent signal stops all instances and retains existing host handlers', async (t) => {
  const before = process.listenerCount('SIGTERM');
  const f = fixture(t);
  const hostSignal = () => {};
  process.on('SIGTERM', hostSignal);
  t.after(() => process.removeListener('SIGTERM', hostSignal));
  const starting = startRayfinLocal({ projectDir: '.' });
  f.event(ready);
  const running = await starting;
  process.emit('SIGTERM');
  assert(f.child.stdin.writableEnded);
  f.close();
  await running.stop();
  await tick();
  assert.equal(process.listenerCount('SIGTERM'), before + 1);
});

test('reload failures and restart-required are delivered as events without closing the backend', async (t) => {
  const f = fixture(t);
  const events = [];
  const starting = startRayfinLocal({ projectDir: '.', onEvent: (event) => events.push(event) });
  f.event({ type: 'functions', state: 'ready' });
  f.event({ ...ready, features: { functions: { enabled: true } } });
  const running = await starting;
  events.length = 0;
  const changes = [
    { type: 'functions', state: 'failed' }, { type: 'analysis', ok: false },
    { type: 'schema', result: 'refused' }, { type: 'schema', result: 'failed' },
    { type: 'config-changed', restartRequired: true },
  ];
  for (const change of changes) f.event(change);
  assert.deepEqual(events, changes.map((change) => ({ v: 1, ...change })));
  assert.equal(f.warnings.mock.callCount(), 0);
  assert.equal(f.child.stdin.writableEnded, false);
  const stopped = running.stop();
  f.close();
  await stopped;
});

test('throwing event callbacks do not break readiness or later event delivery', async (t) => {
  const f = fixture(t);
  let calls = 0;
  const starting = startRayfinLocal({
    projectDir: '.', onEvent: () => { if (++calls === 1) throw new Error('callback failed'); },
  });
  f.event(ready);
  const running = await starting;
  f.event({ type: 'config-changed', restartRequired: true });
  assert.equal(calls, 2);
  assert.equal(f.warnings.mock.calls[0].arguments[1].code, 'rayfin-event-callback-failed');
  const stopped = running.stop();
  f.close();
  await stopped;
});

test('split and batched stdout events retain their order and are delivered once', async (t) => {
  const f = fixture(t);
  const events = [];
  const starting = startRayfinLocal({ projectDir: '.', onEvent: (event) => events.push(event) });
  const line = JSON.stringify(ready);
  f.child.stdout.write(line.slice(0, 17));
  assert.equal(events.length, 0);
  const reload = { v: 1, type: 'analysis', ok: false };
  const restart = { v: 1, type: 'config-changed', restartRequired: true };
  f.child.stdout.write(`${line.slice(17)}\n${JSON.stringify(reload)}\n${JSON.stringify(restart)}\n`);
  const running = await starting;
  assert.deepEqual(events, [ready, reload, restart]);
  const stopped = running.stop();
  f.close();
  await stopped;
});
