/**
 * New session, as the sidebar and the sessions page both offer it.
 *
 * Nothing is written. `newSession` mints a key and attaches the socket, and the
 * row appears when the first message lands, so pressing this and changing your
 * mind leaves no trace. The agent and the workspace go with it from the
 * composer's pickers, which is where the choice is made.
 */

import { useNavigate } from '@tanstack/react-router';
import { useCallback } from 'react';

import { useAgent } from '@/agents/agent-context.js';
import { newSession } from '@/lib/connection.js';
import { useWorkspace } from '@/workspaces/workspace-context.js';

/** Returns a function that starts one and opens it, and says its key. */
export function useStartSession(): () => string {
  const navigate = useNavigate();
  const { workspaceId } = useWorkspace();
  const { agentId } = useAgent();

  return useCallback(() => {
    const key = newSession(workspaceId, agentId);
    void navigate({
      to: '/sessions/$sessionKey',
      params: { sessionKey: key },
    });
    return key;
  }, [navigate, workspaceId, agentId]);
}
