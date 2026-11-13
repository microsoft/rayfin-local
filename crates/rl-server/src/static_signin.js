// The protected-site bootstrap, before app assets can load. Standalone, it uses the same PKCE popup
// as the SDK; in the Fabric frame, it asks the host for a handoff code over the `fabric-auth` bridge.
(() => {
  'use strict';
  const config = JSON.parse(document.getElementById('rl-static-config').textContent);
  const button = document.getElementById('signin');
  const status = document.getElementById('status');
  const base64url = (bytes) => btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  const random = () => base64url(crypto.getRandomValues(new Uint8Array(32)));
  const challengeFor = async (verifier) => base64url(new Uint8Array(
    await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier))));
  const EMBEDDED_PARAM = 'fabricEmbedded';
  const HANDOFF_TIMEOUT_MS = 30000;
  let popup = null;
  let challenge = null;
  let verifier = '';
  let redeeming = false;

  function fail(message) {
    status.textContent = message;
    button.disabled = false;
  }

  // The SDK's embedded-mode test: the host marks only the first frame URL, so the mark is kept for
  // the tab. It picks the sign-in UX; it never grants access.
  function fabricEmbedded() {
    if (window.self === window.top) return false;
    try {
      if (new URL(location.href).searchParams.get(EMBEDDED_PARAM) === 'true') {
        try {
          sessionStorage.setItem(EMBEDDED_PARAM, 'true');
        } catch {
          // Not kept: in-frame navigations that drop the query sign in with the popup.
        }
        return true;
      }
      return sessionStorage.getItem(EMBEDDED_PARAM) === 'true';
    } catch {
      return false;
    }
  }

  // Redeems a handoff code, then sets the hosting cookie and primes the SDK's session.
  async function redeem(handoffCode, codeVerifier) {
    const response = await fetch('/api/auth/v1/token', {
      method: 'POST', cache: 'no-store', credentials: 'same-origin',
      headers: { 'content-type': 'application/json', 'x-publishable-key': config.publishableKey },
      body: JSON.stringify({
        grantType: 'authorization_code', codeType: 'fabric_handoff',
        verificationCode: handoffCode, codeVerifier, redirectUri: location.origin,
      }),
    });
    if (!response.ok) throw new Error('redemption');
    const session = await response.json();
    const cookie = await fetch('/.rayfin-local/static-session', {
      method: 'POST', cache: 'no-store', credentials: 'same-origin',
      headers: { authorization: `Bearer ${session.accessToken}` },
    });
    if (!cookie.ok) throw new Error('cookie');
    // Fabric primes the SDK's default session store (localStorage, `authSession`) before reloading.
    try {
      const payload = JSON.parse(atob(session.accessToken.split('.')[1]
        .replace(/-/g, '+').replace(/_/g, '/')));
      localStorage.setItem('authSession', JSON.stringify({
        user: { id: payload.sub, email: payload.email },
        accessToken: session.accessToken, refreshToken: session.refreshToken ?? null,
        expiresAt: new Date(Date.now() + session.expiresIn * 1000).toISOString(),
      }));
    } catch {
      // Apps using a custom storage prefix or blocked storage sign in themselves.
    }
  }

  // In the Fabric frame: `auth.requestHandoff` to the parent, as the SDK's embedded login does.
  async function signInEmbedded() {
    if (!window.crypto?.subtle) {
      fail('Sign-in needs a secure browser context. Open this site on localhost or use HTTPS.');
      return;
    }
    button.disabled = true;
    status.textContent = 'Signing in…';
    const codeVerifier = random();
    const state = random();
    const requestId = random();
    let settled = false;
    let timer = null;
    const finish = (message) => {
      settled = true;
      clearTimeout(timer);
      window.removeEventListener('message', onMessage);
      if (message) fail(message);
    };
    async function onMessage(event) {
      const data = event.data;
      if (settled || event.source !== window.parent || !data || typeof data !== 'object'
          || data.channel !== 'fabric-auth' || data.kind !== 'response'
          || data.requestId !== requestId) return;
      if (data.success !== true) {
        finish(data.error?.message || 'The Fabric host refused sign-in. Try again.');
        return;
      }
      if (typeof data.result?.handoffCode !== 'string' || data.result.state !== state) {
        finish('Could not finish sign-in. Try again.');
        return;
      }
      finish(null);
      try {
        await redeem(data.result.handoffCode, codeVerifier);
        location.reload();
      } catch {
        fail('Could not finish sign-in. Check rayfin-local serve output and try again.');
      }
    }
    try {
      const codeChallenge = await challengeFor(codeVerifier);
      window.addEventListener('message', onMessage);
      timer = setTimeout(() => finish('The Fabric host did not answer. Try again.'),
        HANDOFF_TIMEOUT_MS);
      window.parent.postMessage({
        channel: 'fabric-auth', version: 1, kind: 'auth.requestHandoff', requestId,
        payload: { callbackUrl: location.origin, codeChallenge, codeChallengeMethod: 'S256', state },
      }, '*');
    } catch {
      finish('Could not start sign-in. Reload this page and try again.');
    }
  }

  async function signInWithPopup() {
    if (!window.crypto?.subtle) {
      fail('Sign-in needs a secure browser context. Open this site on localhost or use HTTPS.');
      return;
    }
    button.disabled = true;
    redeeming = false;
    verifier = random();
    const state = random();
    // Open synchronously with the click, before awaiting PKCE, to avoid popup blocking.
    popup = window.open('about:blank', 'rayfin-static-signin', 'width=520,height=720');
    if (!popup) {
      fail('Allow popups for this site and try again.');
      return;
    }
    try {
      challenge = {
        type: 'brokeredAuth.challenge', returnOrigin: location.origin,
        codeChallenge: await challengeFor(verifier), codeChallengeMethod: 'S256', state,
      };
      const url = new URL(`${config.portalUrl.replace(/\/$/, '')}/secureItemEmbed`);
      url.search = new URLSearchParams({
        workspaceId: config.workspaceId, itemId: config.itemId,
        itemType: 'AppBackend', extensionPath: '/brokeredauth',
      });
      popup.location.href = url.href;
      status.textContent = 'Finish signing in in the popup.';
    } catch {
      popup.close();
      fail('Could not start sign-in. Reload this page and try again.');
    }
  }

  window.addEventListener('message', async (event) => {
    if (!popup || event.source !== popup
        || event.origin !== new URL(config.portalUrl).origin || !challenge) return;
    const data = event.data;
    if (!data || typeof data !== 'object') return;
    if (data.type === 'brokeredAuth.ready') {
      popup.postMessage(challenge, event.origin);
      return;
    }
    if (data.state !== challenge.state) return;
    if (data.type === 'brokeredAuth.error') {
      fail('Sign-in was cancelled or refused. Try again.');
      return;
    }
    if (data.type !== 'brokeredAuth.handoff' || typeof data.handoffCode !== 'string'
        || redeeming) return;
    redeeming = true;
    try {
      await redeem(data.handoffCode, verifier);
      popup.close();
      location.reload();
    } catch {
      redeeming = false;
      fail('Could not finish sign-in. Check rayfin-local serve output and try again.');
    }
  });

  const embedded = fabricEmbedded();
  button.addEventListener('click', () => (embedded ? signInEmbedded() : signInWithPopup()));
  if (embedded) signInEmbedded();
})();
