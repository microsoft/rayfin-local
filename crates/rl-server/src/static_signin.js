// The protected-site bootstrap uses the same PKCE popup as the SDK, before app assets can load.
(() => {
  'use strict';
  const config = JSON.parse(document.getElementById('rl-static-config').textContent);
  const button = document.getElementById('signin');
  const status = document.getElementById('status');
  const base64url = (bytes) => btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  let popup = null;
  let challenge = null;
  let verifier = '';
  let redeeming = false;

  function fail(message) {
    status.textContent = message;
    button.disabled = false;
  }

  button.addEventListener('click', async () => {
    if (!window.crypto?.subtle) {
      fail('Sign-in needs a secure browser context. Open this site on localhost or use HTTPS.');
      return;
    }
    button.disabled = true;
    redeeming = false;
    verifier = base64url(crypto.getRandomValues(new Uint8Array(32)));
    const state = base64url(crypto.getRandomValues(new Uint8Array(32)));
    // Open synchronously with the click, before awaiting PKCE, to avoid popup blocking.
    popup = window.open('about:blank', 'rayfin-static-signin', 'width=520,height=720');
    if (!popup) {
      fail('Allow popups for this site and try again.');
      return;
    }
    try {
      const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier));
      challenge = {
        type: 'brokeredAuth.challenge', returnOrigin: location.origin,
        codeChallenge: base64url(new Uint8Array(digest)), codeChallengeMethod: 'S256', state,
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
  });

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
      const response = await fetch('/api/auth/v1/token', {
        method: 'POST', cache: 'no-store', credentials: 'same-origin',
        headers: { 'content-type': 'application/json', 'x-publishable-key': config.publishableKey },
        body: JSON.stringify({
          grantType: 'authorization_code', codeType: 'fabric_handoff',
          verificationCode: data.handoffCode, codeVerifier: verifier, redirectUri: location.origin,
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
      popup.close();
      location.reload();
    } catch {
      redeeming = false;
      fail('Could not finish sign-in. Check rayfin-local serve output and try again.');
    }
  });
})();
