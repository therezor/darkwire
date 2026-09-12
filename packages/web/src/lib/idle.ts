/**
 * "Do this when the browser has nothing better to do."
 *
 * Syntax highlighting is the caller that matters. It is pure decoration on top
 * of text that is already readable, and it runs while the next delta is
 * arriving — so doing it on the main thread during a stream is trading a legible
 * code block for a stuttering one. `requestIdleCallback` is exactly the right
 * primitive and exactly the one Safari only shipped in 16.4, hence the fallback.
 *
 * The fallback is a `setTimeout`, not a microtask: the point is to yield past
 * the current frame's work, and a microtask runs before the browser paints.
 */

interface IdleHandle {
  readonly cancel: () => void;
}

/** A conservative deadline. Long enough to be useful, short enough not to jank. */
const IDLE_TIMEOUT_MS = 500;

/** The fallback delay when there is no `requestIdleCallback`. */
const FALLBACK_DELAY_MS = 32;

interface IdleWindow {
  requestIdleCallback?: (
    callback: () => void,
    options?: { timeout: number },
  ) => number;
  cancelIdleCallback?: (handle: number) => void;
}

/**
 * Runs `task` at the next idle moment. Returns a handle that cancels it.
 *
 * The `timeout` matters: without it a tab that is never idle — one streaming a
 * long answer, say — would never highlight anything at all.
 */
export function onIdle(
  task: () => void,
  view: IdleWindow = globalThis,
): IdleHandle {
  const request = view.requestIdleCallback;
  const cancel = view.cancelIdleCallback;

  if (typeof request === 'function' && typeof cancel === 'function') {
    const handle = request.call(view, task, { timeout: IDLE_TIMEOUT_MS });
    return {
      cancel: () => {
        cancel.call(view, handle);
      },
    };
  }

  const timer = setTimeout(task, FALLBACK_DELAY_MS);
  return {
    cancel: () => {
      clearTimeout(timer);
    },
  };
}
