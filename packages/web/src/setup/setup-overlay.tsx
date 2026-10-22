/**
 * The first-run wizard.
 *
 * An overlay rather than a route, for the reason the login overlay is one:
 * there is nothing behind it to navigate back from, and its "closed" state is a
 * finished setup rather than an Escape key. It sits *above* the login overlay,
 * because on an unclaimed install `/api/auth/me` also 401s and both would
 * otherwise render at once — and the one that can actually get the user in is
 * this one.
 *
 * Four steps, split by whether they may be skipped. Access — the code, then the
 * password — is mandatory, because an unclaimed server is a shell-capable agent
 * with no password. Configuration — a provider, then a model — is not, because
 * an install with no model still serves files, settings, workspaces and
 * notifications; only chat is unavailable. Skipping lands in a working app with
 * a pointer in the composer rather than in a dead end.
 *
 * The step machine lives in `setup-steps.ts` with no DOM in it, and the steps
 * in `setup-wizard.tsx`, which loads only when this decides to open.
 */

import { useQuery } from '@tanstack/react-query';
import { Suspense, lazy, useEffect, useState, type JSX } from 'react';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { initialStep, type SetupStep } from './setup-steps.js';

const SetupWizard = lazy(async () => ({
  default: (await import('./setup-wizard.js')).SetupWizard,
}));

/**
 * Whether the wizard opens, and where. Kept small and eager: every load asks
 * the question, and almost every answer is no.
 */
export function SetupOverlay(): JSX.Element | null {
  const [first, setFirst] = useState<SetupStep | null>(null);

  // Public, so it answers on a browser with no session, which is the only
  // state an unclaimed install has.
  const setup = useQuery({
    queryKey: queryKeys.setup,
    queryFn: ({ signal }) => api.setupStatus(signal),
  });

  // 401 while unclaimed, which is why the wizard cannot depend on it to decide
  // whether to open. It is read only to answer "is there a model yet".
  const status = useQuery({
    queryKey: queryKeys.status,
    queryFn: ({ signal }) => api.status(signal),
    retry: false,
  });

  const required = setup.data?.required;
  const configured = status.data?.configured;
  // Non-empty as soon as an endpoint resolves, which it does before a model is
  // chosen: `resolveProvider` picks the instance and only then refuses on the
  // missing model. So this is exactly "is there a provider already".
  const hasProvider = (status.data?.provider ?? '') !== '';

  useEffect(() => {
    // Only ever *opens* the wizard. Once it is showing, this must not move it:
    // every step changes one of these flags, and re-deriving would walk the
    // user backwards the moment they finished a step.
    if (first !== null || required === undefined) return;
    setFirst(initialStep({ setupRequired: required, configured, hasProvider }));
  }, [first, required, configured, hasProvider]);

  if (first === null) return null;

  return (
    // The backdrop while the steps load, so the app behind never shows.
    <Suspense fallback={<div className="setup-overlay" aria-busy="true" />}>
      <SetupWizard first={first} />
    </Suspense>
  );
}
