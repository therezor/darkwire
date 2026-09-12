/**
 * A textarea that grows with its content, without measuring anything.
 *
 * The trick is a one-cell grid holding a mirror `div` and the textarea on top
 * of each other: the mirror carries the same text and the same type, so *it*
 * sizes the cell and the textarea simply fills it. No `scrollHeight` read, no
 * pixel height written per keystroke, and it reflows correctly at 200% zoom for
 * free — which a measured height, being a number in device pixels, does not.
 *
 * The trailing newline in the mirror is load-bearing: without it the last line
 * is clipped at the moment it is typed, because a string ending in `\n` and one
 * that does not occupy different heights.
 *
 * The composer has its own copy of these class names because it is a much
 * larger control — it carries an upload row and a meta row besides the input.
 * This is the widget on its own, for the message editor.
 */

import type { ComponentProps, JSX } from 'react';

import { cn } from '@/lib/cn.js';

type AutoGrowTextareaProps = Omit<ComponentProps<'textarea'>, 'rows'> & {
  readonly value: string;
};

export function AutoGrowTextarea({
  value,
  className,
  ...props
}: AutoGrowTextareaProps): JSX.Element {
  return (
    <div className="auto-grow">
      <div aria-hidden="true" className="auto-grow__mirror">
        {`${value}\n`}
      </div>
      <textarea
        {...props}
        value={value}
        rows={1}
        className={cn('auto-grow__input', className)}
      />
    </div>
  );
}
