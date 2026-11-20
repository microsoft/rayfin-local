export interface ManagedDev {
  readonly apiUrl: string;
  /** Idempotent; resolves after backend exit and project-lock release. Rejects on failed shutdown. */
  stop(): Promise<void>;
}

export interface ManagedDevError extends Error {
  readonly name: 'RayfinManagedDevError';
  readonly code: string;
  readonly eventType?: string;
  readonly fatal?: boolean;
  readonly exitCode?: number | null;
  readonly signal?: string | null;
}

/** Starts the packaged CLI, not the native addon. Requires installed Rayfin project dependencies. */
export function startRayfinLocal(options: { projectDir: string }): Promise<ManagedDev>;
