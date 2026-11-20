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
