/**
 * The one place the command table meets the application.
 *
 * `commands.ts` names what it needs and translates nothing; this file supplies
 * both. It is the only file in the feature that knows about `api`, the socket,
 * the router, the query cache or a toast, which is what keeps the table itself
 * testable against a plain object.
 *
 * **What it returns is synchronous, and that is load-bearing.** Enter has to
 * decide *now* whether what was typed is a command, because the answer decides
 * whether the box clears — and `parseCommand` is pure, so it can. The effect is
 * then fired and not awaited. An `await` between the keypress and the clear is a
 * window in which a second Enter runs the same command again, and `/branch`
 * forking twice is not a hazard worth a lock. It is the shape `runAction` in
 * `routes/chat.tsx` already has, for the same reason.
 */

import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import type { TFunction } from 'i18next';
import { useCallback } from 'react';
import { useTranslation } from 'react-i18next';

import {
  agentSettingsPatch,
  type AgentSettingsChange,
  type ModelInfo,
} from '@ghostwire/protocol';

import { useAgent } from '@/agents/agent-context.js';
import { useAgentChoice } from '@/agents/use-agent-choice.js';
import { toast } from '@/components/ui/toast.js';
import { api } from '@/lib/api.js';
import { newSession, stopTurn } from '@/lib/connection.js';
import { queryKeys } from '@/lib/query.js';
import { afterSettingsWrite } from '@/settings/use-settings.js';
import { useTurnStore } from '@/state/turn.js';
import type { Transcript } from '@/state/transcript.js';
import { useWorkspace } from '@/workspaces/workspace-context.js';
import { useExtensionCommands } from './use-extension-commands.js';
import {
  parseCommand,
  runCommand,
  type CommandContext,
  type CommandOutcome,
} from './commands.js';

/**
 * Runs what was typed if it is a command, and says whether it was one.
 *
 * `true` means consumed — the effect is under way and the box should clear.
 * `false` means prose, and the message goes out as it always did.
 */
export type RunCommand = (text: string) => boolean;

export function useCommands(): RunCommand {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { workspaceId } = useWorkspace();
  // Fetched, because the answer changes while the page is open: approving an
  // extension adds a command to a composer nobody reloaded. Its own hook rather
  // than a field on the extensions query — the composer wants four strings on
  // every `/`, and the panel wants a row's `lastError`.
  const extensionCommands = useExtensionCommands();

  const sessionKey = useTurnStore((state) => state.sessionKey);
  const busy = useTurnStore((state) => state.busy);
  const transcript = useTurnStore((state) => state.transcript);

  // The *preference*, not this conversation's binding — the two differ once a
  // session has been moved, and `/new` means the same thing the sidebar's New
  // session button means. `useAgentChoice` owns the other question.
  const { agentId } = useAgent();
  const { agents, current, stored, choose } = useAgentChoice(sessionKey);

  return useCallback(
    (text: string): boolean => {
      const parsed = parseCommand(text);
      if (parsed === undefined) return false;

      // Every command that reaches a route needs one, and every one of those is
      // behind the `stored` guard — which is false when there is no key at all.
      const key = sessionKey ?? '';

      /**
       * Moves one field on the agent this conversation runs on.
       *
       * **The tree is fetched, not read from the cache.** `agents.list.*` is in
       * the merge's `REPLACE_WHOLESALE` list — the patch *is* the agent — so
       * `agentSettingsPatch` sends the stored entry back whole with one field
       * changed. Spreading a cached copy would send back whatever the cache
       * last saw: edit an agent's system prompt in the Agents panel, type
       * `/model` here, and the pre-edit entry would be written over the edit.
       * `ensureQueryData` — which `models()` above uses, correctly, for a list
       * whose staleness is harmless — returns cached data without refetching,
       * so it is exactly the wrong call here.
       *
       * `current` rather than the `agentId` preference: a conversation that has
       * been moved runs on its stored binding, and this has to edit the agent
       * that will actually answer.
       */
      const editAgent = async (changes: AgentSettingsChange): Promise<void> => {
        const settings = await queryClient.fetchQuery({
          queryKey: queryKeys.settings,
          queryFn: ({ signal }) => api.settings(signal),
        });
        const saved = await api.patchSettings(
          agentSettingsPatch(settings.config, current, changes),
        );
        // The shared fan-out rather than a list of its own — including
        // `queryKeys.agents`, which is what moves the composer's picker onto
        // the new model. See `afterSettingsWrite`.
        afterSettingsWrite(queryClient, saved);
      };

      const ctx: CommandContext = {
        sessionKey,
        workspaceId,
        agentId,
        busy,
        stored,
        lastUserSeq: lastUserSeq(transcript),
        agents,
        models: async (): Promise<readonly ModelInfo[]> =>
          (
            await queryClient.ensureQueryData({
              queryKey: queryKeys.models,
              queryFn: ({ signal }) => api.models(signal),
            })
          ).models,
        newSession: () => newSession(workspaceId, agentId),
        openSession: (target) => {
          void navigate({ to: '/', search: { session: target } });
        },
        rename: async (title) => {
          await api.renameSession(key, title);
          void queryClient.invalidateQueries({
            queryKey: queryKeys.sessions(),
          });
        },
        clear: async () => {
          await api.clearMessages(key);
          // The transcript on screen empties from the `session.reset` frame the
          // route answers with; this is for the stored copy React Query holds,
          // which a reload would otherwise read back.
          void queryClient.invalidateQueries({
            queryKey: queryKeys.messages(key),
          });
        },
        branch: async (seq) => {
          const fork = await api.branchSession(key, seq);
          void queryClient.invalidateQueries({
            queryKey: queryKeys.sessions(),
          });
          return fork.key;
        },
        stop: stopTurn,
        chooseAgent: choose,
        extensionCommands: extensionCommands.map((command) => command.id),
        runExtensionCommand: async (id, args) => {
          const answer = await api.runCommand(id, {
            args,
            ...(sessionKey === undefined ? {} : { sessionKey }),
          });
          return { message: answer.message, ok: answer.ok };
        },
        agentLabel: agents.find((one) => one.id === current)?.label ?? current,
        setModel: async (model) => {
          // Both halves, because they are one setting: an agent's `provider`
          // and `model` are inherited per field, so sending the model alone
          // would leave `provider` naming an instance that never offered it.
          // This is the pair the agent editor's Save writes.
          await editAgent({ provider: model.providerId, model: model.id });
        },
        setEffort: async (effort) => {
          await editAgent({ reasoningEffort: effort });
        },
        setTemperature: async (temperature) => {
          await editAgent({ temperature });
        },
      };

      // Fired, not awaited — see the file docblock. A rejected request is the
      // only failure the table cannot describe itself, so it is reported here,
      // with the words the server sent.
      runCommand(parsed, ctx)
        .then((outcome) => {
          report(outcome, t);
        })
        .catch((error: unknown) => {
          toast.error(
            t('chat.commands.errors.failed'),
            error instanceof Error ? error.message : undefined,
          );
        });
      return true;
    },
    [
      agentId,
      agents,
      busy,
      choose,
      current,
      extensionCommands,
      navigate,
      queryClient,
      sessionKey,
      stored,
      t,
      transcript,
      workspaceId,
    ],
  );
}

/**
 * The seq `/branch` forks at: the last thing the user said.
 *
 * Unchanged, unlike the transcript's own "Branch from here", which passes
 * `seq - 1` so the message it is under can be re-asked. This is the terminal's
 * reading of `/branch` — fork inclusively at the resolved seq — and the two
 * differ by one on purpose.
 */
function lastUserSeq(transcript: Transcript): number | undefined {
  for (let index = transcript.length - 1; index >= 0; index -= 1) {
    const item = transcript[index];
    if (item?.kind === 'user' && item.seq !== undefined) return item.seq;
  }
  return undefined;
}

function report(outcome: CommandOutcome, t: TFunction): void {
  // An extension's answer arrives as words rather than a key — its copy ships
  // with the extension, so there is nothing here to look up. See
  // `CommandOutcome`.
  const sentence =
    'text' in outcome ? outcome.text : t(outcome.key, outcome.values ?? {});
  if (outcome.kind === 'error') {
    toast.error(sentence);
    return;
  }
  toast.success(sentence);
}
