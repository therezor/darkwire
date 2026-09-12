/**
 * Settings.
 *
 * The panel lives in the URL as `?panel=`, which is what makes a settings screen
 * linkable — "set your key here" is a link, not a sentence describing four
 * clicks. `panelById` falls back rather than 404ing, so a bookmark to a panel
 * that was renamed lands on a settings screen instead of an empty tab.
 *
 * Everything below the tabs waits for one `GET /api/settings`. Each panel is
 * mounted with the config it initialises its form from, and mounted *only* once
 * that config exists — a form built from defaults and then re-synced is a form
 * that shows the wrong values for a frame, and on a slow connection for rather
 * more than a frame.
 *
 * `loadError` is surfaced at the top rather than swallowed. It means the file on
 * disk failed to parse and the process is running on defaults, which is exactly
 * the state where the settings screen looks fine and is describing something
 * that is not what will load next time.
 */

import { useNavigate, useSearch } from '@tanstack/react-router';
import { AlertTriangle } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs.js';
import { AppearancePanel } from '@/settings/appearance-panel.js';
import { AccountPanel } from '@/settings/account-panel.js';
import { ExtensionsPanel } from '@/settings/extensions-panel.js';
import { McpPanel } from '@/settings/mcp-panel.js';
import { ProvidersPanel } from '@/settings/providers-panel.js';
import { ToolsPanel } from '@/settings/tools-panel.js';
import { AutomationPanel } from '@/settings/automation-panel.js';
import { ChannelsPanel } from '@/settings/channels-panel.js';
import { panelById, SETTINGS_PANELS } from '@/settings/panels.js';
import { useSettings } from '@/settings/use-settings.js';

export function SettingsRoute(): JSX.Element {
  const { t } = useTranslation();
  const { panel: requested } = useSearch({ from: '/settings' });
  const navigate = useNavigate();
  const settings = useSettings();
  const panel = panelById(requested);

  return (
    <div className="stack page page--wide">
      <div className="stack page__heading">
        <h1 className="page__title">{t('settings.title')}</h1>
        <p className="page__note">{t(panel.summary)}</p>
      </div>

      {settings.data?.loadError !== undefined && (
        <p role="alert" className="settings-load-error">
          <AlertTriangle />
          <span>
            {t('settings.loadError', { error: settings.data.loadError })}
          </span>
        </p>
      )}

      {/* Separate from `loadError` above, which means the file did not parse at
          all. These parsed and could not be honoured — a delegation to an agent
          that was deleted, an entry under a key that is not a usable id — and
          each is addressed on its own, so they render as a list rather than
          being folded into one sentence. The server's message is shown as
          written: it names the agents involved, which no key here could. */}
      {(settings.data?.warnings ?? []).map((warning) => (
        <p
          key={`${warning.code}:${warning.agentId ?? ''}:${warning.message}`}
          role="alert"
          className="settings-load-error"
        >
          <AlertTriangle />
          <span>{warning.message}</span>
        </p>
      ))}

      <Tabs
        value={panel.id}
        onValueChange={(value) => {
          // `replace`, because switching panels is not a step the Back button
          // should have to walk through to leave Settings.
          void navigate({
            to: '/settings',
            search: { panel: value },
            replace: true,
          });
        }}
      >
        {/* Scrolls rather than wraps: a tab strip that reflows to two rows moves
            every tab under the pointer as the window is resized. */}
        <TabsList className="settings-tabs">
          {SETTINGS_PANELS.map((entry) => (
            <TabsTrigger key={entry.id} value={entry.id}>
              {t(entry.label)}
            </TabsTrigger>
          ))}
        </TabsList>

        <TabsContent value={panel.id}>
          <PanelBody panelId={panel.id} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

/**
 * Split out so the panel choice is one `switch`-shaped expression rather than a
 * ladder of ternaries nested inside JSX, which is where a missing branch hides.
 */
function PanelBody({ panelId }: { readonly panelId: string }): JSX.Element {
  const { t } = useTranslation();
  const settings = useSettings();
  const panel = panelById(panelId);

  // Before the settings gate, and the only panel that goes before it: a
  // credential is not in `config.json`, so this panel has nothing to wait for —
  // and an install whose settings request is failing is exactly the one whose
  // owner may be trying to fix their password.
  if (panel.id === 'account') return <AccountPanel />;

  // Beside `account`, and before the settings gate, for half a reason rather
  // than the whole one: the theme half of this panel needs nothing from the
  // server, and an install whose settings request is failing is one whose owner
  // may well want to turn the lights on while they read the error.
  if (panel.id === 'appearance') return <AppearancePanel />;

  if (settings.isPending) {
    return <p className="page__note">{t('settings.loading')}</p>;
  }
  if (settings.isError) {
    return (
      <p role="alert" className="page__error">
        {t('settings.loadFailed', { message: settings.error.message })}
      </p>
    );
  }

  const { config } = settings.data;
  if (panel.id === 'tools') return <ToolsPanel config={config} />;
  if (panel.id === 'automation') return <AutomationPanel config={config} />;
  if (panel.id === 'mcp') return <McpPanel config={config} />;
  if (panel.id === 'extensions') return <ExtensionsPanel config={config} />;
  if (panel.id === 'channels') {
    // The only panel that needs more than the config: whether the bot is
    // actually answering is not a setting, and the vault is write-only so
    // "a token is saved" cannot be read off `config` either.
    return <ChannelsPanel config={config} channels={settings.data.channels} />;
  }
  return <ProvidersPanel />;
}
