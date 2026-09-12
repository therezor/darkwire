/**
 * Which agent a *new* conversation starts on.
 *
 * The same shape as `workspaces/workspace-context.tsx`, and for the same
 * reason: it is a persisted preference rather than live-turn state, so it
 * follows `theme-context` — `localStorage` plus one context mounted in
 * `providers.tsx` — instead of living in the turn store.
 *
 * What differs is the scope of the choice, and it is worth being exact about.
 * A workspace can be switched under an open conversation: the folder is not
 * part of the history. An agent cannot. A session is bound to one when it is
 * created, and after that the binding belongs to the stored row — the server
 * ignores an agent named on a frame for a session that already exists, because
 * a history built under one agent's prompt, tools and permissions must not
 * silently continue under another's.
 *
 * So this is the agent the *next* conversation will use. `adopt` is how the
 * switcher follows an existing session rather than claiming to have changed it:
 * opening a conversation that belongs to another agent moves the control to
 * that agent, and starting a new one from there keeps it.
 *
 * Both callers of `adopt` are in `use-agent-choice.ts` — the effect that
 * follows an open conversation's binding, and the move that has just changed
 * one. This file holds no policy about *when* either happens: it has no
 * listing, no session and no way to tell a live id from a deleted one.
 */

import {
  createContext,
  useCallback,
  useContext,
  useState,
  type JSX,
  type ReactNode,
} from 'react';

import { DEFAULT_AGENT_ID } from '@ghostwire/protocol';

const STORAGE_KEY = 'ghostai:agent';

export { DEFAULT_AGENT_ID };

interface AgentState {
  /** The agent a new conversation starts on. */
  readonly agentId: string;
  /** The user chose this one. Persisted. */
  readonly select: (agentId: string) => void;
  /**
   * The open conversation belongs to this one.
   *
   * Named apart from `select` so a component cannot treat a report as a
   * request: the binding is the session row's, and echoing it back would be
   * claiming to set something that is already set.
   */
  readonly adopt: (agentId: string) => void;
}

/**
 * The stored choice, or the default.
 *
 * Wrapped rather than optional-chained, matching the workspace context: the
 * environments where `localStorage` is missing throw on access rather than
 * being absent, and one `catch` covers that and a quota error together.
 */
function stored(): string {
  try {
    return localStorage.getItem(STORAGE_KEY) ?? DEFAULT_AGENT_ID;
  } catch {
    return DEFAULT_AGENT_ID;
  }
}

function remember(agentId: string): void {
  try {
    localStorage.setItem(STORAGE_KEY, agentId);
  } catch {
    // A preference that cannot be persisted is still a preference for this tab.
  }
}

const AgentContext = createContext<AgentState | undefined>(undefined);

export function AgentProvider({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  const [agentId, setAgentId] = useState<string>(stored);

  const move = useCallback((next: string): void => {
    setAgentId(next);
    remember(next);
  }, []);

  return (
    <AgentContext.Provider value={{ agentId, select: move, adopt: move }}>
      {children}
    </AgentContext.Provider>
  );
}

export function useAgent(): AgentState {
  const state = useContext(AgentContext);
  if (state === undefined) {
    throw new Error('useAgent must be used inside an AgentProvider');
  }
  return state;
}
