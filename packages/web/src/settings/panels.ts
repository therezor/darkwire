/**
 * What Settings is made of, as data.
 *
 * Every panel here is built. There is deliberately no placeholder mechanism: an
 * entry naming a future phase instead of a form is not a state a reader can act
 * on. A panel arrives on this list when it has something to configure.
 *
 * It is data rather than a `<Tabs>` written out by hand so the list can be
 * asserted about: `panels.test.ts` is what stops a panel reaching the tab strip
 * without a label and a summary that resolve.
 */

import type { WebKey } from '@/i18n/keys.js';

interface SettingsPanel {
  /** Also the tab value and the `?panel=` search parameter. */
  readonly id: string;
  /**
   * Resource keys rather than words, because this table is read as data — the
   * tab strip, the heading and `panels.test.ts` all index into it. A `string`
   * here would widen the literal on the way in and hand `t()` something it
   * cannot check, which is precisely the mistake this layer exists to catch.
   */
  readonly label: WebKey;
  /** One line under the heading, saying what the panel governs. */
  readonly summary: WebKey;
}

const PROVIDERS_PANEL: SettingsPanel = {
  id: 'providers',
  label: 'settings.panels.providers.label',
  summary: 'settings.panels.providers.summary',
};

export const SETTINGS_PANELS: readonly SettingsPanel[] = [
  PROVIDERS_PANEL,
  {
    id: 'tools',
    label: 'settings.panels.tools.label',
    summary: 'settings.panels.tools.summary',
  },
  {
    id: 'environments',
    label: 'settings.panels.environments.label',
    summary: 'settings.panels.environments.summary',
  },
  {
    id: 'account',
    label: 'settings.panels.account.label',
    summary: 'settings.panels.account.summary',
  },
  {
    id: 'appearance',
    label: 'settings.panels.appearance.label',
    summary: 'settings.panels.appearance.summary',
  },
  {
    id: 'automation',
    label: 'settings.panels.automation.label',
    summary: 'settings.panels.automation.summary',
  },
  {
    id: 'mcp',
    label: 'settings.panels.mcp.label',
    summary: 'settings.panels.mcp.summary',
  },
  {
    id: 'channels',
    label: 'settings.panels.channels.label',
    summary: 'settings.panels.channels.summary',
  },
  {
    id: 'extensions',
    label: 'settings.panels.extensions.label',
    summary: 'settings.panels.extensions.summary',
  },
];

export const DEFAULT_PANEL_ID: string = PROVIDERS_PANEL.id;

/** The panel a `?panel=` value names, falling back rather than 404ing. */
export function panelById(id: string | undefined): SettingsPanel {
  const found = SETTINGS_PANELS.find((panel) => panel.id === id);
  // A stale bookmark to a panel that was renamed lands on Providers, which is a
  // settings screen. The alternative is an empty tab panel, which is not.
  return found ?? PROVIDERS_PANEL;
}
