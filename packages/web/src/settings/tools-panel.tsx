/**
 * Settings → Tools: how this install reaches the web.
 *
 * Install-wide on purpose, unlike `exec` and the result budget, which are on
 * the agent. What is here describes the shape of an outbound connection and the
 * machine making it: which backend answers a search, how this install
 * identifies itself, and what every request is bounded by. None of it is
 * something one agent should be able to answer differently from another.
 *
 * **What an agent may reach is not here.** That is its allow-list, on the agent,
 * because it is a statement about that agent rather than about this machine.
 *
 * The MCP servers have their own panel. They are a list an operator keeps, the
 * same split Agents already makes, and folding them in here would put a
 * collection and five knobs on one screen.
 */

import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { Config } from '@darkwire/protocol';

import {
  FieldGrid,
  SaveBar,
  Section,
  SelectField,
  TextField,
  TextareaField,
} from '@/components/form/controls.js';
import { parseNumber, type PatchResult } from '@/components/form/fields.js';

import { useSaveSettings } from './use-settings.js';

export function ToolsPanel({
  config,
  defaultUserAgent,
}: {
  readonly config: Config;
  /**
   * What an empty user agent sends, served by the API rather than written here.
   * A second copy in the browser would go stale the moment the Chrome major
   * moves, showing an operator a string this build does not send.
   */
  readonly defaultUserAgent: string;
}): JSX.Element {
  const { t } = useTranslation();
  const { save, saving } = useSaveSettings();
  const web = config.tools.web;

  const [provider, setProvider] = useState<string>(web.searchProvider);
  const [searchUrl, setSearchUrl] = useState(web.searchUrl);
  const [userAgent, setUserAgent] = useState(web.userAgent);
  const [timeout, setTimeout] = useState(String(web.timeoutSeconds));
  const [readTimeout, setReadTimeout] = useState(
    String(web.readTimeoutSeconds),
  );
  const [maxBytes, setMaxBytes] = useState(String(web.maxBytes));
  const [cacheEntries, setCacheEntries] = useState(String(web.cacheEntries));
  const [cacheTtl, setCacheTtl] = useState(String(web.cacheTtlSeconds));
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const [dirty, setDirty] = useState(false);

  const touched = <T,>(set: (value: T) => void) => {
    return (value: T) => {
      set(value);
      setDirty(true);
    };
  };

  const build = (): PatchResult => {
    const numbers = {
      timeoutSeconds: parseNumber(timeout, t, { min: 1, integer: true }),
      readTimeoutSeconds: parseNumber(readTimeout, t, {
        min: 1,
        integer: true,
      }),
      maxBytes: parseNumber(maxBytes, t, { min: 1, integer: true }),
      cacheEntries: parseNumber(cacheEntries, t, { min: 0, integer: true }),
      cacheTtlSeconds: parseNumber(cacheTtl, t, { min: 0, integer: true }),
    };
    const next: Record<string, string> = {};
    for (const [key, parsed] of Object.entries(numbers)) {
      if (!parsed.ok) next[key] = parsed.error;
    }
    // A SearXNG instance with no URL reaches nothing, and the tool would say so
    // on the first search rather than here, where it can still be fixed.
    if (provider === 'searxng' && searchUrl.trim() === '') {
      next.searchUrl = t('settings.tools.searchUrlRequired');
    }
    if (Object.keys(next).length > 0) {
      return { ok: false, errors: next };
    }

    return {
      ok: true,
      patch: {
        tools: {
          web: {
            searchProvider: provider === 'searxng' ? 'searxng' : 'auto',
            // Cleared when the provider does not use it, so a stale URL cannot
            // come back into effect by switching the provider back.
            searchUrl: provider === 'searxng' ? searchUrl.trim() : '',
            userAgent: userAgent.trim(),
            timeoutSeconds: numbers.timeoutSeconds.ok
              ? numbers.timeoutSeconds.value
              : 0,
            readTimeoutSeconds: numbers.readTimeoutSeconds.ok
              ? numbers.readTimeoutSeconds.value
              : 0,
            maxBytes: numbers.maxBytes.ok ? numbers.maxBytes.value : 0,
            cacheEntries: numbers.cacheEntries.ok
              ? numbers.cacheEntries.value
              : 0,
            cacheTtlSeconds: numbers.cacheTtlSeconds.ok
              ? numbers.cacheTtlSeconds.value
              : 0,
          },
        },
      },
    };
  };

  return (
    <Section
      title={t('settings.tools.webTitle')}
      description={t('settings.tools.webDesc')}
    >
      <SelectField
        label={t('settings.tools.searchProvider')}
        hint={t('settings.tools.searchProviderHint')}
        value={provider}
        options={[
          { value: 'auto', label: t('settings.tools.providerAuto') },
          { value: 'searxng', label: t('settings.tools.providerSearxng') },
        ]}
        onValueChange={touched(setProvider)}
      />
      {provider === 'searxng' && (
        <TextField
          label={t('settings.tools.searchUrl')}
          hint={t('settings.tools.searchUrlHint')}
          value={searchUrl}
          error={errors.searchUrl}
          onValueChange={touched(setSearchUrl)}
        />
      )}
      <TextareaField
        label={t('settings.tools.userAgent')}
        hint={t('settings.tools.userAgentHint')}
        value={userAgent}
        placeholder={defaultUserAgent}
        rows={2}
        onValueChange={touched(setUserAgent)}
      />
      <FieldGrid>
        <TextField
          label={t('settings.tools.timeout')}
          hint={t('settings.tools.timeoutHint')}
          value={timeout}
          error={errors.timeoutSeconds}
          inputMode="numeric"
          onValueChange={touched(setTimeout)}
        />
        <TextField
          label={t('settings.tools.readTimeout')}
          hint={t('settings.tools.readTimeoutHint')}
          value={readTimeout}
          error={errors.readTimeoutSeconds}
          inputMode="numeric"
          onValueChange={touched(setReadTimeout)}
        />
        <TextField
          label={t('settings.tools.maxBytes')}
          hint={t('settings.tools.maxBytesHint')}
          value={maxBytes}
          error={errors.maxBytes}
          inputMode="numeric"
          onValueChange={touched(setMaxBytes)}
        />
        <TextField
          label={t('settings.tools.cacheEntries')}
          hint={t('settings.tools.cacheEntriesHint')}
          value={cacheEntries}
          error={errors.cacheEntries}
          inputMode="numeric"
          onValueChange={touched(setCacheEntries)}
        />
        <TextField
          label={t('settings.tools.cacheTtl')}
          hint={t('settings.tools.cacheTtlHint')}
          value={cacheTtl}
          error={errors.cacheTtlSeconds}
          inputMode="numeric"
          onValueChange={touched(setCacheTtl)}
        />
      </FieldGrid>
      {/* An operator looking for the allow-list needs sending somewhere, not
          left to conclude it is missing. */}
      <p className="settings-field__hint">
        {t('settings.tools.egressElsewhere')}
      </p>
      <SaveBar
        dirty={dirty}
        saving={saving}
        onRevert={() => {
          setProvider(web.searchProvider);
          setSearchUrl(web.searchUrl);
          setUserAgent(web.userAgent);
          setTimeout(String(web.timeoutSeconds));
          setReadTimeout(String(web.readTimeoutSeconds));
          setMaxBytes(String(web.maxBytes));
          setCacheEntries(String(web.cacheEntries));
          setCacheTtl(String(web.cacheTtlSeconds));
          setErrors({});
          setDirty(false);
        }}
        onSave={() => {
          const result = build();
          if (!result.ok) {
            setErrors(result.errors);
            return;
          }
          setErrors({});
          save(result.patch);
          setDirty(false);
        }}
      />
    </Section>
  );
}
