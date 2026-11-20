'use strict';

const childProcess = require('node:child_process');
const path = require('node:path');

const START_TIMEOUT_MS = 120_000;
const STOP_TIMEOUT_MS = 30_000;
const MAX_LINE_LENGTH = 1024 * 1024;
const active = new Set();
const signals = { SIGINT: 130, SIGTERM: 143 };
let shuttingDown = false;

function failure(code, message, metadata = {}) {
  return Object.assign(new Error(message), { name: 'RayfinManagedDevError', code, ...metadata });
}

function onExit() {
  for (const instance of active) instance.end();
}

async function onSignal(signal) {
  if (shuttingDown) return;
  shuttingDown = true;
  const stopped = await Promise.allSettled([...active].map((instance) => instance.stop()));
  if (stopped.some((result) => result.status === 'rejected')) {
    process.emitWarning('Rayfin managed backend shutdown failed.', { code: 'rayfin-stop-failed' });
  }
  shuttingDown = false;
  // Do not replace a host's shutdown policy. With no host handler, preserve signal exit semantics.
  if (process.listenerCount(signal) === 0) process.exit(signals[signal]);
}

const signalHandlers = Object.fromEntries(
  Object.keys(signals).map((signal) => [signal, () => { void onSignal(signal); }]),
);

function register(instance) {
  if (active.size === 0) {
    process.on('exit', onExit);
    for (const [signal, handler] of Object.entries(signalHandlers)) process.on(signal, handler);
  }
  active.add(instance);
}

function unregister(instance) {
  active.delete(instance);
  if (active.size === 0) {
    process.removeListener('exit', onExit);
    for (const [signal, handler] of Object.entries(signalHandlers)) process.removeListener(signal, handler);
  }
}

function readyUrl(event) {
  if (event.v !== 1 || event.openSignIn !== true || event.appUrl !== null || event.embedUrl !== null) {
    throw failure('invalid-ready', 'Rayfin did not report the managed backend readiness contract.');
  }
  let url;
  try { url = new URL(event.apiUrl); }
  catch { throw failure('invalid-ready', 'Rayfin reported an invalid backend URL.'); }
  if (url.protocol !== 'http:' || !['localhost', '127.0.0.1'].includes(url.hostname)
    || !url.port || Number(url.port) === 0 || url.username || url.password
    || url.search || url.hash || event.apiUrl !== url.origin) {
    throw failure('invalid-ready', 'Rayfin must report a direct HTTP loopback origin with an allocated port.');
  }
  if (event.sdk?.supported !== true) {
    throw failure('unsupported-sdk', 'The app SDK is not supported by this Rayfin runtime.');
  }
  return url.origin;
}

/**
 * Start this package's CLI as a managed backend. The returned stop promise settles only after exit.
 * @param {{projectDir: string}} options
 * @returns {Promise<{apiUrl: string, stop(): Promise<void>}>}
 */
async function startRayfinLocal(options) {
  if (!options || typeof options.projectDir !== 'string' || !options.projectDir.trim()
    || Object.keys(options).some((key) => key !== 'projectDir')) {
    throw failure('config-invalid', 'Supply startRayfinLocal({projectDir}) with a nonempty project directory.');
  }
  if (shuttingDown) throw failure('host-stopping', 'The host is shutting down.');
  const projectDir = path.resolve(options.projectDir);
  let binaryPath;
  try { ({ binaryPath } = require('./binary.cjs')); }
  catch (cause) {
    throw failure('binary-unavailable', 'Install the matching Rayfin runtime platform package.', { cause });
  }
  const env = { ...process.env, RAYFIN_LOCAL_NODE: process.execPath };
  // Host-owned admin credentials/state must not leak into an app backend or its children.
  for (const key of ['RAYFIN_LOCAL_CONTROL_TOKEN', 'RAYFIN_LOCAL_SIGN_IN_SECRET', 'RAYFIN_LOCAL_STATE_DIR']) {
    delete env[key];
  }
  let child;
  try {
    child = childProcess.spawn(binaryPath, [
      'serve', '--json', '--no-frontend', '--open-sign-in', '-C', projectDir,
      '--host', '127.0.0.1', '--port', '0', '--sign-in-as', 'user:managed-dev@example.test',
    ], { env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  } catch (cause) {
    throw failure('spawn-failed', 'Could not start the packaged Rayfin executable.', { cause });
  }
  let resolveStart;
  let rejectStart;
  const started = new Promise((resolve, reject) => { resolveStart = resolve; rejectStart = reject; });
  let resolveExit;
  const exited = new Promise((resolve) => { resolveExit = resolve; });
  let settled = false;
  let closed = false;
  let stopping = false;
  let stopPromise;
  let error;
  let timer;
  let stopTimer;
  let buffer = '';
  let functionsReady = false;

  function end() {
    if (!child.stdin.destroyed) child.stdin.end();
  }
  function stop() {
    if (stopPromise) return stopPromise;
    stopping = true;
    clearTimeout(timer);
    end();
    if (!closed) {
      stopTimer = setTimeout(() => {
        error ??= failure('stop-timeout', 'Rayfin did not stop after stdin closed; forced termination was requested.');
        child.kill('SIGKILL');
      }, STOP_TIMEOUT_MS);
    }
    stopPromise = exited.then(() => {
      if (error) throw error;
    });
    return stopPromise;
  }
  const instance = { stop, end };
  function fail(reason) {
    error ??= reason;
    if (settled && !stopping) {
      process.emitWarning('The Rayfin managed backend failed; restart the app dev server.', { code: error.code });
    }
    // Startup rejection is delayed until the child releases its listeners and project lock.
    void stop().catch(() => {});
  }
  function event(value) {
    if (!value || value.v !== 1 || typeof value.type !== 'string') {
      throw failure('invalid-event', 'Rayfin emitted an unsupported event envelope.');
    }
    if (value.type === 'error' && value.fatal) {
      throw failure(typeof value.code === 'string' ? value.code : 'backend-error',
        'Rayfin reported a fatal backend error.', { eventType: 'error', fatal: true });
    }
    if (!settled && !stopping) {
      if (value.type === 'analysis' && value.ok === false) {
        throw failure('analysis-failed', 'Rayfin could not analyze the app.');
      }
      if (value.type === 'schema' && ['failed', 'refused'].includes(value.result)) {
        throw failure(`schema-${value.result}`, 'Rayfin could not apply the app schema.');
      }
      if (value.type === 'functions') {
        functionsReady = value.state === 'ready';
        if (value.state === 'failed') throw failure('functions-failed', 'The app functions failed to build.');
      }
      if (value.type === 'ready') {
        const apiUrl = readyUrl(value);
        if (value.features?.functions?.enabled && !functionsReady) {
          throw failure('functions-not-ready', 'Rayfin announced readiness before app functions were ready.');
        }
        settled = true;
        clearTimeout(timer);
        resolveStart({ apiUrl, stop });
      }
    } else if (settled && !stopping
      && ((value.type === 'error') || (value.type === 'analysis' && !value.ok)
        || (value.type === 'functions' && value.state === 'failed')
        || (value.type === 'schema' && ['failed', 'refused'].includes(value.result)))) {
      process.emitWarning('Rayfin reported an app reload failure; correct the app and retry.', {
        code: 'rayfin-reload-failed',
      });
    } else if (settled && !stopping && value.type === 'config-changed' && value.restartRequired) {
      process.emitWarning('Rayfin configuration changed; restart the app dev server to apply it.', {
        code: 'rayfin-restart-required',
      });
    }
  }
  register(instance);
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', (data) => {
    if (stopping) return;
    buffer += data;
    try {
      let newline;
      while ((newline = buffer.indexOf('\n')) !== -1) {
        if (newline > MAX_LINE_LENGTH) throw failure('invalid-event', 'Rayfin emitted an oversized event.');
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        if (line.trim()) {
          let parsed;
          try { parsed = JSON.parse(line); }
          catch { throw failure('invalid-event', 'Rayfin stdout was not valid NDJSON.'); }
          event(parsed);
        }
      }
      if (buffer.length > MAX_LINE_LENGTH) throw failure('invalid-event', 'Rayfin emitted an oversized event.');
    } catch (reason) { fail(reason); }
  });
  // Do not forward arbitrary child logs: function output may contain application credentials.
  child.stderr.resume();
  child.stdin.on('error', () => {
    if (!stopping) fail(failure('stdin-failed', 'The Rayfin control pipe closed unexpectedly.'));
  });
  child.stdout.on('error', () => fail(failure('stdout-failed', 'Could not read Rayfin events.')));
  child.stderr.on('error', () => fail(failure('stderr-failed', 'Could not drain Rayfin diagnostics.')));
  child.on('error', () => fail(failure('spawn-failed', 'Could not run the packaged Rayfin executable.')));
  child.on('close', (exitCode, signal) => {
    closed = true;
    clearTimeout(timer);
    clearTimeout(stopTimer);
    unregister(instance);
    if (!stopping || exitCode !== 0 || signal) {
      error ??= failure('backend-exited', 'Rayfin exited unexpectedly.', { exitCode, signal });
    }
    if (!settled) {
      settled = true;
      rejectStart(error ?? failure('startup-stopped', 'Rayfin stopped before becoming ready.'));
    } else if (!stopping) {
      process.emitWarning('The Rayfin managed backend exited; restart the app dev server.', {
        code: error.code,
      });
    }
    resolveExit();
  });
  timer = setTimeout(() => fail(failure('startup-timeout', 'Rayfin did not become ready within 120 seconds.')), START_TIMEOUT_MS);
  return started;
}

exports.startRayfinLocal = startRayfinLocal;
