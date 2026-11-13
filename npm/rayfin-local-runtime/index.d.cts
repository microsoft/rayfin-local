export type RuntimeState = 'created' | 'starting' | 'running' | 'stopping' | 'stopped';

export interface RuntimeOptions {
  projectDir: string;
  stateDir?: string;
  /** Defaults to process.execPath. Electron requires an explicit standalone Node executable. */
  nodePath?: string;
  server?: { host?: string; port?: number };
  frontend?: { mode?: 'dev' | 'static' | 'off'; port?: number; reuseBuild?: boolean };
  functions?: { enabled?: boolean };
  forceSchema?: boolean;
  seed?: boolean;
  verbose?: boolean;
}

export interface RuntimeEvent {
  v: number;
  type: string;
  time: string;
  [key: string]: unknown;
}

export interface ReadyInfo extends RuntimeEvent {
  type: 'ready';
  apiUrl: string;
  appUrl: string | null;
  embedUrl: string | null;
  portalUrl: string | null;
  dataDir: string;
}

export interface RuntimeFailure {
  code: string;
  message: string;
  hint?: string | null;
  details: Record<string, unknown>;
}

export interface StopInfo {
  reason: 'closed' | 'error';
  error: RuntimeFailure | null;
  cleanupErrors: string[];
}

export interface RuntimeStatus {
  state: RuntimeState;
  [key: string]: unknown;
}

export declare class RayfinRuntimeError extends Error {
  constructor(info: RuntimeFailure, options?: ErrorOptions);
  code: string;
  hint?: string;
  details: Record<string, unknown>;
}

export interface LocalRuntime {
  readonly state: RuntimeState;
  readonly closed: Promise<StopInfo>;
  start(options?: { signal?: AbortSignal }): Promise<ReadyInfo>;
  status(): Promise<RuntimeStatus>;
  reload(options?: { forceSchema?: boolean }): Promise<{ schema: RuntimeEvent; config: RuntimeEvent | null }>;
  close(): Promise<StopInfo>;
  /** Callbacks run on the JS thread. Exceptions issue a process warning; delivery continues. */
  onEvent(listener: (event: RuntimeEvent) => void): () => void;
}

export declare function createRuntime(options: RuntimeOptions): LocalRuntime;
