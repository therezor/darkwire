/**
 * Switch.
 *
 * A switch is a checkbox that applies immediately, and the distinction is not
 * cosmetic: `role="switch"` is what tells a screen reader there is no Save
 * button coming. Radix supplies that plus `Space`/`Enter` and the hidden native
 * input that makes it work inside a form.
 *
 * On is `--accent` as a fill — a fill is exactly what the accent token is for —
 * and off is the strong line colour rather than a surface, so the track stays
 * visible against every one of the four surfaces.
 */

import * as SwitchPrimitive from '@radix-ui/react-switch';
import type { ComponentProps, JSX } from 'react';

import { cn } from '@/lib/cn.js';

export function Switch({
  className,
  ...props
}: ComponentProps<typeof SwitchPrimitive.Root>): JSX.Element {
  return (
    <SwitchPrimitive.Root className={cn('switch', className)} {...props}>
      <SwitchPrimitive.Thumb className="switch__thumb" />
    </SwitchPrimitive.Root>
  );
}
