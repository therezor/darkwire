/**
 * Response fixtures, typed against the schemas the client actually parses.
 *
 * **The typing is the entire point of this file.** `api.status()` parses the
 * response through `StatusResponseSchema`, so a stub missing one required field
 * does not fail loudly — the parse rejects, `status.data` stays `undefined`, and
 * every component that reads it renders its empty state. That looks exactly like
 * the bug under investigation, and it is silent: no test goes red, because the
 * assertions in those files are about other things.
 *
 * It happened. `workspace: '/tmp/w'` outlived the field being replaced by
 * `workspaceId` plus `workspaceCount`, and six test files carried the dead shape
 * for as long as it took someone to write an assertion that depended on it. One
 * consequence worth naming: `chat.test.tsx` has a case for the "No model is
 * configured yet" notice, and that notice could never render, because the flag it
 * branches on lives on a response the client was throwing away.
 *
 * Declaring these `satisfies StatusResponse` turns the next such change into a
 * compile error in one place instead of silence in six.
 */

import type { AgentListResponse, StatusResponse } from '@ghostwire/protocol';

/** A configured install. Spread it to vary one field: `{ ...STATUS, configured: false }`. */
export const STATUS = {
  version: '0.0.0',
  protocolVersion: 2,
  uptimeMs: 1,
  model: 'test-model',
  provider: 'ollama',
  configured: true,
  workspaceId: 'default',
  workspaceCount: 1,
  authEnabled: false,
  toolCount: 3,
  mcpServersConnected: 0,
  extensionsLoaded: 0,
} satisfies StatusResponse;

/**
 * A fresh install: the server is up and only chat is unavailable.
 *
 * Both strings empty *and* `configured: false`, which is the contract
 * `StatusResponseSchema` documents — a client branches on the flag rather than
 * reading meaning into an empty model name.
 */
export const UNCONFIGURED_STATUS = {
  ...STATUS,
  model: '',
  provider: '',
  configured: false,
} satisfies StatusResponse;

/**
 * Two agents whose models differ, because that difference is what most agent
 * assertions are about: `default` inherits the install's model and `researcher`
 * pins its own.
 */
export const AGENTS = {
  agents: [
    {
      id: 'default',
      label: 'Default',
      model: 'test-model',
      provider: 'ollama',
    },
    {
      id: 'researcher',
      label: 'Researcher',
      model: 'pinned-research-model',
      provider: 'lmstudio',
    },
  ],
} satisfies AgentListResponse;
