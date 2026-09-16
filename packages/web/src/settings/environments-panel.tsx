import { useState, type JSX } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '@/lib/api.js';
import { Button } from '@/components/ui/button.js';
import { Section } from '@/components/form/controls.js';
import { queryKeys } from '@/lib/query.js';
import { useTranslation } from 'react-i18next';

/**
 * The environments an operator installed, and the containers held open for them.
 *
 * Two halves, and the split is what the operator can act on. The **installed**
 * list is read-only: the definition file on disk *is* the policy, so there is no
 * verb here that changes one, and what it shows is what an agent's picker will
 * offer. The **running** list is lifecycle only. Stopping an instance gets a
 * fresh one on the next command, from the same definition.
 *
 * Read from the server on every visit rather than cached across one: an edited
 * definition must report its new capabilities and its new gateway verdict the
 * moment it changes, and an operator who just fixed one is the likeliest visitor.
 *
 * **Only shared environments can be warmed.** A private instance is keyed on the
 * agent and conversation that will use it, so one started from here would be
 * keyed on nothing real and sit idle until it was reaped: warm in the list and
 * never reused. Shared ones are keyed on the workspace, the definition and the
 * network, all of which this form can supply.
 */
export function EnvironmentsPanel(): JSX.Element {
  const { t } = useTranslation();
  const client = useQueryClient();
  const [environment, setEnvironment] = useState('');
  const [workspace, setWorkspace] = useState('default');
  const installed = useQuery({
    queryKey: queryKeys.environments,
    queryFn: ({ signal }) => api.environments(signal),
  });
  const definitions = installed.data?.environments ?? [];
  const warmable = definitions.filter((entry) => entry.shared);
  const instances = useQuery({
    queryKey: queryKeys.environmentInstances,
    queryFn: ({ signal }) => api.sandboxes(signal),
    enabled: definitions.length > 0,
    refetchInterval: 5000,
    retry: false,
  });
  const mutation = useMutation({
    mutationFn: api.manageSandbox,
    onSuccess: async () => {
      await client.invalidateQueries({
        queryKey: queryKeys.environmentInstances,
      });
    },
  });
  return (
    <>
      <Section
        title={t('settings.environments.installedTitle')}
        description={t('settings.environments.installedDescription')}
      >
        {installed.isPending && <p>{t('settings.environments.loading')}</p>}
        {!installed.isPending && definitions.length === 0 && (
          // Says where to put one rather than rendering an empty list. A blank
          // panel reads as a broken screen; this reads as a step not taken yet.
          <p>{t('settings.environments.noneInstalled')}</p>
        )}
        <ul className="stack">
          {definitions.map((entry) => (
            <li key={entry.name}>
              <p>
                <code>{entry.name}</code> · {entry.kind} ·{' '}
                {entry.shared
                  ? t('settings.environments.shared')
                  : t('settings.environments.private')}
              </p>
              <p>
                <code>{entry.image}</code>
              </p>
              <p>
                {t('settings.environments.runtimeLine', {
                  runtime: entry.runtime,
                  user: entry.user,
                  workdir: entry.workdir,
                })}
              </p>
              <p>
                {t('settings.environments.limitsLine', {
                  memory: entry.limits.memoryMb,
                  cpus: entry.limits.cpus,
                  pids: entry.limits.pidsMax,
                })}
              </p>
              {entry.capsAdded.length > 0 && (
                <p>
                  {t('settings.environments.capsAdded', {
                    caps: entry.capsAdded.join(', '),
                  })}
                </p>
              )}
              {/* Warnings, not refusals: a weakened definition is usable, and
                  an operator who chose the weakening deserves to be reminded
                  rather than blocked. */}
              {entry.weakened.length > 0 && (
                <p role="status">
                  {t('settings.environments.weakened', {
                    what: entry.weakened.join(', '),
                  })}
                </p>
              )}
              {entry.gatewayProblem !== undefined && (
                <p role="status">
                  {t('settings.environments.gatewayProblem', {
                    why: entry.gatewayProblem,
                  })}
                </p>
              )}
              {entry.problem !== undefined && (
                <p role="alert">{entry.problem}</p>
              )}
            </li>
          ))}
        </ul>
      </Section>
      {definitions.length > 0 && (
        <Section
          title={t('settings.environments.runningTitle')}
          description={t('settings.environments.runningDescription')}
        >
          {instances.error && <p role="status">{instances.error.message}</p>}
          {mutation.error && <p role="alert">{mutation.error.message}</p>}
          {warmable.length > 0 && (
            <>
              <label>
                {t('settings.environments.environment')}{' '}
                <select
                  value={environment}
                  onChange={(event) => {
                    setEnvironment(event.target.value);
                  }}
                >
                  <option value="">{t('settings.environments.select')}</option>
                  {warmable.map((entry) => (
                    <option key={entry.name} value={entry.name}>
                      {entry.name}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                {t('settings.environments.workspace')}{' '}
                <input
                  value={workspace}
                  onChange={(event) => {
                    setWorkspace(event.target.value);
                  }}
                />
              </label>
              <Button
                disabled={!environment || !workspace || mutation.isPending}
                onClick={() => {
                  mutation.mutate({
                    op: 'start',
                    environment,
                    workspace,
                    agent: 'operator',
                    session: 'operator',
                    // No network, which is also an agent's default. An
                    // instance's egress is part of its identity, so warming
                    // with one an agent did not ask for would start a container
                    // nothing reuses.
                    network: { mode: 'none', allow: [], hosts: [], dns: [] },
                  });
                }}
              >
                {t('settings.environments.start')}
              </Button>
            </>
          )}
          {instances.data?.instances.length === 0 && (
            <p>{t('settings.environments.noneRunning')}</p>
          )}
          <ul className="stack">
            {instances.data?.instances.map((instance) => (
              <li key={instance.id}>
                <p>
                  <code>{instance.id}</code> · {instance.workspace} ·{' '}
                  {instance.environment} ·{' '}
                  {instance.shared
                    ? t('settings.environments.shared')
                    : t('settings.environments.private')}{' '}
                  ·{' '}
                  {t('settings.environments.activeCommands', {
                    count: instance.busy,
                  })}
                </p>
                {instance.agents.length > 0 && (
                  <p>
                    {t('settings.environments.agents', {
                      agents: instance.agents.join(', '),
                    })}
                  </p>
                )}
                <Button
                  disabled={mutation.isPending}
                  onClick={() => {
                    mutation.mutate({ op: 'stop', instance: instance.id });
                  }}
                >
                  {t('settings.environments.stop')}
                </Button>
                <Button
                  disabled={mutation.isPending}
                  onClick={() => {
                    mutation.mutate({ op: 'restart', instance: instance.id });
                  }}
                >
                  {t('settings.environments.restart')}
                </Button>
              </li>
            ))}
          </ul>
        </Section>
      )}
    </>
  );
}
