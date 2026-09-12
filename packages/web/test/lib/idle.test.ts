/**
 * The idle scheduler, on both platforms.
 *
 * The fallback branch is the one worth testing: it is the branch that runs in
 * Safari before 16.4 and in jsdom, which is to say in every test that renders a
 * code block. A fallback that never fired would mean highlighting that works in
 * Chrome and silently does nothing everywhere else.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { onIdle } from '@/lib/idle.js';

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('onIdle', () => {
  it('uses requestIdleCallback with a deadline when the browser has one', () => {
    const request = vi.fn().mockReturnValue(7);
    const cancel = vi.fn();
    const task = vi.fn();

    const handle = onIdle(task, {
      requestIdleCallback: request,
      cancelIdleCallback: cancel,
    });

    // The timeout is what makes this fire at all in a tab that is never idle —
    // one streaming a long answer, for instance.
    expect(request).toHaveBeenCalledWith(task, { timeout: 500 });

    handle.cancel();
    expect(cancel).toHaveBeenCalledWith(7);
  });

  it('falls back to a timeout, which still runs', () => {
    const task = vi.fn();

    onIdle(task, {});
    expect(task).not.toHaveBeenCalled();

    vi.advanceTimersByTime(32);
    expect(task).toHaveBeenCalledOnce();
  });

  it('cancels the fallback too', () => {
    const task = vi.fn();

    onIdle(task, {}).cancel();
    vi.advanceTimersByTime(1000);

    expect(task).not.toHaveBeenCalled();
  });
});
