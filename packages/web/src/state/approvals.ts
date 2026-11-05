/**
 * Every tool call waiting for somebody to approve it, in every session.
 *
 * The server's list, not one this tab works out from its transcript. A tab
 * only receives frames for the session it shows, an answer given in another
 * tab sends nothing here until the tool finishes, and an expiry changes
 * nothing on screen. `approvals.pending` is sent on each of those, so the tab
 * title, the favicon and the sidebar can all trust it.
 */

import { create } from 'zustand';

import type { PendingApproval } from '@darkwire/protocol';

interface ApprovalsState {
  readonly pending: readonly PendingApproval[];
  /** Replaces the list. Each frame is the whole of it. */
  readonly receive: (pending: readonly PendingApproval[]) => void;
  /** Forgets the list, for a socket that is no longer listening. */
  readonly clear: () => void;
}

export const useApprovalsStore = create<ApprovalsState>()((set) => ({
  pending: [],
  receive: (pending) => {
    set({ pending });
  },
  clear: () => {
    set({ pending: [] });
  },
}));
