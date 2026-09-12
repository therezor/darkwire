/**
 * The first-run wizard.
 *
 * An overlay rather than a route, for the reason the login overlay is one:
 * there is nothing behind it to navigate back from, and its "closed" state is a
 * finished setup rather than an Escape key. It sits *above* the login overlay,
 * because on an unclaimed install `/api/auth/me` also 401s and both would
 * otherwise render at once — and the one that can actually get the user in is
 * this one.
 *
 * Four steps, split by whether they may be skipped. Access — the code, then the
 * password — is mandatory, because an unclaimed server is a shell-capable agent
 * with no password. Configuration — a provider, then a model — is not, because
 * an install with no model still serves files, settings, workspaces and
 * notifications; only chat is unavailable. Skipping lands in a working app with
 * a pointer in the composer rather than in a dead end.
 *
 * The step machine lives in `setup-steps.ts` with no DOM in it. This file owns
 * the answers; that one owns which question comes next.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState, type JSX, type SyntheticEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { SUPPORTED_LOCALES } from '@ghostwire/i18n';

import { useAppLocale } from '@/i18n/i18n-context.js';
import { useSaveSettings } from '@/settings/use-settings.js';

import {
  DEFAULT_USERNAME,
  PASSWORD_MIN_LENGTH,
  type ProviderInfo,
  DEFAULT_AGENT_ID,
  agentSettingsPatch,
} from '@ghostwire/protocol';

import { ApiError, api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { Button } from '@/components/ui/button.js';
import { Field } from '@/components/ui/field.js';
import { Wordmark } from '@/components/wordmark.js';
import { SelectField, TextField } from '@/components/form/controls.js';
import {
  EMPTY_PROVIDER_FORM,
  toCreateProviderPatch,
} from '@/settings/provider-form.js';
import {
  initialStep,
  isSkippable,
  nextStep,
  previousStep,
  progressOf,
  titleOf,
  type SetupStep,
} from './setup-steps.js';

export function SetupOverlay(): JSX.Element | null {
  const queryClient = useQueryClient();
  const [step, setStep] = useState<SetupStep | null>(null);
  // Where this run opened, so Back cannot lead behind it.
  const [from, setFrom] = useState<SetupStep>('language');
  // Held rather than saved: `config.ui.locale` needs a session, and the language
  // is chosen before there is one. Written once `password` authenticates.
  const [pendingLocale, setPendingLocale] = useState<string | undefined>(
    undefined,
  );
  const [instanceId, setInstanceId] = useState('');
  const locale = useAppLocale();
  const { t } = useTranslation();
  const { save } = useSaveSettings();

  // Public, so it answers on a browser with no session — which is the only
  // state an unclaimed install has.
  const setup = useQuery({
    queryKey: queryKeys.setup,
    queryFn: ({ signal }) => api.setupStatus(signal),
  });

  // 401 while unclaimed, which is why the wizard cannot depend on it to decide
  // whether to open. It is read only to answer "is there a model yet".
  const status = useQuery({
    queryKey: queryKeys.status,
    queryFn: ({ signal }) => api.status(signal),
    retry: false,
  });

  const required = setup.data?.required;
  const configured = status.data?.configured;
  // Non-empty as soon as an endpoint resolves, which it does before a model is
  // chosen — `resolveProvider` picks the instance and only then refuses on the
  // missing model. So this is exactly "is there a provider already".
  const hasProvider = (status.data?.provider ?? '') !== '';

  useEffect(() => {
    // Only ever *opens* the wizard. Once a step is showing, this must not move
    // it: every step changes one of these two flags, and re-deriving would walk
    // the user backwards the moment they finished a step.
    if (step !== null || required === undefined) return;
    const first = initialStep({
      setupRequired: required,
      configured,
      hasProvider,
    });
    if (first !== null) setFrom(first);
    setStep(first);
  }, [step, required, configured, hasProvider]);

  if (step === null || step === 'done') return null;

  const finish = async (): Promise<void> => {
    setStep('done');
    // Everything fetched while unclaimed is a 401 in the cache.
    await queryClient.invalidateQueries();
  };

  const advance = (): void => {
    const next = nextStep(step);
    if (next === 'done') void finish();
    else setStep(next);
  };

  const { title, note } = titleOf(step, t);
  const progress = progressOf(step);
  const back = previousStep(step, from);

  return (
    // Not a Dialog: there is nothing behind it to return focus to, and its
    // "closed" state is a finished setup rather than an Escape key.
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="setup-title"
      className="setup-overlay"
    >
      <div className="stack setup-card">
        <div className="stack setup-card__header">
          <Wordmark className="eyebrow" />
          <p className="setup-card__progress">
            {t('setup.progress', {
              current: progress.current,
              total: progress.total,
            })}
          </p>
          <h1 id="setup-title" className="setup-card__title">
            {title}
          </h1>
          <p className="setup-card__note">{note}</p>
        </div>

        {step === 'language' && (
          <LanguageStep
            value={locale.resolved}
            onChange={(next) => {
              setPendingLocale(next);
              // Applied now, saved later: the wizard has to *become* the chosen
              // language immediately or the choice cannot be verified by the
              // person making it.
              locale.setPreference(next);
            }}
            onDone={advance}
          />
        )}
        {step === 'code' && <CodeStep onDone={advance} />}
        {step === 'password' && (
          <PasswordStep
            onDone={() => {
              // The first moment a config write is possible. Fire-and-forget:
              // the language is already applied in the browser, so a refused
              // patch costs the *persistence* of the choice, not the choice —
              // and blocking the wizard on it would strand a claimed install
              // behind a settings write.
              if (pendingLocale !== undefined) {
                save({ ui: { locale: pendingLocale } });
              }
              advance();
            }}
          />
        )}
        {step === 'provider' && (
          <ProviderStep
            onDone={(id) => {
              setInstanceId(id);
              advance();
            }}
          />
        )}
        {step === 'model' && (
          // The already-resolved endpoint when the provider step was skipped,
          // which is how a claimed install with a provider and no model opens.
          // Without it this step would filter the catalogue by an empty id and
          // then save an empty provider, which the schema refuses.
          <ModelStep
            instanceId={
              instanceId === '' ? (status.data?.provider ?? '') : instanceId
            }
            onDone={advance}
          />
        )}

        <div className="row setup-card__actions">
          {back !== null && (
            <Button
              variant="ghost"
              onClick={() => {
                setStep(back);
              }}
            >
              {t('setup.back')}
            </Button>
          )}
          <span className="spacer" />
          {isSkippable(step) && (
            <Button variant="ghost" onClick={advance}>
              {t('setup.skip')}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * The first question, and the only one answered before there is a session.
 *
 * Applied immediately as a browser preference so the rest of the wizard renders
 * in it — that is the entire reason this step is first. It is *not* written to
 * `config.ui.locale` here: that needs auth, and nobody has a password yet. The
 * overlay holds the choice and PATCHes it once the password step has
 * authenticated.
 */
function LanguageStep({
  value,
  onChange,
  onDone,
}: {
  readonly value: string;
  readonly onChange: (locale: string) => void;
  readonly onDone: () => void;
}): JSX.Element {
  const { t } = useTranslation();

  return (
    <form
      className="stack setup-form"
      onSubmit={(event) => {
        event.preventDefault();
        onDone();
      }}
    >
      <SelectField
        label={t('setup.language')}
        value={value}
        options={SUPPORTED_LOCALES.map((tag) => ({
          value: tag,
          label: nameOf(tag),
        }))}
        onValueChange={onChange}
      />
      <Button type="submit">{t('setup.continue')}</Button>
    </form>
  );
}

/** A language named in its own language — see the note in `appearance-panel.tsx`. */
function nameOf(locale: string): string {
  try {
    return (
      new Intl.DisplayNames([locale], { type: 'language' }).of(locale) ?? locale
    );
  } catch {
    return locale;
  }
}

function CodeStep({ onDone }: { readonly onDone: () => void }): JSX.Element {
  const { t } = useTranslation();
  const [code, setCode] = useState('');
  const claim = useMutation({
    mutationFn: (value: string) => api.claimSetup(value),
    onSuccess: onDone,
  });

  const submit = (event: SyntheticEvent): void => {
    event.preventDefault();
    claim.mutate(code);
  };

  return (
    <form onSubmit={submit} className="stack setup-card__body">
      <Field
        label={t('setup.setupCode')}
        name="code"
        // The one place an autofocus is right: the overlay covers everything,
        // and this field is the only thing to interact with.
        autoFocus
        autoComplete="one-time-code"
        spellCheck={false}
        className="setup-card__code"
        placeholder={t('setup.codePlaceholder')}
        value={code}
        onChange={(event) => {
          setCode(event.target.value);
        }}
        error={messageOf(claim.error, 'Incorrect or already-used setup code.')}
        hint={t('setup.codeHint')}
      />
      <Button
        type="submit"
        variant="primary"
        disabled={claim.isPending || code.trim() === ''}
      >
        {claim.isPending ? 'Checking…' : 'Continue'}
      </Button>
    </form>
  );
}

function PasswordStep({
  onDone,
}: {
  readonly onDone: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  // Prefilled and editable rather than asked for. Naming the account is not a
  // decision a first run should have to stop for, and an empty field here would
  // make it one — but the field is present so that an operator who wants a name
  // other than the default never has to find out where it is changed later.
  const [username, setUsername] = useState(DEFAULT_USERNAME);
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const set = useMutation({
    mutationFn: (value: { username: string; password: string }) =>
      api.setSetupPassword(value),
    onSuccess: onDone,
  });

  // Checked on submit rather than on every keystroke: a mismatch while the
  // second field is half-typed is not a mistake yet.
  const [mismatch, setMismatch] = useState(false);

  const submit = (event: SyntheticEvent): void => {
    event.preventDefault();
    if (password !== confirm) {
      setMismatch(true);
      return;
    }
    setMismatch(false);
    set.mutate({ username: username.trim(), password });
  };

  return (
    <form onSubmit={submit} className="stack setup-card__body">
      <Field
        label={t('setup.username')}
        name="username"
        autoComplete="username"
        spellCheck={false}
        autoFocus
        value={username}
        onChange={(event) => {
          setUsername(event.target.value);
        }}
        hint={t('setup.usernameHint')}
      />
      <Field
        label={t('setup.password')}
        type="password"
        name="new-password"
        autoComplete="new-password"
        value={password}
        onChange={(event) => {
          setPassword(event.target.value);
        }}
        hint={`At least ${String(PASSWORD_MIN_LENGTH)} characters. Behind it is an agent that can read files and run commands on this machine.`}
      />
      <Field
        label={t('setup.confirmPassword')}
        type="password"
        name="confirm-password"
        autoComplete="new-password"
        value={confirm}
        onChange={(event) => {
          setConfirm(event.target.value);
        }}
        error={
          mismatch
            ? 'The two passwords do not match.'
            : messageOf(set.error, 'Could not set the password.')
        }
      />
      <Button
        type="submit"
        variant="primary"
        // The length is checked here as well as on the server, so the wizard
        // answers instantly instead of round-tripping to be told the rule.
        disabled={
          set.isPending ||
          password.length < PASSWORD_MIN_LENGTH ||
          username.trim() === ''
        }
      >
        {set.isPending ? 'Saving…' : 'Continue'}
      </Button>
    </form>
  );
}

function ProviderStep({
  onDone,
}: {
  readonly onDone: (instanceId: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [type, setType] = useState('');
  const [apiBase, setApiBase] = useState('');
  const [key, setKey] = useState('');

  const providers = useQuery({
    queryKey: queryKeys.providers,
    queryFn: ({ signal }) => api.providers(signal),
  });

  const add = useMutation({
    mutationFn: async (chosen: ProviderInfo) => {
      // The bare type as the instance id: this is the first provider on a fresh
      // install, so nothing is taken. Adding a second of the same type from the
      // settings panel is where the `-2` suffix comes in.
      const id = chosen.id;
      // Through the same builder the settings dialog uses, on the same form
      // shape: the wizard asks for a subset of the fields, not for a different
      // patch. An empty `apiBase` is read as "unset" and falls back to the
      // registry's default.
      await api.patchSettings(
        toCreateProviderPatch(id, {
          ...EMPTY_PROVIDER_FORM,
          type: chosen.id,
          apiBase,
        }),
      );
      // After the instance exists, because the vault is keyed by instance id.
      const trimmedKey = key.trim();
      if (trimmedKey !== '') {
        await api.setCredential({
          namespace: 'providers',
          key: id,
          value: trimmedKey,
        });
      }
      return id;
    },
    onSuccess: onDone,
  });

  const chosen = providers.data?.types.find(
    (candidate) => candidate.id === type,
  );

  return (
    <div className="stack setup-card__body">
      <SelectField
        label={t('setup.provider')}
        value={type}
        placeholder={t('setup.chooseOne')}
        options={(providers.data?.types ?? []).map((candidate) => ({
          value: candidate.id,
          label: candidate.displayName,
        }))}
        onValueChange={setType}
        hint={
          chosen === undefined
            ? 'Anything OpenAI-compatible works. Pick Custom if yours is not listed.'
            : chosen.isLocal
              ? 'A server on this machine or your network. Usually needs no key.'
              : 'A cloud provider. Needs an API key.'
        }
      />

      {chosen !== undefined && (
        <>
          <TextField
            label={t('setup.apiBase')}
            value={apiBase}
            placeholder={chosen.defaultApiBase ?? 'Required for this provider'}
            spellCheck={false}
            onValueChange={setApiBase}
            hint={t('setup.apiBaseHint')}
          />
          <TextField
            label={chosen.isLocal ? 'API token (optional)' : 'API key'}
            type="password"
            autoComplete="new-password"
            spellCheck={false}
            value={key}
            onValueChange={setKey}
            hint={
              chosen.isLocal
                ? 'Only if your server sits behind an authenticating proxy.'
                : 'Written to the encrypted vault. It is never sent back to the browser.'
            }
          />
        </>
      )}

      <Button
        variant="primary"
        disabled={chosen === undefined || add.isPending}
        onClick={() => {
          if (chosen !== undefined) add.mutate(chosen);
        }}
      >
        {add.isPending ? 'Adding…' : 'Continue'}
      </Button>
      {add.error !== null && (
        <p role="alert" className="field__message field__message--error">
          {messageOf(add.error, 'Could not add the provider.')}
        </p>
      )}
    </div>
  );
}

function ModelStep({
  instanceId,
  onDone,
}: {
  readonly instanceId: string;
  readonly onDone: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [model, setModel] = useState('');

  // The refresh route rather than the cached GET: the provider was added
  // seconds ago, so anything cached predates it.
  const models = useQuery({
    queryKey: [...queryKeys.models, instanceId],
    queryFn: () => api.refreshModels(),
  });

  const queryClient = useQueryClient();
  const save = useMutation({
    mutationFn: async (chosen: string) => {
      // Onto the default agent, which is the one an install runs as before
      // anyone has made another. The wizard configures *an agent*, because
      // there is no settings layer above one.
      //
      // Through `agentSettingsPatch` rather than a literal, for the two reasons
      // that function exists. `agents.list.*` replaces wholesale, so a patch
      // naming the model alone would delete the prompt and tools of an agent
      // that already had them — which is every install reaching this step with
      // a provider already configured. And the endpoint comes from the *model*:
      // `instanceId` is empty whenever the provider step was skipped, and the
      // schema refuses an empty provider.
      const settings = await queryClient.fetchQuery({
        queryKey: queryKeys.settings,
        queryFn: ({ signal }) => api.settings(signal),
      });
      // The catalogue knows which endpoint offers the model; `instanceId` is the
      // fallback for a model typed by hand, which is what a provider that lists
      // none leaves the operator doing.
      const offeredBy = (models.data?.models ?? []).find(
        (entry) => entry.id === chosen,
      )?.providerId;
      await api.patchSettings(
        agentSettingsPatch(settings.config, DEFAULT_AGENT_ID, {
          model: chosen,
          provider: offeredBy ?? instanceId,
        }),
      );
    },
    onSuccess: onDone,
  });

  const offered = (models.data?.models ?? [])
    .filter((entry) => instanceId === '' || entry.providerId === instanceId)
    .map((entry) => entry.id)
    .sort((a, b) => a.localeCompare(b));

  const failure = models.data?.errors[instanceId];

  return (
    <div className="stack setup-card__body">
      {models.isPending ? (
        <p className="setup-card__note">{t('setup.asking')}</p>
      ) : offered.length > 0 ? (
        <SelectField
          label={t('setup.model')}
          value={model}
          placeholder={t('setup.chooseOne')}
          options={offered.map((id) => ({ value: id, label: id }))}
          onValueChange={setModel}
        />
      ) : (
        <TextField
          label={t('setup.model')}
          value={model}
          spellCheck={false}
          placeholder={t('setup.modelPlaceholder')}
          onValueChange={setModel}
          hint={
            failure === undefined
              ? 'This provider does not list its models. Type the id you want to use.'
              : `Could not reach the provider: ${failure}. Type a model id, or skip and try again from Settings.`
          }
        />
      )}

      <Button
        variant="primary"
        disabled={model.trim() === '' || save.isPending}
        onClick={() => {
          save.mutate(model.trim());
        }}
      >
        {save.isPending ? 'Saving…' : 'Finish'}
      </Button>
    </div>
  );
}

/**
 * A server message where there is one, and a written fallback where there is
 * not.
 *
 * The server's own text is preferable — "auth cannot be disabled on a LAN bind"
 * is the whole answer and "could not save" is none of it — but a network
 * failure has no message worth showing, so each caller supplies the sentence
 * that fits its step.
 */
function messageOf(error: unknown, fallback: string): string | undefined {
  if (error === null || error === undefined) return undefined;
  if (!(error instanceof ApiError)) return 'Could not reach the server.';
  if (error.status === 429) {
    return 'Too many attempts. Wait a minute and try again.';
  }
  return error.status === 401 ? fallback : error.message;
}
