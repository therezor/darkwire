/**
 * The login overlay.
 *
 * The behaviour under test is a decision tree that is easy to get subtly wrong
 * and expensive when it is: showing the overlay on a server with authentication
 * disabled locks a user out of their own agent, and *not* showing it on a 401
 * leaves them staring at an empty shell whose every panel failed silently.
 *
 * It is never rendered directly here. `Providers` mounts it — that is the
 * composition that ships, and a test that rendered the component on its own
 * would keep passing after someone removed it from the provider stack.
 */

import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import {
  renderWithProviders,
  stubApi,
  stubFetch,
  urlOf,
} from '@testkit/render.js';

describe('the login overlay', () => {
  it('stays out of the way when the caller is authenticated', async () => {
    stubFetch({
      '/api/auth/me': [200, { authenticated: true, authEnabled: true }],
      '/api/setup': [200, { required: false }],
    });
    renderWithProviders(<main>The app behind it</main>);

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('stays out of the way when authentication is disabled', async () => {
    stubFetch({
      '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
      '/api/setup': [200, { required: false }],
    });
    renderWithProviders(<main>The app behind it</main>);

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('appears on a 401 and takes the password', async () => {
    const user = userEvent.setup();
    const responses = vi.fn((input: RequestInfo | URL) => {
      const url = urlOf(input);
      if (url === '/api/auth/login') {
        return Promise.resolve(
          new Response(JSON.stringify({ ok: true, expiresAtMs: 99 }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        );
      }
      // Claimed, so the overlay does not defer to the setup wizard.
      if (url === '/api/setup') {
        return Promise.resolve(
          new Response(JSON.stringify({ required: false }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        );
      }
      // Authenticated once the login has been posted — the same thing the
      // cookie does for a real browser.
      const authenticated = responses.mock.calls.some(
        ([called]) => urlOf(called) === '/api/auth/login',
      );
      return Promise.resolve(
        new Response(
          JSON.stringify(
            authenticated
              ? { authenticated: true, authEnabled: true }
              : { error: { code: 'unauthorized', message: 'No session' } },
          ),
          {
            status: authenticated ? 200 : 401,
            headers: { 'content-type': 'application/json' },
          },
        ),
      );
    });
    vi.stubGlobal('fetch', responses);

    renderWithProviders(<main>The app behind it</main>);

    const password = await screen.findByLabelText('Password');
    // The overlay is the only thing on screen, so the field takes focus.
    expect(password).toHaveFocus();

    await user.type(password, 'hunter2');
    await user.click(screen.getByRole('button', { name: 'Sign in' }));

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });

  it('distinguishes a wrong password from a rate limit', async () => {
    const user = userEvent.setup();
    stubFetch({
      '/api/auth/me': [
        401,
        { error: { code: 'unauthorized', message: 'No session' } },
      ],
      // Claimed: the setup overlay mounts above the login one and would
      // otherwise be deciding whether to open on an unstubbed request.
      '/api/setup': [200, { required: false }],
      '/api/auth/login': [
        429,
        { error: { code: 'rate_limited', message: 'Slow down' } },
      ],
    });

    renderWithProviders(<main>The app behind it</main>);

    await user.type(await screen.findByLabelText('Password'), 'guess');
    await user.click(screen.getByRole('button', { name: 'Sign in' }));

    // The server's own text, verbatim: it names the number of seconds to wait,
    // and a written-in-the-client "wait a minute" would be wrong in both
    // directions once the throttle escalates.
    expect(await screen.findByRole('alert')).toHaveTextContent('Slow down');
  });

  it('sends the username beside the password', async () => {
    const user = userEvent.setup();
    const calls = stubApi({
      '/api/auth/me': [
        401,
        { error: { code: 'unauthorized', message: 'No session' } },
      ],
      '/api/setup': [200, { required: false }],
      'POST /api/auth/login': [200, { ok: true, expiresAtMs: 99 }],
    });

    renderWithProviders(<main>The app behind it</main>);

    // Prefilled with the default, so the common case is one field to fill.
    const username = await screen.findByLabelText('Username');
    expect(username).toHaveValue('ghost');

    await user.clear(username);
    await user.type(username, 'operator');
    await user.type(screen.getByLabelText('Password'), 'a good password');
    await user.click(screen.getByRole('button', { name: 'Sign in' }));

    await waitFor(() => {
      expect(
        calls.find((call) => call.path === '/api/auth/login')?.body,
      ).toEqual({
        username: 'operator',
        password: 'a good password',
      });
    });
  });

  it('refuses to submit an empty password rather than spending an attempt', async () => {
    stubFetch({
      '/api/auth/me': [401, { error: { code: 'unauthorized', message: 'no' } }],
      '/api/setup': [200, { required: false }],
    });
    renderWithProviders(<main>The app behind it</main>);

    expect(
      await screen.findByRole('button', { name: 'Sign in' }),
    ).toBeDisabled();
  });

  it('says so when the server cannot be reached at all', async () => {
    const user = userEvent.setup();
    vi.stubGlobal('fetch', (input: RequestInfo | URL) => {
      if (urlOf(input) === '/api/auth/login') {
        return Promise.reject(new TypeError('offline'));
      }
      if (urlOf(input) === '/api/setup') {
        return Promise.resolve(
          new Response(JSON.stringify({ required: false }), {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        );
      }
      return Promise.resolve(
        new Response(
          JSON.stringify({ error: { code: 'unauthorized', message: 'no' } }),
          {
            status: 401,
            headers: { 'content-type': 'application/json' },
          },
        ),
      );
    });

    renderWithProviders(<main>The app behind it</main>);

    await user.type(await screen.findByLabelText('Password'), 'hunter2');
    await user.click(screen.getByRole('button', { name: 'Sign in' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Could not reach the server.',
    );
  });
});
