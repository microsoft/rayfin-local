import type { ReadyInfo, RuntimeEvent } from './events.cjs';

export interface ServerOptions {
  projectDir: string;
  /** Resolved relative to the host working directory. Unset preserves CLI/env precedence. */
  stateDir?: string;
  /** Allow destructive schema changes. Default false. */
  forceSchema?: boolean;
  /** Empty database, users and stored files on startup. Default false. */
  reset?: boolean;
  /** Receives versioned CLI events, including reload failures and restart-required changes.
   * Throwing callbacks issue a warning without interrupting event delivery. */
  onEvent?: (event: RuntimeEvent) => void;
  /** Run Rayfin's frontend dev server. Default false (backend only). */
  frontend?: boolean;
  /** Listen only on loopback. Default '127.0.0.1'. */
  host?: '127.0.0.1' | 'localhost' | '::1';
  /** Backend port; 0 selects an available port. Default 0. */
  port?: number;
  /** Override automatic identity. Unset follows project configuration and the CLI default. */
  signInAs?: 'none' | 'developer' | `user:${string}`;
  /** Permit any local process to mint sessions without a secret. Default false; explicit opt-in. */
  openSignIn?: boolean;
}

export interface ServerReady extends ReadyInfo {
  v: 1;
  openSignIn: boolean;
  signInAs: { kind: 'none' | 'developer' } | { kind: 'user'; username: string };
  sdk: { version: string | null; supported: true };
}

export interface RayfinServer {
  readonly ready: ServerReady;
  readonly apiUrl: string;
  /** Settles after exit: resolves on clean stop, rejects on failure. Safe to leave unobserved. */
  readonly closed: Promise<void>;
  /** Idempotent; resolves after backend exit and project-lock release. Rejects on failed shutdown. */
  stop(): Promise<void>;
}

export interface RayfinServerError extends Error {
  readonly name: 'RayfinServerError';
  readonly code: string;
  readonly eventType?: string;
  readonly fatal?: boolean;
  readonly exitCode?: number | null;
  readonly signal?: string | null;
}

/** Starts the packaged CLI, not the native addon. Requires installed Rayfin project dependencies. */
export function startRayfinLocal(options: ServerOptions): Promise<RayfinServer>;
