/**
 * What a screen shows when it throws, instead of a blank page.
 *
 * Two places catch a throw. The router catches one inside a route, so the
 * header and sidebar stay and the rest of the app still works. The boundary in
 * `providers.tsx` catches the rest: an overlay, the toaster, the shell itself.
 */

import { Component, type ErrorInfo, type JSX, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '@/components/ui/button.js';

export function ErrorScreen({
  error,
}: {
  readonly error: unknown;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="stack page page--reading" role="alert">
      <h1 className="page__title">{t('shell.crashed')}</h1>
      <p className="page__note">{t('shell.crashedBody')}</p>
      {error instanceof Error && error.message !== '' && (
        <p className="page__note">{error.message}</p>
      )}
      <div>
        <Button
          variant="secondary"
          onClick={() => {
            globalThis.location.reload();
          }}
        >
          {t('shell.reload')}
        </Button>
      </div>
    </div>
  );
}

interface BoundaryState {
  readonly error: unknown;
  readonly failed: boolean;
}

/** React has no hook for this; a class is the only way to catch a render. */
export class ErrorBoundary extends Component<
  { readonly children: ReactNode },
  BoundaryState
> {
  override state: BoundaryState = { error: undefined, failed: false };

  static getDerivedStateFromError(error: unknown): BoundaryState {
    return { error, failed: true };
  }

  override componentDidCatch(error: unknown, info: ErrorInfo): void {
    console.error(error, info.componentStack);
  }

  override render(): ReactNode {
    return this.state.failed ? (
      <ErrorScreen error={this.state.error} />
    ) : (
      this.props.children
    );
  }
}
