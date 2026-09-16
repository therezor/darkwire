/**
 * Wall-clock time in a named zone, and back to an instant.
 *
 * This is the *input* half of the timezone story; `format.ts` is the output
 * half. A `datetime-local` field hands back `2026-08-01T14:30` — a wall-clock
 * reading with no zone attached — and something has to decide which clock that
 * was. Left to `Date.parse`, the answer is the browser's zone, silently, which
 * is exactly the implicitness the install-wide `ui.timezone` exists to remove:
 * the field would then mean one thing while the row it renders back means
 * another.
 *
 * **This duplicates `instant_of_local` in `darkwire-core`'s `cron.rs`, on
 * purpose.** The original is Rust and the browser cannot call it, however much
 * it would like to. The two are kept honest by having the same DST cases in
 * both test suites rather than by sharing code across a boundary that does not
 * permit it.
 *
 * The DST rules are the same, and they are the reason this is not four lines:
 *
 * - A wall-clock time the zone **skipped** (spring forward) returns `null`. It
 *   is not an error and not a near-miss to round into the next hour — it is a
 *   time that did not happen, and a caller shows the operator that rather than
 *   booking something an hour from where they pointed.
 * - A wall-clock time that happened **twice** (fall back) resolves to the
 *   earlier instant, so a job written for it runs once.
 */

/** One formatter per zone. Construction is the expensive half; the parse is not. */
const PART_FORMATTERS = new Map<string, Intl.DateTimeFormat>();

function formatterFor(timeZone: string): Intl.DateTimeFormat {
  const cached = PART_FORMATTERS.get(timeZone);
  if (cached !== undefined) return cached;
  const created = new Intl.DateTimeFormat('en-US', {
    timeZone,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    // `h23` rather than `hour12: false`, which is specified to produce hour 24
    // for midnight in some locales — a value none of the arithmetic here expects.
    hourCycle: 'h23',
  });
  PART_FORMATTERS.set(timeZone, created);
  return created;
}

interface ZonedParts {
  readonly year: number;
  readonly month: number;
  readonly day: number;
  readonly hour: number;
  readonly minute: number;
  readonly second: number;
}

/** Whether `Intl` knows this zone. Cheap enough to call before trusting a config value. */
export function isValidTimeZone(timeZone: string): boolean {
  try {
    new Intl.DateTimeFormat('en-US', { timeZone });
    return true;
  } catch {
    return false;
  }
}

function partsOf(instantMs: number, timeZone: string): ZonedParts {
  const parts = formatterFor(timeZone).formatToParts(new Date(instantMs));
  let year = 0;
  let month = 1;
  let day = 1;
  let hour = 0;
  let minute = 0;
  let second = 0;
  for (const part of parts) {
    const value = Number.parseInt(part.value, 10);
    if (Number.isNaN(value)) continue;
    if (part.type === 'year') year = value;
    else if (part.type === 'month') month = value;
    else if (part.type === 'day') day = value;
    else if (part.type === 'hour') hour = value;
    else if (part.type === 'minute') minute = value;
    else if (part.type === 'second') second = value;
  }
  return { year, month, day, hour, minute, second };
}

/** Milliseconds to add to UTC to reach the wall clock in this zone at this instant. */
function offsetAt(instantMs: number, timeZone: string): number {
  const p = partsOf(instantMs, timeZone);
  const asIfUtc = Date.UTC(
    p.year,
    p.month - 1,
    p.day,
    p.hour,
    p.minute,
    p.second,
  );
  return asIfUtc - Math.floor(instantMs / 1000) * 1000;
}

/**
 * A day either side, which is what makes "the earlier instant" reachable.
 *
 * Probing only around the target cannot find both sides of a fall-back: every
 * probe there already reads the *post*-transition offset, so the pre-transition
 * candidate is never generated and the later instant wins by default. No
 * transition is more than a day wide, so the offsets a day before and a day
 * after bracket any of them.
 */
const DAY_MS = 86_400_000;

function pad(value: number): string {
  return String(value).padStart(2, '0');
}

/**
 * An instant as the `YYYY-MM-DDTHH:mm` a `datetime-local` input wants, read in
 * `timeZone` rather than the browser's.
 *
 * Returns `''` for a non-finite instant, which is what an empty input reads as —
 * a field that cannot show the value should be blank rather than `NaN-NaN-NaN`.
 */
export function zonedInputValue(atMs: number, timeZone: string): string {
  if (!Number.isFinite(atMs)) return '';
  const p = partsOf(atMs, timeZone);
  return `${String(p.year).padStart(4, '0')}-${pad(p.month)}-${pad(p.day)}T${pad(p.hour)}:${pad(p.minute)}`;
}

/**
 * A `datetime-local` value read as a wall clock in `timeZone`, as an instant.
 *
 * `null` means one of two things a caller must tell apart from success and need
 * not tell apart from each other: the text was not a `YYYY-MM-DDTHH:mm` at all,
 * or it named a wall-clock time the zone skipped. Both are "this is not a real
 * moment", and both deserve the same field error.
 *
 * The offset has to be discovered rather than assumed, and discovering it needs
 * an instant, which is what we are solving for. So the offset is sampled at
 * three points — a day before, at the naive guess, and a day after — and each
 * sample yields a candidate instant. Away from a transition all three agree and
 * the loop below runs once for nothing; across one they disagree, which is
 * exactly when more than one candidate is worth testing.
 */
export function instantFromZonedInput(
  value: string,
  timeZone: string,
): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/u.exec(value.trim());
  if (match === null) return null;

  const [
    ,
    rawYear = '',
    rawMonth = '',
    rawDay = '',
    rawHour = '',
    rawMinute = '',
  ] = match;
  const year = Number.parseInt(rawYear, 10);
  const month = Number.parseInt(rawMonth, 10);
  const day = Number.parseInt(rawDay, 10);
  const hour = Number.parseInt(rawHour, 10);
  const minute = Number.parseInt(rawMinute, 10);

  // Cheap range rejection before any `Intl` work. `2026-13-40T99:99` parses as
  // five integers and would otherwise round-trip through `Date.UTC`'s overflow
  // into a real instant in a month the operator did not type.
  if (
    month < 1 ||
    month > 12 ||
    day < 1 ||
    day > 31 ||
    hour > 23 ||
    minute > 59
  ) {
    return null;
  }

  const asIfUtc = Date.UTC(year, month - 1, day, hour, minute, 0);
  const candidates = new Set<number>();
  for (const probe of [asIfUtc - DAY_MS, asIfUtc, asIfUtc + DAY_MS]) {
    candidates.add(asIfUtc - offsetAt(probe, timeZone));
  }

  let best: number | null = null;
  for (const candidate of candidates) {
    const p = partsOf(candidate, timeZone);
    const roundTrips =
      p.year === year &&
      p.month === month &&
      p.day === day &&
      p.hour === hour &&
      p.minute === minute;
    if (!roundTrips) continue;
    // The earlier of two instants that both read as this wall clock, so an
    // ambiguous fall-back time is one moment rather than two.
    if (best === null || candidate < best) best = candidate;
  }
  return best;
}
