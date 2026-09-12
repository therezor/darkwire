import { describe, expect, it } from 'vitest';

import { createWebI18n } from '@ghostwire/i18n/web';

/** English, resolved: the copy assertions below compare what a user reads. */
const t = createWebI18n('en').getFixedT(null, 'web');

import {
  SETUP_STEPS,
  initialStep,
  isSkippable,
  nextStep,
  previousStep,
  progressOf,
  titleOf,
  type SetupStep,
} from '@/setup/setup-steps.js';

describe('the order', () => {
  it('runs access first, then configuration', () => {
    expect(SETUP_STEPS).toEqual([
      'language',
      'code',
      'password',
      'provider',
      'model',
      'done',
    ]);
  });

  it('walks to done and stays there', () => {
    // A double-submit past the last step should be a no-op, not a crash in an
    // overlay the user cannot leave.
    let step: SetupStep = 'code';
    for (let n = 0; n < 10; n += 1) step = nextStep(step);
    expect(step).toBe('done');
  });
});

describe('isSkippable', () => {
  it('lets configuration be skipped and access not', () => {
    // An unclaimed server is a shell-capable agent with no password; an
    // unconfigured one merely cannot chat yet.
    expect(isSkippable('code')).toBe(false);
    expect(isSkippable('password')).toBe(false);
    expect(isSkippable('language')).toBe(true);
    expect(isSkippable('provider')).toBe(true);
    expect(isSkippable('model')).toBe(true);
  });
});

describe('previousStep', () => {
  it('offers no way back from the password step, and one from the code step', () => {
    // `password` cannot return to `code` because the code was single-use and has
    // already been spent, and a button that leads to a form nothing will accept
    // is worse than no button.
    expect(previousStep('password')).toBeNull();
    // `code` can, because the step before it is the language question and
    // nothing has been spent there — and picking the wrong language is the
    // mistake a user is most likely to want to undo the moment they see it.
    expect(previousStep('code')).toBe('language');
    // The first step has nowhere to go, which falls out of `step === from`
    // rather than being stated twice.
    expect(previousStep('language')).toBeNull();
  });

  it('goes back within configuration', () => {
    expect(previousStep('model')).toBe('provider');
    expect(previousStep('provider')).toBe('password');
  });

  it('will not lead behind the step the wizard opened on', () => {
    // A claimed install with no model starts at `provider`. A Back there would
    // offer to rotate a password nobody came here to change.
    expect(previousStep('provider', 'provider')).toBeNull();
    expect(previousStep('model', 'provider')).toBe('provider');
  });
});

describe('progressOf', () => {
  it('counts the five real steps and does not make done a sixth', () => {
    expect(progressOf('language')).toEqual({ current: 1, total: 5 });
    expect(progressOf('code')).toEqual({ current: 2, total: 5 });
    expect(progressOf('model')).toEqual({ current: 5, total: 5 });
    expect(progressOf('done')).toEqual({ current: 5, total: 5 });
  });
});

describe('initialStep', () => {
  it('starts at the language on an unclaimed install', () => {
    expect(
      initialStep({
        setupRequired: true,
        configured: false,
        hasProvider: false,
      }),
    ).toBe('language');
  });

  it('does not open at all on a claimed, configured install', () => {
    expect(
      initialStep({
        setupRequired: false,
        configured: true,
        hasProvider: true,
      }),
    ).toBeNull();
  });

  it('stays shut while the model question is unanswerable', () => {
    // `/api/status` needs a session, so a signed-out browser never learns this.
    // Treating unknown as "not configured" would pop the wizard open in front
    // of the login form for anyone whose session had merely expired.
    expect(
      initialStep({
        setupRequired: false,
        configured: undefined,
        hasProvider: false,
      }),
    ).toBeNull();
  });

  it('skips the credential steps for a claimed install with nothing configured', () => {
    // The tab was closed after the password step. Asking for a code that no
    // longer exists would be a dead end on an install that is already claimed.
    expect(
      initialStep({
        setupRequired: false,
        configured: false,
        hasProvider: false,
      }),
    ).toBe('provider');
  });

  it('opens on the model when a provider is already configured', () => {
    // Unconfigured is two situations. An install whose endpoint resolves needs
    // a model and nothing else, and opening on "add a provider" in front of
    // somebody who has one reads as the app having forgotten their settings.
    expect(
      initialStep({
        setupRequired: false,
        configured: false,
        hasProvider: true,
      }),
    ).toBe('model');
  });
});

describe('titleOf', () => {
  it('says something for every step', () => {
    for (const step of SETUP_STEPS) {
      const { title, note } = titleOf(step, t);
      expect(title).not.toBe('');
      expect(note).not.toBe('');
    }
  });
});
