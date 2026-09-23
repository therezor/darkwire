/**
 * The approval prompt, inline in the tool card that raised it.
 *
 * Inline and not a dialog, deliberately. A modal over the transcript hides the
 * conversation that produced the request, which is the only context that makes
 * the decision answerable: "run `rm -rf build`?" is a different question
 * depending on what was asked a paragraph earlier. It also cannot stack: two
 * tabs, or one turn with two gated calls, would queue modals.
 *
 * Three answers and a refusal:
 *
 *  - **Once**: this call. The next one asks again.
 *  - **This session**: calls the tool treats as this one, for the rest of the
 *    conversation. For `exec` that is this exact command.
 *  - **Always allow…**: `exec` only. Saves a command rule on the agent, where
 *    it can be seen and removed in the agent's settings, and runs this call.
 *    Not offered for a shell, because a rule for one covers every program.
 *
 * The deadline is the server's `expiresAtMs`, counted down locally. When it
 * passes, the buttons go: pressing one would send an answer the gate stopped
 * waiting for, and a button that does nothing is worse than no button.
 */

import { Check, ShieldAlert, X } from 'lucide-react';
import { useEffect, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type {
  ApprovalScope,
  CommandPolicy,
  ExecRule,
} from '@darkwire/protocol';

import { cn } from '@/lib/cn.js';
import {
  formatPattern,
  parsePattern,
  patternMatches,
  suggestPatterns,
} from '@/lib/exec-pattern.js';
import { formatDuration } from '@/lib/format.js';
import { Button } from '@/components/ui/button.js';
import { Tooltip } from '@/components/ui/tooltip.js';
import { TextField } from '@/components/form/controls.js';
import type { ToolApprovalState } from '@/state/transcript.js';

interface ApprovalPromptProps {
  readonly toolName: string;
  readonly approval: ToolApprovalState;
  readonly onAnswer: (
    approved: boolean,
    scope: ApprovalScope,
    rule?: ExecRule,
  ) => void;
}

export function ApprovalPrompt({
  toolName,
  approval,
  onAnswer,
}: ApprovalPromptProps): JSX.Element | null {
  const { t } = useTranslation();
  const remainingMs = useCountdown(approval.expiresAtMs);
  const [editing, setEditing] = useState(false);
  const answered = approval.answered;
  const command = approval.command;

  if (answered !== undefined) {
    return (
      <p
        className={cn(
          'row approval__resolved',
          answered === 'approved'
            ? 'approval__resolved--approved'
            : 'approval__resolved--denied',
        )}
      >
        {answered === 'approved' ? <Check /> : <X />}
        {answered === 'approved'
          ? t('chat.approval.approved')
          : t('chat.approval.denied')}
      </p>
    );
  }

  if (remainingMs <= 0) {
    return (
      <p className="row approval__resolved">{t('chat.approval.closed')}</p>
    );
  }

  const standing = command !== undefined && !command.shell;

  return (
    <div className="stack approval">
      <p className="row approval__line">
        <ShieldAlert />
        <span>
          <strong>{toolName}</strong> {t('chat.approval.needs')}
        </span>
        {/* A live region, because the number changes without anyone acting,
            but off and on a coarse value, or a screen reader reads a
            countdown out loud once a second. */}
        <span className="approval__timer" role="timer" aria-live="off">
          {/* Formatted, not raw seconds: a generous `approvals.timeoutMs`
              otherwise counts down from a four-digit number. */}
          {formatDuration(Math.ceil(remainingMs / 1000) * 1000)}
        </span>
      </p>

      {command?.shell === true && (
        <p className="approval__warning">{t('chat.approval.shellWarning')}</p>
      )}
      {approval.error !== undefined && (
        <p className="approval__error" role="alert">
          {approval.error}
        </p>
      )}

      {editing && command !== undefined ? (
        <RuleEditor
          command={command}
          onCancel={() => {
            setEditing(false);
          }}
          onSave={(rule) => {
            onAnswer(true, 'session', rule);
          }}
        />
      ) : (
        <div className="cluster approval__actions">
          <Tooltip label={t('chat.approval.onceHint')}>
            <Button
              size="sm"
              variant="primary"
              onClick={() => {
                onAnswer(true, 'once');
              }}
            >
              {t('chat.approval.once')}
            </Button>
          </Tooltip>
          <Tooltip
            label={
              command === undefined
                ? t('chat.approval.sessionHint')
                : t('chat.approval.sessionHintCommand')
            }
          >
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                onAnswer(true, 'session');
              }}
            >
              {t('chat.approval.session')}
            </Button>
          </Tooltip>
          {standing && (
            <Tooltip label={t('chat.approval.alwaysHint')}>
              <Button
                size="sm"
                variant="secondary"
                onClick={() => {
                  setEditing(true);
                }}
              >
                {t('chat.approval.always')}
              </Button>
            </Tooltip>
          )}

          <div className="spacer" />

          <Button
            size="sm"
            variant="danger"
            onClick={() => {
              onAnswer(false, 'once');
            }}
          >
            {t('chat.approval.deny')}
          </Button>
        </div>
      )}
    </div>
  );
}

/**
 * Chooses the rule "Always allow…" saves.
 *
 * It starts on the narrowest wildcard rather than the exact command, because
 * the exact command is what "This session" already covers. A rule that would
 * not cover this call cannot be saved: the prompt is answering this call.
 */
function RuleEditor({
  command,
  onCancel,
  onSave,
}: {
  readonly command: CommandPolicy;
  readonly onCancel: () => void;
  readonly onSave: (rule: ExecRule) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const suggestions = suggestPatterns(command.argv).map(formatPattern);
  const [pattern, setPattern] = useState(
    suggestions[1] ?? suggestions[0] ?? '',
  );

  const parsed = parsePattern(pattern);
  const error = !parsed.ok
    ? t(`chat.approval.rule.${parsed.error}`)
    : patternMatches(parsed.argv, command.argv)
      ? undefined
      : t('chat.approval.rule.noMatch');

  return (
    <div className="stack approval__rule">
      <div
        className="cluster approval__suggestions"
        role="group"
        aria-label={t('chat.approval.rule.suggestions')}
      >
        {suggestions.map((suggestion) => (
          <Button
            key={suggestion}
            size="sm"
            variant="ghost"
            aria-pressed={suggestion === pattern}
            className="approval__suggestion"
            onClick={() => {
              setPattern(suggestion);
            }}
          >
            {suggestion}
          </Button>
        ))}
      </div>
      <TextField
        label={t('chat.approval.rule.pattern')}
        hint={t('chat.approval.rule.patternHint')}
        error={error}
        value={pattern}
        spellCheck={false}
        autoComplete="off"
        onValueChange={setPattern}
      />
      <div className="cluster approval__actions">
        <div className="spacer" />
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {t('chat.approval.rule.cancel')}
        </Button>
        <Button
          size="sm"
          variant="primary"
          disabled={!parsed.ok || error !== undefined}
          onClick={() => {
            if (!parsed.ok) return;
            onSave({ action: 'allow', argv: [...parsed.argv] });
          }}
        >
          {t('chat.approval.rule.save')}
        </Button>
      </div>
    </div>
  );
}

/**
 * Milliseconds left, ticking once a second.
 *
 * A second is the right cadence because that is the resolution shown. Ticking
 * on `requestAnimationFrame` would re-render sixty times to change a number
 * once.
 */
function useCountdown(deadlineMs: number): number {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (deadlineMs <= now) return undefined;
    const timer = setInterval(() => {
      setNow(Date.now());
    }, 1000);
    return () => {
      clearInterval(timer);
    };
    // `now` is deliberately not a dependency: it changes on every tick, and
    // including it would tear down and rebuild the interval each second.
  }, [deadlineMs]);

  return deadlineMs - now;
}
