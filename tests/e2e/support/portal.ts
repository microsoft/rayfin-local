import { expect, type Page } from '@playwright/test';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { RESULTS_DIR } from './paths.ts';

/*
 * The emulated "Sign in with Fabric" popup (`<api>/.rayfin-local/portal/secureItemEmbed`, from
 * crates/rl-auth/src/portal): sign-in and sign-up tabs over one form; it closes itself once the app
 * has its code. Switching tabs and submitting clear the form's error. The local Fabric host
 * (`<api>/.rayfin-local/portal/embed`) shows the same form on the page itself.
 */

export interface Credentials {
  email: string;
  password: string;
}

export type PortalOutcome = 'signed-up' | 'signed-in';

/** Accounts signed up in this run, across worker processes (test-results is new every run). */
const ACCOUNTS_DIR = join(RESULTS_DIR, '.accounts');
const marker = (email: string) => join(ACCOUNTS_DIR, email.replace(/[^\w.@-]/g, '_'));

/**
 * Clicks `trigger` on `page`, which must open the popup, and signs `user` up there on the run's
 * first sign-in, else signs in. Resolves once the popup closed.
 */
export async function signInThroughPopup(
  page: Page,
  trigger: () => Promise<void>,
  user: Credentials,
): Promise<{ popupUrl: URL; outcome: PortalOutcome }> {
  const [popup] = await Promise.all([page.waitForEvent('popup', { timeout: 30_000 }), trigger()]);
  // Settles when the popup or its whole context closes.
  const closed = popup.waitForEvent('close', { timeout: 0 }).then(
    () => true,
    () => true,
  );
  // The interstitial of a protected static site opens the popup on about:blank first.
  await popup.waitForURL(/^http/, { timeout: 30_000 }).catch(() => {});
  await popup.waitForLoadState('domcontentloaded').catch(() => {});
  const popupUrl = new URL(popup.url());
  const outcome = await submitForm(popup, user, closed, () => popup.isClosed(), 'The sign-in popup');
  return { popupUrl, outcome };
}

/**
 * Signs `user` in on the local Fabric host (`<api>/.rayfin-local/portal/embed`, the sign-in
 * form on the page itself), signing them up on the run's first sign-in. Resolves once the page
 * frames the app.
 */
export async function signInOnHostPage(page: Page, user: Credentials): Promise<PortalOutcome> {
  const framed = page.locator('#frame iframe');
  const shown = framed.waitFor({ state: 'attached', timeout: 0 }).then(
    () => true,
    () => true,
  );
  return submitForm(page, user, shown, () => false, 'The local Fabric host');
}

/** Fills and submits the sign-in form until `done` settles; signs up or in, as the run needs. */
async function submitForm(
  form: Page,
  user: Credentials,
  done: Promise<boolean>,
  gone: () => boolean,
  what: string,
): Promise<PortalOutcome> {
  await expect(form.locator('#username'), 'the sign-in form').toBeEnabled({ timeout: 30_000 });
  const canSignUp = await form.locator('#tab-signup').isVisible();
  const signIn = { tab: 'signin', outcome: 'signed-in' } as const;
  const signUp = { tab: 'signup', outcome: 'signed-up' } as const;
  // The other attempt is a fallback; the 4xx of the first then fails the test's log check.
  const attempts = !canSignUp ? [signIn] : existsSync(marker(user.email)) ? [signIn, signUp] : [signUp, signIn];
  const errors: string[] = [];
  for (const attempt of attempts) {
    if (canSignUp) await form.locator(`#tab-${attempt.tab}`).click();
    await form.locator('#username').fill(user.email);
    await form.locator('#password').fill(user.password);
    if (attempt.tab === 'signup') await form.locator('#confirm').fill(user.password);
    await form.locator('#submit').click();
    const error = await doneOrError(form, done, gone, what);
    if (error === null) {
      mkdirSync(ACCOUNTS_DIR, { recursive: true });
      writeFileSync(marker(user.email), attempt.outcome);
      return attempt.outcome;
    }
    errors.push(`${attempt.tab}: ${error}`);
  }
  throw new Error(`${what} refused ${user.email}: ${errors.join('; ')}`);
}

/** Null once `done` settled (the popup closed, the app is framed), else the error the form shows. */
async function doneOrError(
  form: Page,
  done: Promise<boolean>,
  gone: () => boolean,
  what: string,
  timeoutMs = 30_000,
): Promise<string | null> {
  const formError = form.locator('#form-error');
  const first = await Promise.race([
    done.then(() => 'done' as const),
    formError.waitFor({ state: 'visible', timeout: timeoutMs }).then(
      () => 'error' as const,
      () => 'neither' as const,
    ),
  ]);
  if (first === 'done' || gone()) return null;
  if (first === 'error') return (await formError.textContent())?.trim() || 'an empty error';
  // The wait may have ended because the popup is closing.
  const late = await Promise.race([done.then(() => true), sleep(2_000).then(() => false)]);
  if (late) return null;
  const shown = await form.locator('body').innerText().catch(() => '');
  throw new Error(`${what} neither finished nor showed an error within ${timeoutMs / 1000} s. It shows:\n${shown}`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
