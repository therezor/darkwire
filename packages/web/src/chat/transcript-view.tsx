/**
 * The scrolling transcript.
 *
 * The behaviour worth describing is the scroll pin. A chat that always scrolls
 * to the bottom is unusable while an answer streams — the moment the reader
 * scrolls up to re-read something, the next delta yanks them back down. A chat
 * that never scrolls is worse. So the container tracks whether the reader is
 * *at* the bottom, and only follows while they are: scroll up and it stops
 * following and offers a way back; scroll down to the end and it resumes.
 *
 * The threshold is a few lines rather than zero because a bottom-anchored
 * container is never exactly at the bottom — sub-pixel rounding, a scrollbar
 * that appeared, an image that finished loading — and an exact comparison
 * unpins on its own within a second of any of those.
 */

import { ArrowDown } from 'lucide-react';
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type JSX,
} from 'react';
import { useTranslation } from 'react-i18next';

import type { ApprovalScope } from '@ghostwire/protocol';

import { Button } from '@/components/ui/button.js';
import type { Transcript } from '@/state/transcript.js';
import { Message, type MessageAction } from './message.js';

/** How close to the bottom still counts as "at the bottom", in CSS pixels. */
const PIN_THRESHOLD = 48;

interface TranscriptViewProps {
  readonly transcript: Transcript;
  readonly busy: boolean;
  /** Which conversation this is, for the turn-details lookup. */
  readonly sessionKey: string | undefined;
  readonly onApprove: (
    callId: string,
    approved: boolean,
    scope: ApprovalScope,
  ) => void;
  readonly onAction: (action: MessageAction) => void;
}

export function TranscriptView({
  transcript,
  busy,
  sessionKey,
  onApprove,
  onAction,
}: TranscriptViewProps): JSX.Element {
  const { t } = useTranslation();
  const viewportRef = useRef<HTMLDivElement>(null);
  const [pinned, setPinned] = useState(true);

  const scrollToBottom = useCallback((behavior: ScrollBehavior = 'auto') => {
    const viewport = viewportRef.current;
    if (viewport === null) return;
    viewport.scrollTo({ top: viewport.scrollHeight, behavior });
  }, []);

  // `useLayoutEffect`, not `useEffect`: the scroll has to happen in the same
  // frame the new content was laid out in, or the reader sees the transcript
  // jump after it has already painted a line lower down.
  useLayoutEffect(() => {
    if (pinned) scrollToBottom();
  }, [transcript, busy, pinned, scrollToBottom]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (viewport === null) return undefined;

    const onScroll = (): void => {
      const distance =
        viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight;
      setPinned(distance <= PIN_THRESHOLD);
    };

    viewport.addEventListener('scroll', onScroll, { passive: true });
    return () => {
      viewport.removeEventListener('scroll', onScroll);
    };
  }, []);

  const lastTurn = transcript.findLast((item) => item.kind === 'turn');

  return (
    <div className="transcript">
      <div
        ref={viewportRef}
        className="transcript__viewport"
        data-testid="transcript"
      >
        <ol className="stack transcript__list">
          {transcript.map((item) => (
            <li key={item.id} className="transcript__item">
              <Message
                item={item}
                streaming={busy && item === lastTurn}
                busy={busy}
                sessionKey={sessionKey}
                onApprove={onApprove}
                onAction={onAction}
              />
            </li>
          ))}
        </ol>
      </div>

      {!pinned && (
        <Button
          size="sm"
          variant="secondary"
          className="transcript__jump"
          onClick={() => {
            setPinned(true);
            scrollToBottom('smooth');
          }}
        >
          <ArrowDown />
          {t('chat.jumpToLatest')}
        </Button>
      )}
    </div>
  );
}
