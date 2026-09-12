/**
 * First-run setup, in a real browser against a real unclaimed server.
 *
 * What this asserts that no unit test can: that the wizard, the login overlay
 * and the app agree about which of them is showing. All three mount at once on
 * a browser with no session — `/api/auth/me` 401s for the wizard's situation
 * and for an expired session alike — and picking the wrong one leaves the
 * operator at a sign-in form whose password does not exist yet.
 */

import type { Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';

test.describe('an unclaimed install', () => {
  test.use({ harnessOptions: { password: null } });

  /**
   * The wizard opens on the language question, which every one of these walks
   * past. Skipping rather than choosing: the browser's language is already
   * applied — Playwright pins it to `en-US` — so `Skip` means "yes, that one",
   * and it is the click a first run actually makes.
   */
  const pastLanguage = async (page: Page): Promise<void> => {
    await expect(
      page.getByRole('heading', { name: 'Choose a language' }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Skip' }).click();
  };

  test('takes the one-time code and lands in the app', async ({
    page,
    harness,
  }) => {
    await page.goto(harness.url);
    await pastLanguage(page);

    await expect(
      page.getByRole('heading', { name: 'Enter the setup code' }),
    ).toBeVisible();
    // The login overlay must not also be up: its password does not exist yet.
    await expect(page.getByRole('heading', { name: 'Sign in' })).toBeHidden();

    // Lower case and without the grouping, which is how it arrives when someone
    // copies it out of a terminal. Both are presentation.
    const typed = (harness.setupCode ?? '').replaceAll('-', '').toLowerCase();
    // By role: the dialog is labelled by its own heading, so `getByLabel`
    // matches the overlay as well as the field inside it.
    await page.getByRole('textbox', { name: 'Setup code' }).fill(typed);
    await page.getByRole('button', { name: 'Continue' }).click();

    await expect(
      page.getByRole('heading', { name: 'Choose a password' }),
    ).toBeVisible();
    // Prefilled with the default, so a first run is a password and nothing else
    // unless the operator wants otherwise.
    await expect(page.getByLabel('Username')).toHaveValue('ghost');
    await page
      .getByLabel('Password', { exact: true })
      .fill('chosen-in-the-wizard');
    await page.getByLabel('Confirm password').fill('chosen-in-the-wizard');
    await page.getByRole('button', { name: 'Continue' }).click();

    // Past the password the wizard is optional: an install with no model still
    // serves everything but a turn.
    await expect(
      page.getByRole('heading', { name: 'Add a model provider' }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Skip' }).click();
    await page.getByRole('button', { name: 'Skip' }).click();

    await expect(
      page.getByRole('complementary', { name: 'Sidebar' }),
    ).toBeVisible();
    await expect(page.getByRole('dialog')).toBeHidden();
  });

  test('refuses a wrong code and stays on the step', async ({
    page,
    harness,
  }) => {
    await page.goto(harness.url);
    await pastLanguage(page);

    await page
      .getByRole('textbox', { name: 'Setup code' })
      .fill('ZZZZ-ZZZZ-ZZZZ');
    await page.getByRole('button', { name: 'Continue' }).click();

    await expect(page.getByRole('alert')).toContainText(
      'Incorrect or already-used',
    );
    await expect(
      page.getByRole('heading', { name: 'Enter the setup code' }),
    ).toBeVisible();
  });

  test('offers no way past the credential steps', async ({ page, harness }) => {
    await page.goto(harness.url);
    await pastLanguage(page);
    await expect(
      page.getByRole('heading', { name: 'Enter the setup code' }),
    ).toBeVisible();

    // Skipping here would leave a shell-capable agent with no password, which
    // is the state the whole flow exists to end.
    await expect(page.getByRole('button', { name: 'Skip' })).toBeHidden();
    // `Back` is offered, and leads to the language question rather than to
    // anything spent. The password step is the one with neither.
    await expect(page.getByRole('button', { name: 'Back' })).toBeVisible();
  });
});

test.describe('a claimed install with a provider but no model', () => {
  // The shape an upgrade lands in: the endpoint is configured and resolves, and
  // the only thing missing is the model each agent now has to state for itself.
  test.use({
    harnessOptions: {
      config: {
        providers: { ollama: { type: 'ollama', models: ['qwen3'] } },
        agents: { list: { default: { model: '' } } },
      },
    },
  });

  test('asks for the model, not for the provider it already has', async ({
    app,
  }) => {
    await expect(
      app.getByRole('heading', { name: 'Choose a model' }),
    ).toBeVisible();
    await expect(
      app.getByRole('heading', { name: 'Add a model provider' }),
    ).toBeHidden();
    // Nowhere behind this step was shown, so Back would lead somewhere the
    // operator was never sent.
    await expect(app.getByRole('button', { name: 'Back' })).toBeHidden();
  });
});

test.describe('a claimed install with nothing configured', () => {
  // `provider: 'auto'` with an empty `providers` map and no key in the
  // environment resolves no endpoint at all — which is what makes this the
  // install that genuinely needs the provider step, unlike the one above.
  //
  // The empty map has to be written out. Every harness now starts a real
  // server, and a real server needs an endpoint to answer with, so the default
  // config carries one; naming `providers` at all is what replaces it. The
  // in-process harness could leave the line out because its model was injected
  // rather than configured — which was the one thing about it a browser could
  // never have seen.
  test.use({
    harnessOptions: {
      config: {
        providers: {},
        agents: { list: { default: { provider: 'auto', model: '' } } },
      },
    },
  });

  test('opens at the provider step rather than asking for a spent code', async ({
    app,
  }) => {
    await expect(
      app.getByRole('heading', { name: 'Add a model provider' }),
    ).toBeVisible();
    await expect(
      app.getByRole('heading', { name: 'Enter the setup code' }),
    ).toBeHidden();
    // Nothing behind this step was ever shown, so there is nowhere to go back to.
    await expect(app.getByRole('button', { name: 'Back' })).toBeHidden();
  });

  test('leaves every other screen working, and says why chat is not', async ({
    app,
  }) => {
    await app.getByRole('button', { name: 'Skip' }).click();
    await app.getByRole('button', { name: 'Skip' }).click();

    const composer = app.getByRole('textbox', { name: 'Message' });
    await expect(composer).toBeDisabled();
    await expect(composer).toHaveAttribute(
      'placeholder',
      'No model configured yet',
    );
    await expect(
      app.getByRole('link', { name: 'Add a provider' }),
    ).toBeVisible();

    // The files screen is the proof that "everything but a turn" is not a
    // slogan: it lists the workspace on an install that cannot chat.
    await app.getByRole('link', { name: 'Files' }).click();
    await expect(app.getByText('notes.md')).toBeVisible();
  });
});
