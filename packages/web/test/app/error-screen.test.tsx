/**
 * A throw anywhere in the app shows the app's own error screen, not a blank
 * page with nothing to press.
 */

import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { Providers } from '@/app/providers.js';
import { stubFetch } from '@testkit/render.js';

function Broken(): never {
  throw new Error('the panel fell over');
}

describe('a component that throws', () => {
  it('is replaced by the error screen, with a way back', async () => {
    // React reports a caught render error on the console as well.
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    stubFetch({
      '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
      '/api/setup': [200, { required: false }],
    });

    render(
      <Providers>
        <Broken />
      </Providers>,
    );

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Something went wrong',
    );
    expect(screen.getByText('the panel fell over')).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'Reload app' }),
    ).toBeInTheDocument();
  });
});
