import assert from 'node:assert/strict';
import { webcrypto } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const script = readFileSync(
  new URL('../../crates/rl-server/src/static_signin.js', import.meta.url), 'utf8',
);

function page({ embedded = false, href = 'http://localhost:7171/?fabricEmbedded=true' } = {}) {
  const handlers = {};
  const requests = [];
  const messages = [];
  const posted = [];
  const stored = new Map();
  const session = new Map();
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
  const parent = { postMessage: (data, target) => posted.push({ data, target }) };
  const self = {};
  const context = {
    document: { getElementById: (id) => elements[id] },
    window: {
      crypto: webcrypto, open: () => popup,
      self, top: embedded ? parent : self, parent: embedded ? parent : self,
      addEventListener: (type, handler) => { handlers[type] = handler; },
      removeEventListener: (type, handler) => {
        if (handlers[type] === handler) delete handlers[type];
      },
    },
    crypto: webcrypto, URL, URLSearchParams, TextEncoder, Uint8Array, btoa, atob,
    setTimeout: () => 0, clearTimeout: () => {},
    location: { origin, href, reload: () => { reloads += 1; } },
    sessionStorage: {
      getItem: (key) => session.get(key) ?? null,
      setItem: (key, value) => session.set(key, value),
    },
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
  return { handlers, requests, messages, posted, stored, session, popup, origin, elements, token,
    parent, reloads: () => reloads };
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

const settle = async (p) => {
  for (let i = 0; i < 20 && p.posted.length === 0; i += 1) {
    await new Promise((resolve) => setImmediate(resolve));
  }
};

test('in the Fabric frame, sign-in asks the host for a handoff code and redeems it', async () => {
  const p = page({ embedded: true });
  await settle(p);
  assert.equal(p.posted.length, 1);
  const { data: request, target } = p.posted[0];
  assert.equal(target, '*');
  assert.equal(request.channel, 'fabric-auth');
  assert.equal(request.version, 1);
  assert.equal(request.kind, 'auth.requestHandoff');
  assert.equal(request.payload.callbackUrl, p.origin);
  assert.equal(request.payload.codeChallengeMethod, 'S256');
  assert.equal(p.session.get('fabricEmbedded'), 'true');
  const answer = (data) => ({ channel: 'fabric-auth', version: 1, kind: 'response',
    requestId: request.requestId, ...data });
  const result = { handoffCode: 'handoff', state: request.payload.state };

  // Only the parent's answer to this request, with this request's state, counts.
  await p.handlers.message({ source: {}, data: answer({ success: true, result }) });
  await p.handlers.message({ source: p.parent,
    data: { ...answer({ success: true, result }), requestId: 'other' } });
  assert.equal(p.requests.length, 0);
  await p.handlers.message({ source: p.parent, data: answer({ success: true, result }) });
  assert.equal(p.requests.length, 2);
  const redemption = JSON.parse(p.requests[0].options.body);
  assert.equal(redemption.verificationCode, 'handoff');
  assert.equal(redemption.codeType, 'fabric_handoff');
  assert.equal(redemption.redirectUri, p.origin);
  const digest = await webcrypto.subtle.digest('SHA-256', new TextEncoder().encode(redemption.codeVerifier));
  assert.equal(Buffer.from(digest).toString('base64url'), request.payload.codeChallenge);
  assert.equal(p.requests[1].url, '/.rayfin-local/static-session');
  assert.equal(JSON.parse(p.stored.get('authSession')).user.id, 'user-id');
  assert.equal(p.reloads(), 1);
});

test('in the Fabric frame, a state mismatch or a refusal installs nothing', async () => {
  for (const reply of [
    { success: true, result: { handoffCode: 'stolen', state: 'wrong' } },
    { success: false, error: { code: 'ACCESS_DENIED', message: 'Nobody is signed in.' } },
  ]) {
    const p = page({ embedded: true });
    await settle(p);
    const { requestId } = p.posted[0].data;
    await p.handlers.message({ source: p.parent,
      data: { channel: 'fabric-auth', version: 1, kind: 'response', requestId, ...reply } });
    assert.equal(p.requests.length, 0);
    assert.equal(p.reloads(), 0);
    assert.equal(p.elements.signin.disabled, false);
    if (!reply.success) assert.equal(p.elements.status.textContent, 'Nobody is signed in.');
  }
});

test('a frame without the Fabric mark signs in with the popup', async () => {
  const p = page({ embedded: true, href: 'http://localhost:7171/' });
  await settle(p);
  assert.equal(p.posted.length, 0);
  await p.handlers.click();
  assert.match(p.popup.location.href, /secureItemEmbed/);
});