/**
 * The two-column shell: sidebar, and everything else.
 *
 * Below `md` the left column is not narrowed, it is removed — and reached
 * through a Dialog instead. A sidebar squeezed to 4rem is a sidebar whose
 * labels are gone and whose targets are too small, which is worse on a phone
 * than a button that opens the real thing.
 *
 * The header carries the wordmark and then, in the space a hosted product would
 * spend on navigation, what the agent is doing: the socket's state, unread
 * notifications, and the theme control.
 *
 * Not the resolved provider and model: those are already on screen where they
 * belong — the welcome card names them before the first message, and the turn
 * inspector names the ones a given answer actually ran on. A header copy of the
 * same pair is a second place to read it and a second place for it to go
 * stale.
 *
 * The WebSocket hangs here, and here specifically: this is the router's root
 * component, so it is the only one that survives every navigation. A socket
 * opened by the chat route would be closed and redialled by a trip to Settings,
 * which would drop the turn the user went to Settings to reconfigure.
 */

import { useRouterState, useSearch } from '@tanstack/react-router';
import { Menu, RotateCw } from 'lucide-react';
import { useEffect, useState, type JSX, type ReactNode } from 'react';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';

import { api } from '@/lib/api.js';
import { useTurnStore } from '@/state/turn.js';
import { useConnection } from '@/chat/use-connection.js';
import { Button } from '@/components/ui/button.js';
import { toast } from '@/components/ui/toast.js';
import {
  Dialog,
  DialogContent,
  DialogHeading,
  DialogTrigger,
} from '@/components/ui/dialog.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';
import { NotificationBell } from '@/notifications/notification-bell.js';
import { ThemeSwitcher } from '@/components/theme-switcher.js';
import { Wordmark } from '@/components/wordmark.js';
import { Sidebar } from './sidebar.js';
import type { WebKey } from '@/i18n/keys.js';

/**
 * The tab title for each section, keyed by the path prefix that opens it.
 *
 * A section and everything under it share one title: the agent editor is an
 * Agents tab, and a tab strip full of "DarkWire" tells nobody which one to
 * click. The root is the bare name, because it is the app rather than a
 * section of it.
 */
const PAGE_TITLES: ReadonlyArray<readonly [string, WebKey]> = [
  ['/sessions', 'nav.sessions'],
  ['/agents', 'nav.agents'],
  ['/files', 'nav.files'],
  ['/workspaces', 'nav.workspaces'],
  ['/notifications', 'notifications.title'],
  ['/automation', 'nav.automation'],
  ['/settings', 'nav.settings'],
  ['/tokens', 'nav.tokens'],
];

/** The section of `pathname`, or `undefined` at the root and on an unknown path. */
export function pageTitleKey(pathname: string): WebKey | undefined {
  return PAGE_TITLES.find(
    ([prefix]) => pathname === prefix || pathname.startsWith(`${prefix}/`),
  )?.[1];
}

function usePageTitle(): void {
  const { t } = useTranslation();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  useEffect(() => {
    const key = pageTitleKey(pathname);
    document.title =
      key === undefined
        ? t('shell.appName')
        : t('shell.pageTitle', { page: t(key) });
  }, [pathname, t]);
}

export function Shell({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  const { t } = useTranslation();
  const [drawerOpen, setDrawerOpen] = useState(false);
  usePageTitle();

  // `strict: false` because the shell is above every route and only one of them
  // has a `session` parameter — on the others the answer is legitimately
  // `undefined`, which is a request to keep the socket where it is.
  const search: { session?: string } = useSearch({ strict: false });
  useConnection(search.session);

  return (
    <div className="shell">
      <Header onOpenDrawer={setDrawerOpen} drawerOpen={drawerOpen} />

      <div className="shell__body">
        <aside aria-label={t('shell.sidebar')} className="shell__sidebar">
          <Sidebar />
        </aside>

        <main className="shell__main">{children}</main>
      </div>
    </div>
  );
}

function Header({
  drawerOpen,
  onOpenDrawer,
}: {
  readonly drawerOpen: boolean;
  readonly onOpenDrawer: (open: boolean) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const connection = useTurnStore((state) => state.connection);
  // The header, not the chat route: the inspector measures the session the
  // socket is on, and that outlives a trip to Settings and back.

  return (
    <header className="app-header">
      <Dialog open={drawerOpen} onOpenChange={onOpenDrawer}>
        <DialogTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className="shell__menu-button"
            aria-label={t('shell.openMenu')}
          >
            <Menu />
          </Button>
        </DialogTrigger>
        <DialogContent className="dialog--drawer">
          <DialogHeading className="sr-only">
            {t('common.navigation')}
          </DialogHeading>
          <Sidebar
            onNavigate={() => {
              onOpenDrawer(false);
            }}
          />
        </DialogContent>
      </Dialog>

      <Wordmark className="app-header__brand" />

      <div className="spacer" />

      <ConnectionBadge connection={connection} />
      <NotificationBell />
      <ThemeSwitcher />
    </header>
  );
}

/**
 * Keyed rather than worded, so the map stays the exhaustive list of socket
 * states it already was — `keyof typeof` below is what makes an unhandled state
 * a compile error — while the words themselves live with the rest of the copy.
 */
const CONNECTION_LABELS = {
  connecting: 'connection.connecting',
  open: 'connection.open',
  reconnecting: 'connection.reconnecting',
  closed: 'connection.closed',
} as const;

/**
 * Both halves of a reload: the server's settings, then the page.
 *
 * The server first, and the page only if it answered. A tab that reloaded
 * itself and came back on the same stale config would look like the button did
 * nothing — and the reason (a `config.yaml` that does not parse) would have
 * been on screen for the length of a navigation. So a failure keeps the page
 * and says what happened; the operator can fix the file and press again.
 *
 * The page reload is not redundant once the server has rebuilt. Query has a
 * cache, the transcript has a store, and the built assets may be older than the
 * ones on disk — a navigation is the one thing that clears all three.
 */
async function reloadApp(t: TFunction): Promise<void> {
  try {
    await api.reloadSettings();
  } catch (error) {
    toast.error(
      t('shell.reloadFailed'),
      error instanceof Error ? error.message : t('shell.serverSilent'),
    );
    return;
  }
  globalThis.location.reload();
}

/**
 * A dot and a word, not a filled pill — and the menu they hang off.
 *
 * It was a `warning`-toned badge, and on a screen with one accent that made the
 * loudest element in the header a *transient* one: a socket reconnecting for
 * two seconds outranked the product's own name and everything the agent had
 * said. Status is ambient information — it needs to be readable when looked
 * for, not to compete when it is not. The dot carries the colour, which is as
 * much colour as a four-state indicator needs.
 *
 * Reload lives here rather than beside the theme control because this is where
 * someone already is when they want it: the reasons to reload — a socket wedged
 * in `reconnecting`, a config edited in an editor, a build that changed under a
 * tab left open for a week — are all read off this indicator, or read as its
 * absence of change. It is the server's settings and then the page, in that
 * order; `reloadApp` above says why.
 *
 * The live region stays on the wrapper rather than moving to the trigger: this
 * is the one piece of the header that changes without anyone acting, so a
 * socket that dropped has to announce itself to a user who is not looking at
 * the top-right corner — and a name that changes under an *open* menu is the
 * one that must not be the button's own accessible name.
 */
function ConnectionBadge({
  connection,
}: {
  readonly connection: keyof typeof CONNECTION_LABELS;
}): JSX.Element {
  const { t } = useTranslation();
  const label = t(CONNECTION_LABELS[connection]);

  return (
    <span
      role="status"
      aria-live="polite"
      className="conn"
      data-connection={connection}
    >
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="sm" className="conn__trigger">
            <span className="conn__dot" aria-hidden="true" />
            {/* Never `display: none`, even where the word is not drawn — see
                `shell.css`. It is the button's accessible name. */}
            <span className="conn__label">{label}</span>
          </Button>
        </DropdownMenuTrigger>

        <DropdownMenuContent align="end" className="floating--menu">
          {/* The state in words, for the narrow screens that draw the dot
              alone and for anyone who cannot tell green from amber. */}
          <DropdownMenuLabel>{label}</DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            onSelect={() => {
              void reloadApp(t);
            }}
          >
            <RotateCw />
            {t('shell.reload')}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </span>
  );
}
