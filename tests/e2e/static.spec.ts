import { BrowserLog, describe } from './support/browser-log.ts';
import { expectOk, scenarioOutcome, expectOutcomeOk, type AppScope, type SessionInfo } from './support/fixture-app.ts';
import { expect, test } from './support/fixtures.ts';
import { signInOnHostPage, signInThroughPopup, type Credentials } from './support/portal.ts';
import { newUser } from './support/project.ts';
import type { Page } from '@playwright/test';

/*
 * A protected static site (support/static-suite.ts): the fixture app built with
 * `rayfin-local serve --static` and `assetAccess: protected`, on the backend's origin. Checks the
 * hosting gate (docs/hosting.md#asset-access) standalone and in the local Fabric host
 * (`/.rayfin-local/portal/embed`), as Fabric's front door and portal do: the interstitial,
 * sign-in through the popup or the host's handoff, the SPA fallback, and the `_fu` user hint.
 * In this order: anonymous, standalone, embedded as the same user, as another user, then a
 * fresh browser.
 */

test.use({ suiteName: 'static' });

const UNAUTHENTICATED = { error: 'Authentication required.' };
const signedOut = { isAuthenticated: false, user: null };
/** Signed in on the host page after the standalone user, so the frame's `_fu` hint is not theirs. */
const otherUser: Credentials = newUser();

const appUrl = (api: string) => `${api}/`;
const hostUrl = (api: string) => `${api}/.rayfin-local/portal/embed?app=${encodeURIComponent(appUrl(api))}`;

/** The app's frame on the local Fabric host, once the app is ready. */
async function framedApp(page: Page): Promise<AppScope> {
  const iframe = page.locator('#frame iframe');
  await expect(iframe).toBeAttached();
  const src = new URL((await iframe.getAttribute('src')) ?? '');
  expect(src.searchParams.get('fabricEmbedded')).toBe('true');
  expect(src.searchParams.get('_fu'), "the host frames the app with the user's _fu hint").toBeTruthy();
  const frame = page.frameLocator('#frame iframe');
  await expect(frame.getByTestId('app-status')).toHaveText('ready', { timeout: 60_000 });
  return frame;
}

/** The session `initEmbeddedAuth` got when the framed app loaded. */
async function embeddedSession(frame: AppScope): Promise<SessionInfo> {
  const outcome = await scenarioOutcome(frame, 'embedded-auth');
  expectOutcomeOk('embedded-auth', outcome);
  return outcome.value as SessionInfo;
}

test('anonymous: documents get the sign-in page, assets and runtime config a 401', async ({ app }) => {
  const api = app.info.serve.apiUrl;
  const document = await fetch(appUrl(api), {
    headers: { accept: 'text/html', 'sec-fetch-dest': 'document', 'sec-fetch-mode': 'navigate' },
  });
  expect(document.status).toBe(200);
  const html = await document.text();
  expect(html, 'the interstitial').toContain('id="signin"');
  expect(html, 'not the app').not.toContain('data-testid="app-status"');
  expect(document.headers.get('x-frame-options')).toBe('DENY');

  for (const path of ['/rayfin.config.json', '/index.html', '/assets/missing-e2e.js', '/favicon.ico']) {
    const response = await fetch(`${api}${path}`);
    expect(response.status, path).toBe(401);
    expect(await response.json(), path).toEqual(UNAUTHENTICATED);
  }

  await app.page.goto(appUrl(api));
  await expect(app.page.locator('#signin')).toBeVisible();
  await expect(app.page.getByTestId('app-status')).toHaveCount(0);
});

test('standalone: the sign-in page signs in through the popup, then the app works', async ({ app }) => {
  const { page, info } = app;
  await page.goto(appUrl(info.serve.apiUrl));
  const { popupUrl, outcome } = await signInThroughPopup(page, () => page.locator('#signin').click(), info.user);
  expect(popupUrl.pathname).toBe('/.rayfin-local/portal/secureItemEmbed');
  expect(outcome).toBe('signed-up');
  // The sign-in page reloads into the app, its session primed.
  await expect(page.getByTestId('app-status')).toHaveText('ready', { timeout: 60_000 });
  expect(await scenarioOutcome(page, 'embedded-auth')).toMatchObject({ ok: true, value: null });
  expect(await expectOk<SessionInfo>(page, 'session')).toMatchObject({
    isAuthenticated: true,
    user: { email: info.user.email },
  });
  expect(await expectOk(page, 'todo-crud')).toMatchObject({ deleted: true, foundAfterDelete: false });
});

test('standalone: paths with no file get the index document', async ({ app }) => {
  const { page, info } = app;
  const api = info.serve.apiUrl;
  // The context's cookie: signed in by the previous test.
  for (const path of ['/favicon.ico', '/assets/missing-e2e.js', '/some/route']) {
    const response = await page.request.get(`${api}${path}`);
    expect(response.status(), path).toBe(200);
    expect(response.headers()['content-type'], path).toMatch(/^text\/html/);
    expect(await response.text(), path).toContain('data-testid="app-status"');
  }
  const config = await page.request.get(`${api}/rayfin.config.json`);
  expect(config.status()).toBe(200);
  // The backend's origin, under the name serve prints (localhost), which may not be the page's.
  expect(new URL((await config.json()).apiUrl).port).toBe(String(info.serve.apiPort));

  await page.goto(`${api}/some/route`);
  await expect(page.getByTestId('app-status')).toHaveText('ready');
  expect(await expectOk<SessionInfo>(page, 'session')).toMatchObject({ isAuthenticated: true });
});

test('embedded, the same user: the frame keeps the hosting cookie and the session', async ({ app }) => {
  const { page, info } = app;
  await page.goto(hostUrl(info.serve.apiUrl));
  await signInOnHostPage(page, info.user);
  const frame = await framedApp(page);
  expect(await embeddedSession(frame)).toMatchObject({ isAuthenticated: true, user: { email: info.user.email } });
  expect(await expectOk(frame, 'todo-crud')).toMatchObject({ deleted: true, foundAfterDelete: false });
});

test("embedded, another user: the _fu hint rejects the first user's cookie", async ({ app }) => {
  const { page, info } = app;
  if (!page.url().includes('/.rayfin-local/portal/embed')) {
    await page.goto(hostUrl(info.serve.apiUrl));
    await signInOnHostPage(page, info.user);
    await framedApp(page);
  }
  await page.locator('#switch-user').click();
  await signInOnHostPage(page, otherUser);
  await expect(page.locator('#bar-user')).toContainText(otherUser.email);
  const frame = await framedApp(page);
  expect(await embeddedSession(frame)).toMatchObject({ isAuthenticated: true, user: { email: otherUser.email } });
  expect(await expectOk<SessionInfo>(frame, 'session')).toMatchObject({ user: { email: otherUser.email } });
  expect(await expectOk(frame, 'todo-crud')).toMatchObject({ deleted: true, foundAfterDelete: false });
});

test('embedded, a fresh browser: the host signs the framed sign-in page in, with no popup', async ({
  app,
  browser,
}, testInfo) => {
  const context = await browser.newContext();
  const log = new BrowserLog(context);
  try {
    const page = await context.newPage();
    let popups = 0;
    page.on('popup', () => popups++);
    await page.goto(hostUrl(app.info.serve.apiUrl));
    await signInOnHostPage(page, app.info.user);
    const frame = await framedApp(page);
    expect(await embeddedSession(frame)).toMatchObject({
      isAuthenticated: true,
      user: { email: app.info.user.email },
    });
    expect(await expectOk(frame, 'todo-crud')).toMatchObject({ deleted: true, foundAfterDelete: false });
    expect(popups, 'sign-in windows opened').toBe(0);
    const unexpected = log.unexpected(0, [/\/favicon\.ico(\s|$)/]).map(describe);
    if (unexpected.length) {
      await testInfo.attach('browser-log', { body: JSON.stringify(log.since(0), null, 2), contentType: 'application/json' });
    }
    expect(unexpected, 'console errors (such as CSP violations) or failed requests').toEqual([]);
  } finally {
    await context.close();
  }
});

test('sign-out: the hosting cookie ends with the session it carries', async ({ app, browser }) => {
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    await page.goto(appUrl(app.info.serve.apiUrl));
    await signInThroughPopup(page, () => page.locator('#signin').click(), app.info.user);
    await expect(page.getByTestId('app-status')).toHaveText('ready', { timeout: 60_000 });
    expect(await expectOk<SessionInfo>(page, 'session')).toMatchObject({ isAuthenticated: true });
    expect(await expectOk<SessionInfo>(page, 'sign-out')).toEqual(signedOut);
    await page.reload();
    await expect(page.locator('#signin')).toBeVisible();
  } finally {
    await context.close();
  }
});