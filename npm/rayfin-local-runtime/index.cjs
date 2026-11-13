'use strict';

const path = require('node:path');
const { artifact } = require('./resolve.cjs');
const { version } = require('./package.json');

class RayfinRuntimeError extends Error {
  constructor(info, options) {
    super(info.message, options);
    this.name = 'RayfinRuntimeError';
    this.code = info.code;
    this.hint = info.hint ?? undefined;
    this.details = info.details ?? {};
  }
}

function nativeError(cause) {
  let info;
  try {
    info = JSON.parse(cause.message);
  } catch {
    return cause;
  }
  return new RayfinRuntimeError(info, { cause });
}

const finalizers = new FinalizationRegistry(({ native, timer }) => {
  clearInterval(timer);
  native.requestClose();
});

class LocalRuntime {
  #native;
  #listeners = new Set();
  #timer;
  #resolveClosed;
  #start;
  #close;
  #finalizerToken = {};

  constructor(native) {
    this.#native = native;
    this.closed = new Promise((resolve) => { this.#resolveClosed = resolve; });
  }

  get state() { return this.#native.state; }

  onEvent(listener) {
    if (typeof listener !== 'function') throw new TypeError('onEvent requires a function.');
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #pump() {
    for (const event of JSON.parse(this.#native.drainEvents())) {
      for (const listener of [...this.#listeners]) {
        try {
          listener(event);
        } catch (error) {
          process.emitWarning(error instanceof Error ? error : String(error), { type: 'RayfinRuntimeListenerWarning' });
        }
      }
    }
    if (this.state === 'stopped' && !this.#close) this.close();
  }

  #watch() {
    if (this.#timer) return;
    const weak = new WeakRef(this);
    this.#timer = setInterval(() => weak.deref()?.#pump(), 20);
    finalizers.register(this, { native: this.#native, timer: this.#timer }, this.#finalizerToken);
  }

  start({ signal } = {}) {
    if (this.state === 'stopped' || this.state === 'stopping') {
      return Promise.reject(new RayfinRuntimeError({ code: 'runtime-stopped', message: 'A stopped runtime cannot be restarted; create a new runtime.' }));
    }
    if (this.#start) return this.#start;
    if (signal && typeof signal.addEventListener !== 'function') {
      throw new TypeError('signal must be an AbortSignal.');
    }
    const abort = () => this.#native.requestClose();
    this.#watch();
    if (signal?.aborted) abort();
    else signal?.addEventListener('abort', abort, { once: true });
    this.#start = this.#native.start()
      .then(JSON.parse, (error) => { throw nativeError(error); })
      .finally(() => {
        signal?.removeEventListener('abort', abort);
        this.#pump();
      });
    return this.#start;
  }

  async status() { return JSON.parse(this.#native.status()); }

  async reload({ forceSchema = false } = {}) {
    if (typeof forceSchema !== 'boolean') throw new TypeError('forceSchema must be a boolean.');
    try {
      return JSON.parse(await this.#native.reload(forceSchema));
    } catch (error) {
      throw nativeError(error);
    }
  }

  close() {
    if (!this.#close) {
      this.#close = this.#native.close().then((line) => {
        const info = JSON.parse(line);
        this.#pump();
        clearInterval(this.#timer);
        finalizers.unregister(this.#finalizerToken);
        this.#resolveClosed(info);
        return info;
      });
    }
    return this.#close;
  }
}

function createRuntime(options) {
  if (!options || typeof options.projectDir !== 'string' || !options.projectDir) {
    throw new TypeError('projectDir must name a Rayfin project.');
  }
  const resolved = { ...options, projectDir: path.resolve(options.projectDir) };
  if (options.stateDir !== undefined) {
    if (typeof options.stateDir !== 'string' || !options.stateDir) throw new TypeError('stateDir must be a path.');
    resolved.stateDir = path.resolve(options.stateDir);
  }
  if (options.nodePath !== undefined && (typeof options.nodePath !== 'string' || !path.isAbsolute(options.nodePath))) {
    throw new TypeError('nodePath must be an absolute path to Node.js.');
  }
  resolved.nodePath = options.nodePath ?? process.execPath;
  if (process.versions.electron && options.nodePath === undefined) {
    throw new TypeError('Electron hosts must supply nodePath pointing to a standalone Node.js executable.');
  }
  const addon = require(artifact('rayfin_local.node'));
  if (addon.version() !== version) throw new Error('Rayfin runtime addon version does not match the JavaScript package.');
  try {
    return new LocalRuntime(new addon.NativeRuntime(JSON.stringify(resolved)));
  } catch (error) {
    throw nativeError(error);
  }
}

exports.createRuntime = createRuntime;
exports.RayfinRuntimeError = RayfinRuntimeError;
