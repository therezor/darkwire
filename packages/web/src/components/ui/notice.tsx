/**
 * A short advisory block: an icon, a bold title and a sentence.
 *
 * The shape the `.notice` rule in `components/notice.css` has always described,
 * as a component. That stylesheet moved out of the chat screen once four
 * screens were using it; this is the same move for the React half, which stayed
 * behind in `chat/notice.tsx` keyed to the wire's `NoticeKind`. An agent
 * editor warning is the same block of furniture and is not a transcript event,
 * so it needs the furniture without the vocabulary.
 *
 * Not a `Badge`: a notice carries a sentence, and a pill that wraps to three
 * lines is not a pill. The tone vocabulary is shared, the shape is not.
 */

import type { ComponentType, JSX, ReactNode } from 'react';

import type { BadgeProps } from '@/components/ui/badge.js';
import { cn } from '@/lib/cn.js';

type NoticeTone = NonNullable<BadgeProps['tone']>;

/** Neutral is the base rule in `notice.css`, so it needs no modifier. */
const TONE_CLASSES: Readonly<Record<NoticeTone, string>> = {
  danger: 'notice--danger',
  warning: 'notice--warning',
  info: 'notice--info',
  success: 'notice--success',
  accent: 'notice--accent',
  neutral: '',
};

interface NoticeBlockProps {
  /** The bold half. A phrase, not a sentence — it is followed by a full stop. */
  readonly title: string;
  readonly message: ReactNode;
  readonly tone?: NoticeTone;
  /**
   * A `lucide-react` icon. Sized and aligned by the stylesheet.
   *
   * `ComponentType` rather than a function type: lucide's icons are
   * `forwardRef` components, whose call signature returns `ReactNode` and is
   * not assignable to one returning `Element`.
   */
  readonly icon: ComponentType<{ readonly 'aria-hidden'?: boolean }>;
  /**
   * Announced, when the notice appears in response to something the operator
   * just did. Left off for one that is simply part of the page — a live region
   * that was there on load announces nothing and only costs a reader.
   */
  readonly role?: 'alert' | 'status';
  readonly className?: string;
}

export function NoticeBlock({
  title,
  message,
  tone = 'neutral',
  icon: Icon,
  role,
  className,
}: NoticeBlockProps): JSX.Element {
  return (
    <div
      className={cn('notice', TONE_CLASSES[tone], className)}
      {...(role === undefined ? {} : { role })}
    >
      <Icon aria-hidden={true} />
      <span>
        <span className="notice__label">{title}.</span>{' '}
        <span className="notice__message">{message}</span>
      </span>
    </div>
  );
}
