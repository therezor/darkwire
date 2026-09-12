/**
 * The advisory notices, as badges.
 *
 * `prompt_injection` is the one that shaped the rest. Detection in GhostAI is
 * **non-destructive**: the tool output passes through intact and the nonce
 * envelope does the actual defending, because replacing a matched result with a
 * warning banner means reading this project's own security documentation wipes
 * the output and leaves the model reasoning around a hole. So the UI's job is
 * exactly this badge — say that something in the output looked like an
 * instruction, and show the output anyway.
 *
 * The tones are chosen so the two that describe a *refusal* look different from
 * the three that describe a *degradation*: being denied a tool call and having
 * an image dropped to fit a context window are not the same news.
 */

import {
  AlertTriangle,
  BrainCircuit,
  Info,
  ShieldAlert,
  Scissors,
  ShieldX,
  Wrench,
} from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import type { WebKey } from '@/i18n/keys.js';

import type { NoticeKind } from '@ghostwire/protocol';

import type { BadgeProps } from '@/components/ui/badge.js';
import { NoticeBlock } from '@/components/ui/notice.js';

const NOTICES: Record<
  NoticeKind,
  {
    readonly label: WebKey;
    readonly tone: NonNullable<BadgeProps['tone']>;
    readonly icon: typeof Info;
  }
> = {
  prompt_injection: {
    label: 'chat.notices.prompt_injection',
    tone: 'danger',
    icon: ShieldAlert,
  },
  approval_denied: {
    label: 'chat.notices.approval_denied',
    tone: 'danger',
    icon: ShieldX,
  },
  degraded: {
    label: 'chat.notices.degraded',
    tone: 'warning',
    icon: AlertTriangle,
  },
  truncated_history: {
    label: 'chat.notices.truncated_history',
    tone: 'warning',
    icon: Scissors,
  },
  provider_fallback: {
    label: 'chat.notices.provider_fallback',
    tone: 'info',
    icon: Info,
  },
  // Warning rather than info: the substituted agent may allow tools the one
  // this conversation names did not, so the turn ran with wider powers than
  // were configured for it.
  agent_fallback: {
    label: 'chat.notices.agent_fallback',
    tone: 'warning',
    icon: BrainCircuit,
  },
  // Warning rather than danger: nothing was blocked and nothing was at risk —
  // the model reached for a tool this model is not sent, which is a mismatch to
  // fix in the agent's settings rather than an incident.
  tools_disabled: {
    label: 'chat.notices.tools_disabled',
    tone: 'warning',
    icon: Wrench,
  },
};

export function Notice({
  kind,
  message,
  className,
}: {
  readonly kind: NoticeKind;
  readonly message: string;
  readonly className?: string;
}): JSX.Element {
  const { t } = useTranslation();
  const { label, tone, icon: Icon } = NOTICES[kind];
  const text = t(label);

  // The shape is `NoticeBlock`'s; this file's job is the vocabulary — which
  // wire event gets which words, which tone and which icon.
  return (
    <NoticeBlock
      title={text}
      message={message}
      tone={tone}
      icon={Icon}
      {...(className === undefined ? {} : { className })}
    />
  );
}
