/**
 * How a tool permission is worded on screen.
 *
 * Shared by the two rows that offer the choice — a tool's and a subagent's —
 * because a delegation carries a permission exactly as a tool does, and the
 * operator answering "allow, ask or disabled" is answering one question. Two
 * tables would be two vocabularies for one config value.
 */

import type { ToolPermission } from '@ghostwire/protocol';

import type { WebKey } from '@/i18n/keys.js';

export const PERMISSION_LABELS: Readonly<Record<ToolPermission, WebKey>> = {
  allow: 'agents.toolAllow',
  ask: 'agents.toolAsk',
  deny: 'agents.toolDisabled',
};
