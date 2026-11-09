import assert from 'node:assert/strict';
import { webcrypto } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const script = readFileSync(
  new URL('../../crates/rl-server/src/static_signin.js', import.meta.url), 'utf8',
);

function page() {
  const handlers = {};
  const requests = [];
  const messages = [];
  const stored = new Map();
  const config = {
    portalUrl: 'http://localhost:7171/.rayfin-local/portal',
    workspaceId: 'ws', itemId: 'item', publishableKey: 'pk-local',
  };
  const elements = {
    'rl-static-config': { textContent: JSON.stringify(config) },
    signin: { addEventListener: (type, handler) => { handlers[type] = handler; } },
    status: {},
  };
  const popup = {
    location: {}, closed: false,
    close() { this.closed = true; },
    postMessage: (data, origin) => messages.push({ data, origin }),
  };
  let reloads = 0;
  const origin = 'http://localhost:7171';
  const token = `eyJhbGciOiJub25lIn0.${Buffer.from(JSON.stringify({
    sub: 'user-id', email: 'user@example.test',
  })).toString('base64url')}.signature`;
  const context = {
    document: { getElementById: (id) => elements[id] },
    window: {
      crypto: webcrypto, open: () => popup,
      addEventListener: (type, handler) => { handlers[type] = handler; },
    },
    crypto: webcrypto, URL, URLSearchParams, TextEncoder, Uint8Array, btoa, atob,
    location: { origin, reload: () => { reloads += 1; } },
    // The SDK's default session store; sessionStorage is deliberately absent.
    localStorage: { setItem: (key, value) => stored.set(key, value) },
    fetch: async (url, options) => {
      requests.push({ url, options });
      return {
        ok: true,
        json: async () => ({ accessToken: token, refreshToken: 'refresh', expiresIn: 3600 }),
      };
    },
  };
  vm.runInNewContext(script, context);
  return { handlers, requests, messages, stored, popup, origin, elements, token,
    reloads: () => reloads };
}

test('protected-site sign-in completes PKCE, creates the hosting cookie, and reloads', async () => {
  const p = page();
  await p.handlers.click();
  assert.match(p.popup.location.href, /secureItemEmbed/);
  await p.handlers.message({ source: p.popup, origin: 'https://attacker.example',
    data: { type: 'brokeredAuth.ready' } });
  assert.equal(p.messages.length, 0);
  await p.handlers.message({ source: p.popup, origin: p.origin,
    data: { type: 'brokeredAuth.ready' } });
  const challenge = p.messages[0].data;
  assert.equal(challenge.returnOrigin, p.origin);
  await p.handlers.message({ source: p.popup, origin: p.origin,
    data: { type: 'brokeredAuth.handoff', state: 'wrong', handoffCode: 'stolen' } });
  assert.equal(p.requests.length, 0);
  await p.handlers.message({ source: p.popup, origin: p.origin,
    data: { type: 'brokeredAuth.handoff', state: challenge.state, handoffCode: 'handoff' } });
  assert.equal(p.requests.length, 2);
  const redemption = JSON.parse(p.requests[0].options.body);
  assert.equal(redemption.grantType, 'authorization_code');
  assert.equal(redemption.verificationCode, 'handoff');
  assert.equal(redemption.codeType, 'fabric_handoff');
  const digest = await webcrypto.subtle.digest('SHA-256', new TextEncoder().encode(redemption.codeVerifier));
  assert.equal(Buffer.from(digest).toString('base64url'), challenge.codeChallenge);
  assert.equal(p.requests[1].url, '/.rayfin-local/static-session');
  assert.equal(p.requests[1].options.credentials, 'same-origin');
  assert.equal(p.requests[1].options.headers.authorization, `Bearer ${p.token}`);
  assert.equal(JSON.parse(p.stored.get('authSession')).user.id, 'user-id');
  assert.equal(p.reloads(), 1);
  assert.equal(p.popup.closed, true);
});

test('a refused popup does not install a session or reload the protected app', async () => {
  const p = page();
  await p.handlers.click();
  await p.handlers.message({ source: p.popup, origin: p.origin,
    data: { type: 'brokeredAuth.ready' } });
  await p.handlers.message({ source: p.popup, origin: p.origin,
    data: { type: 'brokeredAuth.error', state: p.messages[0].data.state } });
  assert.equal(p.requests.length, 0);
  assert.equal(p.reloads(), 0);
  assert.equal(p.elements.signin.disabled, false);
});
