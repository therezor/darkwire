/**
 * The first-run wizard, as a state machine with no DOM in it.
 *
 * Five steps, and the split between the credential pair and everything else is
 * the only structural idea here. **Access is mandatory and configuration is
 * not.** A server that nobody has claimed is a shell-capable agent with no
 * password, so `code` and `password` have to complete. A server with no model is
 * merely an install that cannot chat yet — files, settings, workspaces and
 * notifications all work — so `provider` and `model` are skippable, and skipping
 * them lands the operator in a working app rather than in a dead end.
 *
 * `language` sits ahead of all of it because every screen after it is prose: a
 * wizard that asked for a password first would have asked in a language the
 * operator may not read.
 *
 * Kept as a module of pure functions rather than as state inside the overlay so
 * that the ordering, the skip rules and the "what does Back mean here" question
 * can be tested without rendering anything. The overlay owns the answers; this
 * owns which question comes next.
 */

import type { TFunction } from 'i18next';

export const SETUP_STEPS = [
  'language',
  'code',
  'password',
  'provider',
  'model',
  'done',
] as const;
export type SetupStep = (typeof SETUP_STEPS)[number];

/**
 * Steps that may be skipped, and the reason each one may be.
 *
 * `code` and `password` are absent deliberately: skipping them would leave the
 * agent unclaimed, which is the state the wizard exists to end.
 *
 * `language` is skippable because it is the one step that has already answered
 * itself: the browser asked for a language, the wizard is rendering in it, and
 * skipping means "yes, that one". A first step that *had* to be answered would
 * be a toll gate in front of the install.
 */
const SKIPPABLE: ReadonlySet<SetupStep> = new Set<SetupStep>([
  'language',
  'provider',
  'model',
]);

export function isSkippable(step: SetupStep): boolean {
  return SKIPPABLE.has(step);
}

/**
 * The step after this one.
 *
 * `done` is its own successor rather than an error: a double-submit past the
 * last step should be a no-op, not a crash in an overlay the user cannot leave.
 */
export function nextStep(step: SetupStep): SetupStep {
  const index = SETUP_STEPS.indexOf(step);
  return SETUP_STEPS[Math.min(index + 1, SETUP_STEPS.length - 1)] ?? 'done';
}

/**
 * The step before this one, or `null` where going back is meaningless.
 *
 * Neither credential step has a Back, and for different reasons. `code` is the
 * first step, so there is nowhere to go; `password` cannot go back to `code`
 * because the code was single-use and has already been spent — offering a
 * button that leads to a form nothing will accept is worse than offering none.
 *
 * `from` is where this run of the wizard *opened*, and it stops Back from
 * leading somewhere the user was never sent: a claimed install with no model
 * starts at `provider`, and a Back to `password` there would be offering to
 * rotate a password nobody came here to change.
 */
export function previousStep(
  step: SetupStep,
  from: SetupStep = 'language',
): SetupStep | null {
  // `password` still cannot go back to `code` — the code was single-use and is
  // already spent. `code` *can* go back to `language`, because nothing has been
  // spent at that point and the language is the one answer a user is most
  // likely to want to correct on sight.
  if (step === 'password' || step === 'done') return null;
  if (step === from) return null;
  const index = SETUP_STEPS.indexOf(step);
  return SETUP_STEPS[index - 1] ?? null;
}

interface SetupProgress {
  /** 1-based, for "Step 2 of 4". `done` is not a step anyone is on. */
  readonly current: number;
  readonly total: number;
}

/** `done` reports as complete rather than as a fifth step. */
export function progressOf(step: SetupStep): SetupProgress {
  const total = SETUP_STEPS.length - 1;
  const index = SETUP_STEPS.indexOf(step);
  return { current: Math.min(index + 1, total), total };
}

/**
 * Where a browser opening the app should start.
 *
 * Three states, and the last two are the ones worth naming.
 *
 * An install whose password is already set but whose wizard was never finished
 * — the tab was closed after the password step — is *claimed*, so it must not
 * ask for a code that no longer exists; it goes straight to the provider step.
 *
 * `configured: undefined` means the question has not been answered yet, which
 * on a signed-out browser it never will be: `/api/status` needs a session.
 * Treating unknown as "not configured" would pop the wizard open in front of
 * the login form for anyone whose session had merely expired.
 */
export function initialStep(input: {
  readonly setupRequired: boolean;
  readonly configured: boolean | undefined;
  /** Whether an endpoint already resolves — `status.provider` is not empty. */
  readonly hasProvider: boolean;
}): SetupStep | null {
  if (input.setupRequired) return 'language';
  // Not "assume the worst": an unanswerable question is not a fresh install.
  if (input.configured === undefined) return null;
  // Nothing to do: a claimed, configured install is just the app.
  if (input.configured) return null;
  // Unconfigured is two different situations, and asking the wrong question is
  // what makes the wizard feel like it is not listening. An install with an
  // endpoint already resolving needs a *model*, and opening on "add a provider"
  // in front of somebody who has one reads as the app having forgotten.
  return input.hasProvider ? 'model' : 'provider';
}

interface SetupTitle {
  readonly title: string;
  readonly note: string;
}

/**
 * What each step says about itself. Here so the copy is testable and in one place.
 *
 * `t` is a parameter rather than a hook because this is a pure function the
 * state-machine tests drive without rendering anything — which is the property
 * the whole module exists to have.
 */
export function titleOf(step: SetupStep, t: TFunction): SetupTitle {
  switch (step) {
    case 'language':
      return { title: t('setup.languageTitle'), note: t('setup.languageNote') };
    case 'code':
      return { title: t('setup.codeTitle'), note: t('setup.codeNote') };
    case 'password':
      return { title: t('setup.passwordTitle'), note: t('setup.passwordNote') };
    case 'provider':
      return { title: t('setup.providerTitle'), note: t('setup.providerNote') };
    case 'model':
      return { title: t('setup.modelTitle'), note: t('setup.modelNote') };
    case 'done':
      return { title: t('setup.doneTitle'), note: t('setup.doneNote') };
  }
}
