/**
 * A form's edges.
 *
 * Every panel here is a form over `Config`, and a form over a typed tree has
 * exactly three places it goes wrong: a number that arrives as a string, a
 * duration stored in one unit and shown in another, and a list edited as text.
 * All three are here, as functions, because all three have a boundary that is
 * invisible in a component and obvious in a test — `""` parsing to `0`, `1500`
 * rendering as `1.5s` and saving back as `1000`, a trailing newline turning into
 * an empty model id the provider then tries to call.
 *
 * The panels hold `string` state and convert once on save. The alternative —
 * holding numbers and coercing on every keystroke — makes a half-typed `0.` or a
 * cleared field unrepresentable, so the input fights the user as they type.
 */

import type { ConfigPatch } from '@ghostwire/protocol';
import type { TFunction } from 'i18next';

interface NumberConstraint {
  readonly min?: number;
  readonly max?: number;
  /** Rejects `1.5`, rather than rounding it behind the operator's back. */
  readonly integer?: boolean;
}

type ParseResult<T> =
  | { readonly ok: true; readonly value: T }
  | {
      readonly ok: false;
      readonly error: string;
    };

/**
 * A number typed into a text input, validated against the schema's own bounds.
 *
 * The bounds are restated at each call site rather than derived from the Zod
 * schema: `ConfigSchema` is not introspectable field-by-field without walking
 * internals, and a wrong bound here is a message shown a moment before the
 * server refuses the save anyway. This is the courtesy check, not the guard.
 */
export function parseNumber(
  raw: string,
  t: TFunction,
  constraint: NumberConstraint = {},
): ParseResult<number> {
  const trimmed = raw.trim();
  if (trimmed === '') {
    return { ok: false, error: t('settings.fields.required') };
  }

  const value = Number(trimmed);
  // `Number('')` is 0 and `Number(' ')` is 0, both already handled above;
  // what is left that `Number` accepts and this must not is `Infinity`.
  if (!Number.isFinite(value)) {
    return { ok: false, error: t('settings.fields.mustBeNumber') };
  }

  if (constraint.integer === true && !Number.isInteger(value)) {
    return { ok: false, error: t('settings.fields.mustBeWhole') };
  }
  if (constraint.min !== undefined && value < constraint.min) {
    return {
      ok: false,
      error: t('settings.fields.mustBeAtLeast', { min: constraint.min }),
    };
  }
  if (constraint.max !== undefined && value > constraint.max) {
    return {
      ok: false,
      error: t('settings.fields.mustBeAtMost', { max: constraint.max }),
    };
  }

  return { ok: true, value };
}

/**
 * Milliseconds as the seconds an operator thinks in.
 *
 * Config stores every duration in milliseconds — `toolTimeoutMs` says so in its
 * name — but nobody sets a tool timeout to 120000. The conversion is lossy in
 * one direction only: a value that is not a whole number of seconds keeps its
 * decimal here and round-trips exactly, so opening a panel and saving it
 * unchanged cannot quietly move a 1500 ms timeout to 2000.
 */
export function msToSeconds(ms: number): string {
  if (!Number.isFinite(ms)) return '0';
  const seconds = ms / 1000;
  return Number.isInteger(seconds)
    ? String(seconds)
    : String(Number(seconds.toFixed(3)));
}

export function secondsToMs(seconds: number): number {
  return Math.round(seconds * 1000);
}

/**
 * A list edited as one item per line.
 *
 * Commas are accepted as well as newlines, because a model list pasted from a
 * provider's documentation arrives comma-separated and rejecting it would be a
 * puzzle rather than a validation. Blank entries are dropped: a trailing newline
 * is how every one of these ends, and an empty string in `providers.x.models` is
 * a model id the picker would offer and the provider would refuse.
 */
export function parseList(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];

  for (const item of text.split(/[\n,]/)) {
    const trimmed = item.trim();
    if (trimmed === '' || seen.has(trimmed)) continue;
    seen.add(trimmed);
    out.push(trimmed);
  }

  return out;
}

/** The inverse, for putting a stored list back into a textarea. */
export function formatList(items: readonly string[]): string {
  return items.join('\n');
}

/**
 * A record edited as one `NAME=value` per line.
 *
 * The shape an operator already has in their hand: an MCP server's environment
 * comes out of a `.env` file or a shell line, and its headers come out of a
 * `curl` command. Both are pasted, so both accept `=` and `:` as the separator
 * and split on the *first* one — a bearer token contains neither, but a URL in
 * a header value contains a colon and would otherwise be cut in half.
 *
 * A line with no separator is dropped rather than stored under an empty key.
 * There is nothing useful to do with `FOO` on its own, and the alternative is a
 * validation error for a stray line in a paste.
 */
export function parseRecord(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const trimmed = line.trim();
    if (trimmed === '') continue;
    const at = trimmed.search(/[=:]/);
    if (at <= 0) continue;
    const key = trimmed.slice(0, at).trim();
    if (key === '') continue;
    out[key] = trimmed.slice(at + 1).trim();
  }
  return out;
}

/** The inverse. Always `=`, because that is what both halves accept back. */
export function formatRecord(record: Readonly<Record<string, string>>): string {
  return Object.entries(record)
    .map(([key, value]) => `${key}=${value}`)
    .join('\n');
}

interface ModelChoice {
  readonly id: string;
  readonly providerId: string;
}

/**
 * The models to offer for a chosen provider.
 *
 * Two rules, both about not losing the current selection:
 *
 *  - **`auto` offers everything.** It is not a provider, it is "resolve one",
 *    so narrowing the list would leave a fresh install with no models at all.
 *  - **The current model is always in the list**, even when no provider
 *    advertises it. A model typed into `config.json` by hand, or one a provider
 *    stopped listing, is still the model this agent is running — and a picker
 *    that silently drops the selected value changes the setting by rendering.
 */
export function modelOptions(
  models: readonly ModelChoice[],
  providerId: string,
  current: string,
): string[] {
  const matching = models
    .filter((model) => providerId === 'auto' || model.providerId === providerId)
    .map((model) => model.id);

  const unique = [
    ...new Set(current === '' ? matching : [current, ...matching]),
  ];
  return unique.sort((a, b) => a.localeCompare(b));
}

/**
 * What every form on this surface returns: a patch, or the reasons there isn't
 * one.
 *
 * Here rather than in one of the forms because two of them produce it and
 * neither owns it. It lived on the agent form and the tools form imported it
 * across, which made deleting the agent form a compile error in a module that
 * has nothing to do with agents.
 */
export type PatchResult =
  | { readonly ok: true; readonly patch: ConfigPatch }
  | { readonly ok: false; readonly errors: Readonly<Record<string, string>> };
