'use strict';

const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
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

test('default server does not opt into open sign-in or an automatic identity', { timeout: 120_000 }, async (t) => {
  const projectDir = await project(t);
  const old = process.env.RAYFIN_LOCAL_OPEN_SIGN_IN;
  process.env.RAYFIN_LOCAL_OPEN_SIGN_IN = 'true';
  let running;
  try {
    running = await startRayfinLocal({ projectDir });
    assert.equal(running.ready.openSignIn, false);
    assert.deepEqual(running.ready.signInAs, { kind: 'none' });
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
