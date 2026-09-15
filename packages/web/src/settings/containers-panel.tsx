import { useState, type JSX } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from '@/lib/api.js';
import { Button } from '@/components/ui/button.js';
import { Section } from '@/components/form/controls.js';
import { queryKeys } from '@/lib/query.js';
import { useTranslation } from 'react-i18next';

/**
 * The containers the sandbox service is holding open, and their lifecycle.
 *
 * Operator lifecycle only. Nothing here approves a definition or changes what
 * an agent may run: stopping an instance gets a fresh one on the next command,
 * under the same approval. That is what makes it safe to expose in a browser
 * where approval is deliberately not.
 *
 * **Only shared containers can be warmed.** A private instance is keyed on the
 * agent and conversation that will use it, so one started from here would be
 * keyed on nothing real and sit idle until it was reaped — warm in the list and
 * never reused. Shared ones are keyed on the workspace, the definition and the
 * network, all of which this form can supply.
 */
export function ContainersPanel(): JSX.Element {
  const { t } = useTranslation();
  const client = useQueryClient();
  const [container, setContainer] = useState('');
  const [workspace, setWorkspace] = useState('default');
  const [force, setForce] = useState(false);
  const installed = useQuery({
    queryKey: queryKeys.containers,
    queryFn: ({ signal }) => api.containers(signal),
  });
  const approved =
    installed.data?.containers.filter((entry) => entry.approved) ?? [];
  const warmable = approved.filter((entry) => entry.shared);
  const instances = useQuery({
    queryKey: queryKeys.containerInstances,
    queryFn: ({ signal }) => api.sandboxes(signal),
    enabled: approved.length > 0,
    refetchInterval: 5000,
    retry: false,
  });
  const mutation = useMutation({
    mutationFn: api.manageSandbox,
    onSuccess: async () => {
      setForce(false);
      await client.invalidateQueries({
        queryKey: queryKeys.containerInstances,
      });
    },
  });
  if (installed.isPending || approved.length === 0) return <></>;
  return (
    <Section
      title={t('settings.tools.containers.title')}
      description={t('settings.tools.containers.description')}
    >
      {instances.error && <p role="status">{instances.error.message}</p>}
      {mutation.error && <p role="alert">{mutation.error.message}</p>}
      {warmable.length > 0 && (
        <>
          <label>
            {t('settings.tools.containers.container')}{' '}
            <select
              value={container}
              onChange={(event) => {
                setContainer(event.target.value);
              }}
            >
              <option value="">{t('settings.tools.containers.select')}</option>
              {warmable.map((entry) => (
                <option key={entry.name} value={entry.name}>
                  {entry.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            {t('settings.tools.containers.workspace')}{' '}
            <input
              value={workspace}
              onChange={(event) => {
                setWorkspace(event.target.value);
              }}
            />
          </label>
          <Button
            disabled={!container || !workspace || mutation.isPending}
            onClick={() => {
              mutation.mutate({
                op: 'start',
                container,
                workspace,
                agent: 'operator',
                session: 'operator',
                // No network, which is also an agent's default. An instance's
                // egress is part of its identity, so warming with one an agent
                // did not ask for would start a container nothing reuses.
                network: { mode: 'none', allow: [], hosts: [], dns: [] },
              });
            }}
          >
            {t('settings.tools.containers.start')}
          </Button>
        </>
      )}
      <label>
        <input
          type="checkbox"
          checked={force}
          onChange={(event) => {
            setForce(event.target.checked);
          }}
        />{' '}
        {t('settings.tools.containers.force')}
      </label>
      {instances.data?.instances.length === 0 && (
        <p>{t('settings.tools.containers.noneRunning')}</p>
      )}
      <ul className="stack">
        {instances.data?.instances.map((instance) => (
          <li key={instance.id}>
            <p>
              <code>{instance.id}</code> · {instance.workspace} ·{' '}
              {instance.container} ·{' '}
              {instance.shared
                ? t('settings.tools.containers.shared')
                : t('settings.tools.containers.private')}{' '}
              ·{' '}
              {t('settings.tools.containers.activeCommands', {
                count: instance.busy,
              })}
            </p>
            {instance.agents.length > 0 && (
              <p>
                {t('settings.tools.containers.agents', {
                  agents: instance.agents.join(', '),
                })}
              </p>
            )}
            <Button
              disabled={mutation.isPending || (instance.busy > 0 && !force)}
              onClick={() => {
                mutation.mutate({ op: 'stop', instance: instance.id, force });
              }}
            >
              {t('settings.tools.containers.stop')}
            </Button>
            <Button
              disabled={mutation.isPending || (instance.busy > 0 && !force)}
              onClick={() => {
                mutation.mutate({
                  op: 'restart',
                  instance: instance.id,
                  force,
                });
              }}
            >
              {t('settings.tools.containers.restart')}
            </Button>
          </li>
        ))}
      </ul>
    </Section>
  );
}
