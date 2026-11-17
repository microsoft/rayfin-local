import type { SuiteInfo } from './suites.ts';

/*
 * The suite's control API (`<api>/.rayfin-local/control`, crates/rayfin-local/src/control.rs),
 * authorized with the suite's `RAYFIN_LOCAL_CONTROL_TOKEN`.
 */

/** Calls `<api>/.rayfin-local/control/<path>` and returns the JSON answer; throws unless 2xx. */
export async function control(info: SuiteInfo, method: string, path: string, body?: unknown): Promise<unknown> {
  if (!info.controlToken) throw new Error(`The ${info.name} suite has no control token.`);
  const response = await fetch(`${info.serve.apiUrl}/.rayfin-local/control/${path}`, {
    method,
    headers: {
      authorization: ['Bearer', info.controlToken].join(' '),
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${method} control/${path} answered ${response.status}: ${text}`);
  return text ? JSON.parse(text) : null;
}

/**
 * Who the dev server signs apps in as (`[auth] sign_in_as`): the suites start with `none`, so
 * the app signs in through the popup; `developer` is what Rayfin's templates expect. Switching
 * signs out every session.
 */
export async function signInAs(info: SuiteInfo, kind: 'developer' | 'none'): Promise<void> {
  await control(info, 'PUT', 'identity', { kind });
}
