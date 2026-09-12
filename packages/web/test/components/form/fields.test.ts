/**
 * The form edges.
 *
 * Each case here is a boundary that produces a *plausible* wrong value rather
 * than an obvious one — `""` becoming `0`, `1500 ms` round-tripping to `2000`,
 * a trailing newline becoming an empty model id — which is exactly the class of
 * bug a settings screen ships with and nobody notices for a month.
 */

import { describe, expect, it } from 'vitest';
import { createWebI18n } from '@ghostwire/i18n/web';

/** English, resolved: these assertions compare the message a user would read. */
const t = createWebI18n('en').getFixedT(null, 'web');

import {
  formatList,
  modelOptions,
  msToSeconds,
  parseList,
  parseNumber,
  secondsToMs,
} from '@/components/form/fields.js';

describe('parseNumber', () => {
  it('reads a number, trimming what a paste brings with it', () => {
    expect(parseNumber(' 8192 ', t)).toEqual({ ok: true, value: 8192 });
  });

  it('refuses an empty field rather than reading it as zero', () => {
    // `Number('')` is 0, and 0 is a *meaningful* value for every duration in
    // the tree — it disables the limit. A cleared field must not silently do
    // that.
    expect(parseNumber('', t)).toEqual({ ok: false, error: 'Required' });
    expect(parseNumber('   ', t)).toEqual({ ok: false, error: 'Required' });
  });

  it('refuses text and infinities', () => {
    expect(parseNumber('abc', t)).toEqual({
      ok: false,
      error: 'Must be a number',
    });
    expect(parseNumber('Infinity', t)).toEqual({
      ok: false,
      error: 'Must be a number',
    });
  });

  it('enforces the bounds it is given', () => {
    expect(parseNumber('0', t, { min: 1 })).toEqual({
      ok: false,
      error: 'Must be at least 1',
    });
    expect(parseNumber('3', t, { max: 2 })).toEqual({
      ok: false,
      error: 'Must be at most 2',
    });
    expect(parseNumber('1.5', t, { integer: true })).toEqual({
      ok: false,
      error: 'Must be a whole number',
    });
    expect(parseNumber('1.5', t, { min: 0, max: 2 })).toEqual({
      ok: true,
      value: 1.5,
    });
  });
});

describe('durations', () => {
  it('round-trips a whole number of seconds', () => {
    expect(msToSeconds(120_000)).toBe('120');
    expect(secondsToMs(120)).toBe(120_000);
  });

  it('keeps a value that is not a whole number of seconds', () => {
    // The one that matters: opening a panel on a 1500 ms timeout and saving it
    // unchanged must not move it to 2000.
    expect(msToSeconds(1500)).toBe('1.5');
    expect(secondsToMs(1.5)).toBe(1500);
  });

  it('treats zero as zero and a broken value as zero', () => {
    expect(msToSeconds(0)).toBe('0');
    expect(msToSeconds(Number.NaN)).toBe('0');
  });

  it('rounds to whole milliseconds, since that is what the config stores', () => {
    expect(secondsToMs(0.0005)).toBe(1);
  });
});

describe('lists', () => {
  it('splits on newlines and on commas', () => {
    expect(parseList('a\nb, c')).toEqual(['a', 'b', 'c']);
  });

  it('drops the blanks a textarea always ends with', () => {
    expect(parseList('a\n\n b \n')).toEqual(['a', 'b']);
    expect(parseList('   ')).toEqual([]);
  });

  it('drops duplicates, which a picker would otherwise show twice', () => {
    expect(parseList('a\nb\na')).toEqual(['a', 'b']);
  });

  it('formats back to one per line', () => {
    expect(formatList(['a', 'b'])).toBe('a\nb');
    expect(parseList(formatList(['a', 'b']))).toEqual(['a', 'b']);
  });
});

describe('modelOptions', () => {
  const models = [
    { id: 'gpt-5', providerId: 'openai' },
    { id: 'llama3', providerId: 'ollama' },
    { id: 'qwen', providerId: 'ollama' },
  ];

  it('narrows to the chosen provider', () => {
    expect(modelOptions(models, 'ollama', '')).toEqual(['llama3', 'qwen']);
  });

  it('offers everything under `auto`, which is not a provider', () => {
    expect(modelOptions(models, 'auto', '')).toEqual([
      'gpt-5',
      'llama3',
      'qwen',
    ]);
  });

  it('always includes the current model, even when nothing advertises it', () => {
    // A picker that dropped the selected value would change the setting by
    // rendering: the control would show — and then save — a different model.
    expect(modelOptions(models, 'ollama', 'hand-typed')).toEqual([
      'hand-typed',
      'llama3',
      'qwen',
    ]);
  });

  it('does not list the current model twice', () => {
    expect(modelOptions(models, 'ollama', 'qwen')).toEqual(['llama3', 'qwen']);
  });
});
