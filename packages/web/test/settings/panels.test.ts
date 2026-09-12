/**
 * The panel registry.
 *
 * The rules asserted here are the ones a tab strip built from data can get
 * wrong without anybody noticing: a duplicate id, and a label or summary whose
 * key never made it into the bundle. Both render as something that looks
 * deliberate — a tab that silently shadows another, or a heading reading
 * `settings.panels.x.summary` — rather than as an error.
 */

import { createWebI18n } from '@ghostwire/i18n/web';
import { describe, expect, it } from 'vitest';

import {
  DEFAULT_PANEL_ID,
  panelById,
  SETTINGS_PANELS,
} from '@/settings/panels.js';

/**
 * The table holds keys now, so every assertion about *wording* has to resolve
 * one. A real English instance rather than a stub: "gives every panel a
 * summary" is worth nothing if it only proves the key string is non-empty —
 * the failure it guards against is a panel whose summary was never written.
 */
const t = createWebI18n('en').getFixedT(null, 'web');

describe('the settings panels', () => {
  it('have unique ids', () => {
    const ids = SETTINGS_PANELS.map((panel) => panel.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('are the eight that are built, in the order the strip shows them', () => {
    // No `agent` panel: the settings it held *are* the default agent's, so they
    // are edited on that agent rather than in a second room describing the same
    // subtree. Agents are a page of their own, and picking one happens in the
    // composer.
    //
    // `appearance` joined them with the translation layer. It is the natural
    // home for the theme too, which until then lived only in the header — a
    // preference with no page you could point someone at.
    //
    // `automation` holds the scheduler engine and nothing else — the jobs are a
    // page in the nav, because a list an operator keeps is not a setting. That
    // is the split Agents already makes: the agents are a page, and only
    // install-wide tool settings are in Settings.
    //
    // `mcp` joined them with the MCP client, `channels` with Telegram, and
    // `extensions` with the extension host. There is no entry naming a future
    // phase: a panel is on this list once it has something to configure.
    expect(SETTINGS_PANELS.map((panel) => panel.id)).toEqual([
      'providers',
      'tools',
      'account',
      'appearance',
      'automation',
      'mcp',
      'channels',
      'extensions',
    ]);
  });

  it('gives every panel a summary, which is the line under the heading', () => {
    for (const panel of SETTINGS_PANELS) {
      // Resolved, so a key that exists in the table but not in the bundle
      // fails here rather than rendering as `settings.panels.x.summary`.
      expect(t(panel.summary).length).toBeGreaterThan(0);
      expect(t(panel.summary)).not.toBe(panel.summary);
      expect(t(panel.label).length).toBeGreaterThan(0);
      expect(t(panel.label)).not.toBe(panel.label);
    }
  });
});

describe('panelById', () => {
  it('finds a panel by its id', () => {
    expect(t(panelById('tools').label)).toBe('Tools');
  });

  it('falls back rather than 404ing on a stale bookmark', () => {
    expect(panelById('renamed-last-year').id).toBe(DEFAULT_PANEL_ID);
    expect(panelById(undefined).id).toBe(DEFAULT_PANEL_ID);
  });
});
