import type { ReadyInfo } from './index.cjs';

export interface ServerOptions {
  projectDir: string;
  /** Run Rayfin's frontend dev server. Default false (backend only). */
  frontend?: boolean;
  /** Listen only on loopback. Default '127.0.0.1'. */
  host?: '127.0.0.1' | 'localhost' | '::1';
  /** Backend port; 0 selects an available port. Default 0. */
  port?: number;
  /** Automatic sign-in identity. Default 'none'; never implicitly selects a developer identity. */
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
