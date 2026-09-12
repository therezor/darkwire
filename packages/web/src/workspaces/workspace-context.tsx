/**
 * Which workspace the UI is showing.
 *
 * Not in `state/turn.ts`: that store is documented as live-turn state only —
 * the frames a socket accumulates — and the current workspace is a persisted
 * preference, closer to the theme than to a turn. So this follows
 * `theme/theme-context.tsx`: `localStorage` plus one context mounted in
 * `providers.tsx`.
 *
 * Two rules keep it honest:
 *
 *  - **The Files page mirrors it into `?workspace=`.** That page's own doctrine
 *    is that its location lives in the URL, and a link to a file is not shareable
 *    if half of the address is in somebody else's `localStorage`. The context is
 *    what the parameter defaults to, not a second source of truth.
 *  - **The server can move it.** Opening a session that belongs to another
 *    workspace moves the UI to *that* workspace — the hub says which one on
 *    `connected` and `session.status`, and `adopt` is how that answer gets back
 *    here. Without it the switcher would claim one workspace while the
 *    conversation on screen ran in another.
 */

import {
  createContext,
  useCallback,
  useContext,
  useState,
  type JSX,
  type ReactNode,
} from 'react';

import { DEFAULT_WORKSPACE_ID } from '@ghostwire/protocol';

const STORAGE_KEY = 'ghostai:workspace';

/**
 * The workspace every install has. Also the parent folder of all the others.
 *
 * Re-exported rather than declared, so the half-dozen files that already import
 * it from here keep working while there is only one `'default'` in the
 * codebase. It belongs with the rest of the id rules — the same module that
 * says which ids are reserved and how a name becomes a folder.
 */
export { DEFAULT_WORKSPACE_ID };

interface WorkspaceState {
  readonly workspaceId: string;
  /** The user chose this one. Persisted. */
  readonly select: (workspaceId: string) => void;
  /**
   * The server said the session is in this one.
   *
   * Persisted like a choice, because it *is* where the user now is — but named
   * apart from `select` so a component cannot accidentally treat a report as a
   * request and echo it back over the socket.
   */
  readonly adopt: (workspaceId: string) => void;
}

/**
 * The stored choice, or the default.
 *
 * Wrapped rather than optional-chained: the type says `localStorage` is always
 * there and the environments where it is not — private browsing, a hardened
 * container, a test stub that never ran — throw on access rather than being
 * absent. One `catch` covers both that and a quota error.
 */
function readStored(): string {
  try {
    const stored = globalThis.localStorage.getItem(STORAGE_KEY);
    return stored === null || stored === '' ? DEFAULT_WORKSPACE_ID : stored;
  } catch {
    return DEFAULT_WORKSPACE_ID;
  }
}

function store(workspaceId: string): void {
  try {
    globalThis.localStorage.setItem(STORAGE_KEY, workspaceId);
  } catch {
    // Not being able to remember the choice is not a reason to refuse it.
  }
}

const WorkspaceContext = createContext<WorkspaceState | undefined>(undefined);

export function WorkspaceProvider({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  const [workspaceId, setWorkspaceId] = useState<string>(readStored);

  const move = useCallback((next: string): void => {
    setWorkspaceId((current) => {
      if (current === next) return current;
      store(next);
      return next;
    });
  }, []);

  return (
    <WorkspaceContext value={{ workspaceId, select: move, adopt: move }}>
      {children}
    </WorkspaceContext>
  );
}

/**
 * The current workspace.
 *
 * Falls back to a non-reactive default outside the provider rather than
 * throwing, matching `useAppTheme`: a component under test that only wants to
 * know which workspace it is in should not have to mount the app to find out.
 */
export function useWorkspace(): WorkspaceState {
  return (
    useContext(WorkspaceContext) ?? {
      workspaceId: DEFAULT_WORKSPACE_ID,
      select: () => undefined,
      adopt: () => undefined,
    }
  );
}
