/**
 * A key in the browser's bundle, as a type.
 *
 * The web's answer to `CliKey`, and it exists for the tables rather than for the
 * call sites. `t('nav.agents')` is checked already — the literal is right there
 * for the compiler to see. What is *not* checked is a table that holds keys as
 * data:
 *
 * ```ts
 * const NAV: readonly NavItem[] = [{ to: '/agents', label: 'nav.agents', … }];
 * ```
 *
 * With `label: string` the literal widens on the way into the array, and by the
 * time it reaches `t(label)` the compiler knows only that it is a string — so a
 * typo'd or deleted key compiles, and shows up as the key rendered on a nav
 * item. Typing the field as `WebKey` keeps the literal narrow through the table
 * and puts the error back at the entry that is wrong.
 */

import type { ResourceKeys, WebResources } from '@ghostwire/i18n';

export type WebKey = ResourceKeys<WebResources>;
