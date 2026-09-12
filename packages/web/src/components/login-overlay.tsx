/**
 * The login overlay.
 *
 * An overlay rather than a route, for one reason: a session can expire at any
 * moment, including mid-turn. A `/login` route would mean navigating away from
 * a conversation, losing the composer's contents and the socket, and then
 * trying to navigate back to where the user was. An overlay covers the app,
 * takes the password, and leaves everything behind it exactly as it was.
 *
 * It renders only when the server says authentication is on *and* the caller is
 * not authenticated — `/api/auth/me` answers both questions in one request, and
 * a 401 from it is the normal state of a fresh browser rather than an error.
 *
 * One more condition, and it is the one that is easy to miss: an install with
 * no password set also 401s, and a sign-in form there is a form that can never
 * succeed. `/api/setup` is the question that distinguishes "you are signed out"
 * from "nobody has claimed this yet", and the setup overlay handles the second.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState, type JSX, type SyntheticEvent } from 'react';
import { useTranslation } from 'react-i18next';

import { DEFAULT_USERNAME } from '@ghostwire/protocol';

import { ApiError, api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { Button } from './ui/button.js';
import { Field } from './ui/field.js';
import { Wordmark } from './wordmark.js';

export function LoginOverlay(): JSX.Element | null {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // Prefilled with the default, which is right on every install that never
  // changed it and one keystroke away from right on the rest. The server will
  // not say what the name actually is before a session exists — that would be
  // handing out half the credential — so a good guess is the best available.
  const [username, setUsername] = useState(DEFAULT_USERNAME);
  const [password, setPassword] = useState('');

  const me = useQuery({
    queryKey: queryKeys.me,
    queryFn: ({ signal }) => api.me(signal),
  });

  const setup = useQuery({
    queryKey: queryKeys.setup,
    queryFn: ({ signal }) => api.setupStatus(signal),
  });

  const login = useMutation({
    mutationFn: (credentials: { name: string; secret: string }) =>
      api.login(credentials.name, credentials.secret),
    onSuccess: async () => {
      setPassword('');
      // Everything fetched while unauthenticated is a 401 in the cache.
      await queryClient.invalidateQueries();
    },
  });

  const unauthenticated =
    me.error instanceof ApiError && me.error.isUnauthenticated;
  if (!unauthenticated) return null;
  // Undefined while the question is still in flight: showing a login for a
  // second and then replacing it with a wizard is a flash of the wrong screen
  // on the one page load where the user has least idea what to expect.
  if (setup.data?.required !== false) return null;

  const submit = (event: SyntheticEvent): void => {
    event.preventDefault();
    login.mutate({ name: username, secret: password });
  };

  return (
    // Not a Dialog: there is nothing behind it to return focus to, and its
    // "closed" state is a successful login rather than an Escape key.
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="login-title"
      className="login-overlay"
    >
      <form onSubmit={submit} className="stack login-card">
        <div className="stack login-card__header">
          <Wordmark className="eyebrow" />
          <h1 id="login-title" className="login-card__title">
            {t('account.signInTitle')}
          </h1>
        </div>

        <Field
          label={t('account.usernameLabel')}
          name="username"
          // `username` rather than nothing, so a password manager files the two
          // fields as one credential and offers to fill both.
          autoComplete="username"
          spellCheck={false}
          value={username}
          onChange={(event) => {
            setUsername(event.target.value);
          }}
        />

        <Field
          label={t('account.passwordLabel')}
          type="password"
          name="password"
          autoComplete="current-password"
          // The autofocus is on the password rather than the username, because
          // the username is already filled with the answer most installs want.
          autoFocus
          value={password}
          onChange={(event) => {
            setPassword(event.target.value);
          }}
          error={errorMessageOf(login.error)}
        />

        <Button
          type="submit"
          variant="primary"
          disabled={
            login.isPending || password === '' || username.trim() === ''
          }
        >
          {login.isPending ? 'Signing in…' : 'Sign in'}
        </Button>
      </form>
    </div>
  );
}

/**
 * A wrong credential and a rate limit are different messages. Anything else is
 * reported verbatim rather than flattened to "login failed" — an operator
 * debugging a reverse proxy needs the actual status.
 *
 * The 401 says "username or password" and not which, because the server does
 * not know which either — it deliberately gives one answer for both so that a
 * failed login cannot be used to confirm an account name.
 *
 * The 429 is shown verbatim: the server's message names the number of seconds,
 * and "wait a minute" would be wrong in both directions once the throttle
 * escalates.
 */
function errorMessageOf(error: unknown): string | undefined {
  if (error === null || error === undefined) return undefined;
  if (!(error instanceof ApiError)) return 'Could not reach the server.';
  if (error.status === 401) return 'Incorrect username or password.';
  if (error.status === 429) return error.message;
  return error.message;
}
