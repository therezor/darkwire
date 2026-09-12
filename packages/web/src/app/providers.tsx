/**
 * Everything mounted once, above the router.
 *
 * The order is not arbitrary: the login overlay is inside `QueryClientProvider`
 * because it *is* a query — `/api/auth/me` — and outside the router because it
 * covers whatever route is behind it rather than replacing it.
 */

import { QueryClientProvider, type QueryClient } from '@tanstack/react-query';
import type { JSX, ReactNode } from 'react';
import { useState } from 'react';

import { createQueryClient } from '@/lib/query.js';
import { I18nProvider } from '@/i18n/i18n-context.js';
import { LoginOverlay } from '@/components/login-overlay.js';
import { Toaster } from '@/components/ui/toast.js';
import { TooltipProvider } from '@/components/ui/tooltip.js';
import { SetupOverlay } from '@/setup/setup-overlay.js';
import { ThemeProvider } from '@/theme/theme-context.js';
import { TimezoneProvider } from '@/timezone/timezone-context.js';
import { WorkspaceProvider } from '@/workspaces/workspace-context.js';
import { AgentProvider } from '@/agents/agent-context.js';

export function Providers({
  children,
  client,
}: {
  readonly children: ReactNode;
  /** Supplied by tests; the app builds its own. */
  readonly client?: QueryClient;
}): JSX.Element {
  // `useState` rather than a module constant: a client created at import time
  // is shared by every test in a file, so one test's cache answers another's
  // request.
  const [queryClient] = useState(() => client ?? createQueryClient());

  return (
    <QueryClientProvider client={queryClient}>
      {/* One copy of the theme state for the whole tree. Two would mean the
          toggle repaints and the code blocks do not. */}
      <ThemeProvider>
        {/* Inside the theme for the same reason the theme is inside the query
            client: it is a preference with the same shape, and the half of it
            that is *not* a preference — `config.ui.locale` — arrives as a
            query. Above everything that renders copy, which is everything. */}
        <I18nProvider>
          {/* Beside the locale and for the same reason: `ui.timezone` is the
              other half of "how this install reads", it arrives on the same
              query, and one copy is what stops two lists disagreeing about
              what a timestamp meant. */}
          <TimezoneProvider>
            {/* Which workspace the UI is showing. Above the router because the
                sidebar, the Files page and the socket all read it, and two
                copies would let the switcher and the file listing disagree. */}
            <WorkspaceProvider>
              {/* Which agent the *next* conversation starts on. Beside the
                  workspace because the two answer the pair of questions a
                  conversation is opened with: where it works and who does it. */}
              <AgentProvider>
                {/* One provider, one shared delay timer — so a row of icon
                    buttons behaves like a control strip rather than eight
                    separate waits. */}
                <TooltipProvider delayDuration={400} skipDelayDuration={300}>
                  {children}
                  <LoginOverlay />
                  {/* Above the login overlay in the tree, because on an unclaimed
                      install `/api/auth/me` also 401s and both would otherwise be
                      showing — and this is the one that can actually get the user
                      in. It renders itself only when the server says setup is
                      needed, so a claimed install never sees it. */}
                  <SetupOverlay />
                  <Toaster />
                </TooltipProvider>
              </AgentProvider>
            </WorkspaceProvider>
          </TimezoneProvider>
        </I18nProvider>
      </ThemeProvider>
    </QueryClientProvider>
  );
}
