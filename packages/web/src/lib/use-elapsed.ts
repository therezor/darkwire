import { useEffect, useState } from 'react';

/**
 * Elapsed milliseconds since mount, ticking while `active`.
 *
 * `floorMs` is the server's last figure, and it wins whenever it is ahead: a
 * tab that was backgrounded had its timers throttled, and a tab that reloaded
 * mounted after the clock started.
 */
export function useElapsed(active: boolean, floorMs: number): number {
  const [startedAtMs] = useState(() => Date.now());
  const [nowMs, setNowMs] = useState(startedAtMs);

  useEffect(() => {
    if (!active) return undefined;
    const timer = setInterval(() => {
      setNowMs(Date.now());
    }, 1000);
    return () => {
      clearInterval(timer);
    };
  }, [active]);

  return Math.max(nowMs - startedAtMs, floorMs);
}
