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
  return Object.assign(new Error(message), { name: 'RayfinServerError', code, ...metadata });
}

function onExit() {
  for (const instance of active) instance.end();
}

async function onSignal(signal) {
  if (shuttingDown) return;
  shuttingDown = true;
  const stopped = await Promise.allSettled([...active].map((instance) => instance.stop()));
  if (stopped.some((result) => result.status === 'rejected')) {
    process.emitWarning('Rayfin server shutdown failed.', { code: 'rayfin-stop-failed' });
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

function readyUrl(event, options) {
  if (event.v !== 1 || event.openSignIn !== options.openSignIn
    || (!options.frontend && (event.appUrl !== null || event.embedUrl !== null))
    || (options.frontend && typeof event.appUrl !== 'string')) {
    throw failure('invalid-ready', 'Rayfin readiness does not match the requested server settings.');
  }
  let url;
  try { url = new URL(event.apiUrl); }
  catch { throw failure('invalid-ready', 'Rayfin reported an invalid backend URL.'); }
  if (url.protocol !== 'http:' || !['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
    || !url.port || Number(url.port) === 0 || url.username || url.password
    || url.search || url.hash || event.apiUrl !== url.origin
    || (options.port !== 0 && Number(url.port) !== options.port)) {
    throw failure('invalid-ready', 'Rayfin must report a direct HTTP loopback origin with an allocated port.');
  }
  if (event.sdk?.supported !== true) {
    throw failure('unsupported-sdk', 'The app SDK is not supported by this Rayfin runtime.');
  }
  const identity = event.signInAs;
  if (options.signInAs.startsWith('user:')
    ? identity?.kind !== 'user' || identity.username !== options.signInAs.slice(5)
    : identity?.kind !== options.signInAs) {
    throw failure('invalid-ready', 'Rayfin reported a different sign-in identity than requested.');
  }
  return url.origin;
}

function settings(input) {
  const keys = ['projectDir', 'frontend', 'host', 'port', 'signInAs', 'openSignIn'];
  const options = { frontend: false, host: '127.0.0.1', port: 0, signInAs: 'none', openSignIn: false, ...input };
  if (!input || typeof input.projectDir !== 'string' || !input.projectDir.trim()
    || Object.keys(input).some((key) => !keys.includes(key))
    || typeof options.frontend !== 'boolean' || typeof options.openSignIn !== 'boolean'
    || !['localhost', '127.0.0.1', '::1'].includes(options.host)
    || !Number.isInteger(options.port) || options.port < 0 || options.port > 65535
    || typeof options.signInAs !== 'string'
    || (!['none', 'developer'].includes(options.signInAs)
      && !/^user:\S[^\r\n\0]*$/.test(options.signInAs))) {
    throw failure('config-invalid', 'Supply a project directory and valid Rayfin frontend, loopback host, port and sign-in options.');
  }
  return options;
}

/**
 * Start this package's CLI. The returned stop promise settles only after exit.
 * @param {import('./server.cjs').ServerOptions} input
 * @returns {Promise<import('./server.cjs').RayfinServer>}
 */
async function startRayfinLocal(input) {
  const options = settings(input);
  if (shuttingDown) throw failure('host-stopping', 'The host is shutting down.');
  const projectDir = path.resolve(options.projectDir);
  let binaryPath;
  try { ({ binaryPath } = require('./binary.cjs')); }
  catch (cause) {
    throw failure('binary-unavailable', 'Could not find a packaged Rayfin executable for this platform.', { cause });
  }
  const env = { ...process.env, RAYFIN_LOCAL_NODE: process.execPath };
  // Host-owned admin credentials/state must not leak into an app backend or its children.
  for (const key of [
    'RAYFIN_LOCAL_CONTROL_TOKEN', 'RAYFIN_LOCAL_SIGN_IN_SECRET',
    'RAYFIN_LOCAL_STATE_DIR', 'RAYFIN_LOCAL_OPEN_SIGN_IN',
  ]) {
    delete env[key];
  }
  let child;
  try {
    child = childProcess.spawn(binaryPath, [
      'serve', '--json', '-C', projectDir,
      '--host', options.host, '--port', String(options.port), '--sign-in-as', options.signInAs,
      ...(!options.frontend ? ['--no-frontend'] : []),
      ...(options.openSignIn ? ['--open-sign-in'] : []),
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
      process.emitWarning('The Rayfin server failed; stop and restart it.', { code: error.code });
    }
    // Startup rejection is delayed until the child releases its listeners and project lock.
    void stop().catch(rejectStart);
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
        const apiUrl = readyUrl(value, options);
        if (value.features?.functions?.enabled && !functionsReady) {
          throw failure('functions-not-ready', 'Rayfin announced readiness before app functions were ready.');
        }
        settled = true;
        clearTimeout(timer);
        resolveStart({ ready: value, apiUrl, stop });
      }
    } else if (settled && !stopping
      && ((value.type === 'error') || (value.type === 'analysis' && !value.ok)
        || (value.type === 'functions' && value.state === 'failed')
        || (value.type === 'schema' && ['failed', 'refused'].includes(value.result)))) {
      process.emitWarning('Rayfin reported an app reload failure; correct the app and retry.', {
        code: 'rayfin-reload-failed',
      });
    } else if (settled && !stopping && value.type === 'config-changed' && value.restartRequired) {
      process.emitWarning('Rayfin configuration changed; stop and restart the server to apply it.', {
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
      process.emitWarning('The Rayfin server exited; stop and restart it.', {
        code: error.code,
      });
    }
    resolveExit();
  });
  timer = setTimeout(() => fail(failure('startup-timeout', 'Rayfin did not become ready within 120 seconds.')), START_TIMEOUT_MS);
  return started;
}

exports.startRayfinLocal = startRayfinLocal;
