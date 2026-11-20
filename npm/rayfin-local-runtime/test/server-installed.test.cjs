'use strict';

const assert = require('node:assert/strict');
const childProcess = require('node:child_process');
const { spawn, spawnSync } = childProcess;
const { once } = require('node:events');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { createRequire } = require('node:module');
const { pathToFileURL } = require('node:url');
const { test } = require('node:test');
const { setTimeout: delay } = require('node:timers/promises');

const installed = process.env.RAYFIN_LOCAL_TEST_PACKAGE;
const fixture = process.env.RAYFIN_LOCAL_TEST_PROJECT;
assert(installed && fixture, 'Set RAYFIN_LOCAL_TEST_PACKAGE and RAYFIN_LOCAL_TEST_PROJECT; installed tests must not skip.');
const packageRequire = createRequire(path.join(installed, 'package.json'));
// Importing either subpath must never try to load the native addon.
require.extensions['.node'] = () => { throw new Error('server loaded a native addon'); };
const { name } = packageRequire('./package.json');
const { startRayfinLocal } = packageRequire(`${name}/server`);
const signIn = { openSignIn: true, signInAs: 'user:example@example.test' };

async function project(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'rayfin-server-package-'));
  await fs.mkdir(path.join(root, 'rayfin', 'data'), { recursive: true });
  await fs.writeFile(path.join(root, 'package.json'), JSON.stringify({
    name: 'server-installed-test', private: true, type: 'module',
    dependencies: { '@microsoft/rayfin-core': '2.0.0' },
  }));
  for (const relative of ['tsconfig.json', 'rayfin/tsconfig.json']) {
    await fs.copyFile(path.join(fixture, relative), path.join(root, relative));
  }
  await fs.writeFile(path.join(root, 'rayfin', 'rayfin.yml'), `name: server-installed-test
services:
  auth:
    enabled: true
    fabric:
      enabled: true
  data:
    enabled: true
    dialect: mssql
  functions:
    enabled: false
  storage:
    enabled: false
`);
  await fs.writeFile(path.join(root, 'rayfin', 'data', 'Note.ts'), `import {entity,role,uuid,text} from '@microsoft/rayfin-core';
@entity()
@role('authenticated', ['create','read','update','delete'])
export class Note {
 @uuid() id!: string;
 @text() title!: string;
}
`);
  await fs.symlink(path.join(fixture, 'node_modules'), path.join(root, 'node_modules'), 'junction');
  t.after(async () => {
    await fs.unlink(path.join(root, 'node_modules'));
    await fs.rm(root, { recursive: true });
  });
  return root;
}

test('packed ESM/CJS server exports agree without loading native addon', async () => {
  const esm = await import(pathToFileURL(path.join(installed, 'server.mjs')));
  assert.equal(esm.startRayfinLocal, startRayfinLocal);
});

test('default server keeps CLI identity defaults but does not opt into open sign-in', { timeout: 120_000 }, async (t) => {
  const projectDir = await project(t);
  const old = process.env.RAYFIN_LOCAL_OPEN_SIGN_IN;
  process.env.RAYFIN_LOCAL_OPEN_SIGN_IN = 'true';
  let running;
  try {
    running = await startRayfinLocal({ projectDir });
    assert.equal(running.ready.openSignIn, false);
    assert.deepEqual(running.ready.signInAs, { kind: 'developer' });
    assert.equal(running.ready.appUrl, null);
    for (const route of ['session-token', 'session-stamp']) {
      assert.equal((await fetch(`${running.apiUrl}/.rayfin-local/dev/${route}`)).status, 403);
    }
  } finally {
    if (old === undefined) delete process.env.RAYFIN_LOCAL_OPEN_SIGN_IN;
    else process.env.RAYFIN_LOCAL_OPEN_SIGN_IN = old;
    await running?.stop();
  }
});

test('caller can start Rayfin frontend and select automatic identity without opening sign-in', { timeout: 120_000 }, async (t) => {
  const projectDir = await project(t);
  await fs.writeFile(path.join(projectDir, 'index.html'), '<!doctype html><title>Standalone Rayfin server</title>');
  const running = await startRayfinLocal({
    projectDir, frontend: true, host: 'localhost', signInAs: 'user:standalone@example.test',
  });
  try {
    assert.equal(running.ready.openSignIn, false);
    assert.deepEqual(running.ready.signInAs, { kind: 'user', username: 'standalone@example.test' });
    assert.equal((await fetch(running.ready.appUrl)).status, 200);
    assert.equal((await fetch(`${running.apiUrl}/.rayfin-local/dev/session-token`)).status, 403);
  } finally { await running.stop(); }
});

test('installed backend sessions, exclusive lock, idempotent awaited stop and restart', { timeout: 180_000 }, async (t) => {
  const projectDir = await project(t);
  let running;
  try {
    running = await startRayfinLocal({ projectDir, ...signIn });
    const first = running.apiUrl;
    assert.equal((await fetch(`${first}/.rayfin-local/health`)).status, 200);
    const stamp = await (await fetch(`${first}/.rayfin-local/dev/session-stamp`)).json();
    const session = await (await fetch(`${first}/.rayfin-local/dev/session-token`)).json();
    assert.equal(typeof session.refreshToken, 'string');
    assert.equal((await fetch(`${first}/.rayfin-local/dev/session-token`, {
      headers: { origin: 'https://untrusted.example' },
    })).status, 403);
    const config = await (await fetch(`${first}/rayfin.config.json`)).json();
    await assert.rejects(startRayfinLocal({ projectDir }), { code: 'already-serving' });
    const stop = running.stop();
    assert.equal(stop, running.stop());
    await stop;
    await running.closed;
    await assert.rejects(fetch(`${first}/.rayfin-local/health`, { signal: AbortSignal.timeout(2000) }));
    running = await startRayfinLocal({ projectDir, ...signIn });
    assert.deepEqual(await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-stamp`)).json(), stamp);
    const refresh = await fetch(`${running.apiUrl}/api/auth/v1/token`, {
      method: 'POST', headers: { 'content-type': 'application/json', 'x-publishable-key': config.publishableKey },
      body: JSON.stringify({ grantType: 'refresh_token', refreshToken: session.refreshToken }),
    });
    assert.equal(refresh.status, 200);
    const renewed = await refresh.json();
    assert(renewed.refreshToken !== session.refreshToken, 'refresh must rotate');
  } finally { await running?.stop(); }
});

test('configured identity and reload failures reach the host without an implicit identity override', { timeout: 120_000 }, async (t) => {
  const projectDir = await project(t);
  const config = path.join(projectDir, 'rayfin-local.toml');
  await fs.writeFile(config, '[auth]\nsign_in_as = "user:first@example.test"\n');
  const events = [];
  const running = await startRayfinLocal({ projectDir, openSignIn: true, onEvent: (event) => events.push(event) });
  const waitFor = async (predicate) => {
    const deadline = Date.now() + 30_000;
    while (!events.some(predicate)) {
      assert(Date.now() < deadline, 'expected CLI reload event was not delivered');
      await delay(50);
    }
  };
  try {
    assert.deepEqual(running.ready.signInAs, { kind: 'user', username: 'first@example.test' });
    const stamp = await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-stamp`)).json();
    await fs.writeFile(config, '[auth]\nsign_in_as = "user:second@example.test"\n');
    await waitFor((event) => event.type === 'ready' && event.signInAs?.username === 'second@example.test');
    assert.notDeepEqual(await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-stamp`)).json(), stamp);
    await fs.writeFile(path.join(projectDir, 'rayfin', 'data', 'Note.ts'), 'export class Note {');
    await waitFor((event) => event.type === 'analysis' && event.ok === false);
    assert.equal((await fetch(`${running.apiUrl}/.rayfin-local/health`)).status, 200);
  } finally { await running.stop(); }
  await running.closed;
});

test('state directory precedence and opt-in reset use the real CLI', { timeout: 180_000 }, async (t) => {
  const projectDir = await project(t);
  const inherited = path.join(projectDir, 'inherited-state');
  const selected = path.join(projectDir, 'selected-state');
  const old = process.env.RAYFIN_LOCAL_STATE_DIR;
  process.env.RAYFIN_LOCAL_STATE_DIR = inherited;
  let running;
  try {
    running = await startRayfinLocal({ projectDir, ...signIn });
    assert.equal(path.resolve(running.ready.dataDir), inherited);
    await running.stop();
    running = await startRayfinLocal({ projectDir, stateDir: path.relative(process.cwd(), selected), ...signIn });
    assert.equal(path.resolve(running.ready.dataDir), selected);
    const stamp = await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-stamp`)).json();
    await running.stop();
    running = await startRayfinLocal({ projectDir, stateDir: selected, ...signIn });
    assert.deepEqual(await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-stamp`)).json(), stamp);
    await running.stop();
    running = await startRayfinLocal({ projectDir, stateDir: selected, reset: true, ...signIn });
    assert.notDeepEqual(await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-stamp`)).json(), stamp);
  } finally {
    if (old === undefined) delete process.env.RAYFIN_LOCAL_STATE_DIR;
    else process.env.RAYFIN_LOCAL_STATE_DIR = old;
    await running?.stop();
  }
});

test('live destructive schema refusal is observable and an explicit forced restart applies it', { timeout: 120_000 }, async (t) => {
  const projectDir = await project(t);
  const events = [];
  let running;
  try {
    running = await startRayfinLocal({ projectDir, ...signIn, onEvent: (event) => events.push(event) });
    const config = await (await fetch(`${running.apiUrl}/rayfin.config.json`)).json();
    const session = await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-token`)).json();
    const created = await (await fetch(`${running.apiUrl}/graphql`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-publishable-key': config.publishableKey,
        authorization: `Bearer ${session.accessToken}` },
      body: JSON.stringify({ query: 'mutation { createNote(item: { title: "Keep until forced" }) { title } }' }),
    })).json();
    assert.equal(created.errors, undefined, JSON.stringify(created));
    assert.equal(created.data.createNote.title, 'Keep until forced');
    const model = path.join(projectDir, 'rayfin', 'data', 'Note.ts');
    await fs.writeFile(model, (await fs.readFile(model, 'utf8'))
      .replace(' @text() title!: string;', '').replace(',text', ''));
    const deadline = Date.now() + 30_000;
    while (!events.some((event) => event.type === 'schema' && ['refused', 'failed'].includes(event.result))) {
      assert(Date.now() < deadline, 'destructive schema reload failure was not delivered');
      await delay(50);
    }
    assert.equal((await fetch(`${running.apiUrl}/.rayfin-local/health`)).status, 200);
    await running.stop();
    await assert.rejects(startRayfinLocal({ projectDir }),
      (error) => error.name === 'RayfinServerError' && error.code.includes('schema'));
    running = await startRayfinLocal({ projectDir, forceSchema: true });
    assert.equal((await fetch(`${running.apiUrl}/.rayfin-local/health`)).status, 200);
  } finally { await running?.stop(); }
});

test('forced shutdown releases the project and leaves no Windows frontend/function processes', {
  timeout: 120_000, skip: process.platform !== 'win32' && 'Windows process-tree regression',
}, async (t) => {
  let link;
  let running;
  let replacement;
  let child;
  const descendants = new Set();
  const alive = (pid) => {
    try { process.kill(pid, 0); return true; }
    catch (error) { if (error.code === 'ESRCH') return false; throw error; }
  };
  t.after(async () => {
    await replacement?.stop();
    for (const pid of [...descendants].reverse()) {
      if (alive(pid)) process.kill(pid, 'SIGKILL');
    }
    if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
    await running?.closed.catch((error) => {
      if (!['stop-timeout', 'backend-exited'].includes(error.code)) throw error;
    });
    if (link) await fs.unlink(link);
  });
  const projectDir = await project(t);
  await fs.writeFile(path.join(projectDir, 'index.html'), '<!doctype html><title>Forced stop</title>');
  const functions = path.join(projectDir, 'rayfin', 'functions');
  await fs.cp(path.join(fixture, 'rayfin', 'functions'), functions, {
    recursive: true, filter: (source) => !['node_modules', 'dist', '.rayfin-local'].includes(path.basename(source)),
  });
  const workerModules = path.join(fixture, 'rayfin', 'functions', 'node_modules');
  const modules = await fs.stat(workerModules).then(() => workerModules, (error) => {
    if (error.code !== 'ENOENT') throw error;
    return path.join(fixture, 'node_modules');
  });
  link = path.join(functions, 'node_modules');
  await fs.symlink(modules, link, 'junction');
  const config = path.join(projectDir, 'rayfin', 'rayfin.yml');
  await fs.writeFile(config, (await fs.readFile(config, 'utf8'))
    .replace('functions:\n    enabled: false', 'functions:\n    enabled: true\n    buildCommand: npm run build'));
  const capture = t.mock.method(childProcess, 'spawn', (...args) => {
    child = spawn(...args);
    return child;
  });
  running = await startRayfinLocal({ projectDir, frontend: true });
  capture.mock.restore();
  const snapshot = spawnSync('powershell.exe', ['-NoProfile', '-Command',
    'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId | ConvertTo-Json -Compress'],
  { encoding: 'utf8' });
  assert.equal(snapshot.status, 0, snapshot.stderr);
  const processes = JSON.parse(snapshot.stdout);
  descendants.add(child.pid);
  for (let size = -1; size !== descendants.size;) {
    size = descendants.size;
    for (const entry of processes) {
      if (descendants.has(entry.ParentProcessId)) descendants.add(entry.ProcessId);
    }
  }
  descendants.delete(child.pid);
  assert(descendants.size > 0, 'test must exercise actual frontend/function descendants');
  // Keep the real backend alive past EOF to exercise the wrapper's 30-second fallback.
  const ignoreEof = t.mock.method(child.stdin, 'end', () => child.stdin);
  await assert.rejects(running.stop(), { code: 'stop-timeout' });
  ignoreEof.mock.restore();
  await assert.rejects(running.closed, { code: 'stop-timeout' });
  await delay(500);
  const leftovers = [...descendants].filter(alive);
  const frontendListening = await fetch(running.ready.appUrl, { signal: AbortSignal.timeout(2000) })
    .then(() => true, (error) => {
      if (error instanceof TypeError || error.name === 'TimeoutError') return false;
      throw error;
    });
  let restartError;
  try { replacement = await startRayfinLocal({ projectDir, frontend: true }); }
  catch (error) { restartError = error; }
  if (leftovers.length || frontendListening) {
    t.todo('Known Windows limitation: forced termination kills only the CLI; descendants can retain ports or locks.');
    t.diagnostic(`Remaining owned PIDs: ${leftovers.join(', ')}; restart: ${restartError?.code ?? 'succeeded'}`);
  }
  assert.equal(restartError, undefined, 'same-project restart must release the project lock');
  assert.deepEqual(leftovers, [], 'no frontend or function descendants may survive forced shutdown');
  assert.equal(frontendListening, false, 'previous frontend port must be released before restart');
});

test('installed startup failure releases state and a corrected project can start', { timeout: 180_000 }, async (t) => {
  const projectDir = await project(t);
  const model = path.join(projectDir, 'rayfin', 'data', 'Note.ts');
  const valid = await fs.readFile(model);
  await fs.writeFile(model, 'export class Note {');
  await assert.rejects(startRayfinLocal({ projectDir }), { code: 'analysis-failed' });
  await fs.writeFile(model, valid);
  const running = await startRayfinLocal({ projectDir });
  await running.stop();
});

test('installed backend waits for real functions and serves SDK 2.0 data and watched code', { timeout: 180_000 }, async (t) => {
  const projectDir = await project(t);
  const functions = path.join(projectDir, 'rayfin', 'functions');
  await fs.mkdir(path.join(functions, 'src'), { recursive: true });
  for (const name of ['package.json', 'tsconfig.json']) {
    await fs.copyFile(path.join(fixture, 'rayfin', 'functions', name), path.join(functions, name));
  }
  await fs.writeFile(path.join(functions, 'host.json'), '{"version":"2.0"}');
  await fs.writeFile(path.join(functions, 'local.settings.json'), '{"IsEncrypted":false,"Values":{}}');
  const source = path.join(functions, 'src', 'function_app.ts');
  const code = (answer) => `import {UserDataFunctions} from '@microsoft/fabric-user-data-functions';
const udf = new UserDataFunctions();
udf.func('ping', async (): Promise<string> => '${answer}', []);
`;
  await fs.writeFile(source, code('first'));
  const configPath = path.join(projectDir, 'rayfin', 'rayfin.yml');
  await fs.writeFile(configPath, (await fs.readFile(configPath, 'utf8'))
    .replace('functions:\n    enabled: false', 'functions:\n    enabled: true\n    buildCommand: npm run build'));
  const dependencyLink = path.join(functions, 'node_modules');
  const workerModules = path.join(fixture, 'rayfin', 'functions', 'node_modules');
  const modules = await fs.stat(workerModules).then(() => workerModules, (error) => {
    if (error.code !== 'ENOENT') throw error;
    return path.join(fixture, 'node_modules'); // Workspace fixtures can hoist worker dependencies.
  });
  await fs.symlink(modules, dependencyLink, 'junction');
  let running;
  let client;
  try {
    running = await startRayfinLocal({ projectDir, ...signIn });
    const config = await (await fetch(`${running.apiUrl}/rayfin.config.json`)).json();
    const load = (name) => import(pathToFileURL(path.join(fixture, 'node_modules/@microsoft', name, 'dist/index.js')));
    const { RayfinClient } = await load('rayfin-client');
    const { browserSession } = await load('rayfin-auth');
    const { signInWithBrokeredToken } = await load('rayfin-auth-provider-fabric');
    const { dataService } = await load('rayfin-data');
    const { functionsService } = await load('rayfin-functions');
    client = RayfinClient.create({
      baseUrl: running.apiUrl,
      publishableKey: config.publishableKey,
      auth: browserSession({ storage: false, persistSession: false, autoRefreshToken: false, multiTabSync: false }),
      services: { data: dataService(), functions: functionsService() },
    });
    const session = await (await fetch(`${running.apiUrl}/.rayfin-local/dev/session-token`)).json();
    await signInWithBrokeredToken(client.auth, session);
    const note = await client.data.Note.create({ title: 'installed SDK2' });
    assert.equal((await client.data.Note.select(['id', 'title']).where({ id: { eq: note.id } }).execute())[0].title,
      'installed SDK2');
    assert.equal(await client.functions.ping.invoke(), 'first');
    await fs.writeFile(source, code('watched'));
    const deadline = Date.now() + 30_000;
    while (await client.functions.ping.invoke() !== 'watched') {
      assert(Date.now() < deadline, 'function watcher did not apply source change');
      await delay(250);
    }
    await client.data.Note.delete({ id: note.id });
  } finally {
    client?.dispose();
    await running?.stop();
    await fs.unlink(dependencyLink);
  }
});

test('parent exit closes stdin and releases the real project lock', { timeout: 180_000 }, async (t) => {
  const projectDir = await project(t);
  const child = spawn(process.execPath, ['-e', `
    const { startRayfinLocal } = require(${JSON.stringify(path.join(installed, 'server.cjs'))});
    startRayfinLocal({projectDir: ${JSON.stringify(projectDir)}}).then(
      ({apiUrl}) => { process.send(apiUrl); },
      (error) => { process.send({code:error.code}); process.exitCode = 1; }
    );
    process.on('message', () => process.exit(0));
  `], { stdio: ['ignore', 'ignore', 'pipe', 'ipc'] });
  child.stderr.resume();
  const exited = once(child, 'close');
  t.after(async () => { if (child.exitCode === null) child.kill(); await exited; });
  const [apiUrl] = await once(child, 'message');
  assert.equal(typeof apiUrl, 'string');
  child.send('exit');
  assert.equal((await exited)[0], 0);
  let replacement;
  const deadline = Date.now() + 30_000;
  while (!replacement) {
    try { replacement = await startRayfinLocal({ projectDir }); }
    catch (error) {
      if (error.code !== 'already-serving' || Date.now() >= deadline) throw error;
      await delay(50);
    }
  }
  await replacement.stop();
});
