/**
 * The IANA zone list, and the sentinel the picker uses for "this browser's".
 *
 * Beside the other formatting primitives rather than inside `automation/
 * job-form.ts`: a job carries no zone of its own, so the list belongs to
 * formatting rather than to the scheduler.
 */

/**
 * The value the select uses for "whatever this browser is set to".
 *
 * A sentinel rather than an empty string, because a Radix select reads `''` as
 * "nothing chosen" and renders a blank trigger, which looks broken. It never
 * reaches the config: `ui.timezone` always stores a concrete IANA name, and the
 * panel resolves this to one on save — the same thing the language select does
 * with its own `system`, and for the same reason. Storing the rule instead would
 * mean the server resolved it to the host zone while a browser resolved it to
 * the reader's, which is precisely the disagreement one setting exists to end.
 */
export const SYSTEM_TZ = '__system__';

/**
 * Every zone this runtime knows, or a usable subset when it does not say.
 *
 * `Intl.supportedValuesOf` is the whole IANA list and needs no data of our own,
 * which matters: a bundled timezone table would be a copy of something the
 * platform already has and would go stale on its own schedule. The fallback
 * covers a runtime without it — the list is short and deliberately not a guess
 * at what the operator wants, since UTC is always first and always correct.
 */
function timezoneNames(): readonly string[] {
  try {
    const supported = Intl.supportedValuesOf('timeZone');
    return supported.length > 0 ? supported : FALLBACK_ZONES;
  } catch {
    return FALLBACK_ZONES;
  }
}

const FALLBACK_ZONES: readonly string[] = [
  'UTC',
  'Europe/London',
  'Europe/Berlin',
  'Europe/Kyiv',
  'America/New_York',
  'America/Chicago',
  'America/Los_Angeles',
  'Asia/Tokyo',
  'Asia/Shanghai',
  'Asia/Kolkata',
  'Australia/Sydney',
];

/**
 * The zones a select offers, with UTC pinned to the top.
 *
 * UTC is the schema default for a reason — a server's own zone moves when the
 * server does — so it is the one entry that should not have to be scrolled to.
 */
export function timezoneOptions(): readonly string[] {
  const rest = timezoneNames().filter((zone) => zone !== 'UTC');
  return ['UTC', ...rest];
}
