/**
 * Which agent this conversation runs on, in the conversation.
 *
 * It sits in the composer's meta row rather than in the sidebar, because that
 * is where the decision is actually made: choosing an agent is part of asking
 * the question, not part of configuring the application. A picker three
 * columns away from the message box is one nobody touches.
 *
 * The control means two different things and says so. Which of them applies is
 * `useAgentChoice`'s to decide — the composer's `/agent` command asks the same
 * question and must get the same answer — and this file is what that decision
 * looks like on screen.
 *
 * The second case is worth a word of warning in the menu rather than a refusal.
 * Moving a conversation mid-way is a legitimate thing to want — start with a
 * cheap model, escalate — and the history is unchanged by it; what changes is
 * the prompt, tools and permissions the *next* turn runs under.
 */

import { Link } from '@tanstack/react-router';
import {
  BrainCircuit,
  ChevronDown,
  Settings2,
  TriangleAlert,
} from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '@/components/ui/button.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';
import { toast } from '@/components/ui/toast.js';
import { useAgentChoice } from './use-agent-choice.js';

export function AgentPicker({
  sessionKey,
}: {
  readonly sessionKey?: string;
}): JSX.Element {
  const { t } = useTranslation();
  const {
    agents: rows,
    current,
    bound,
    match,
    missing,
    choose: pick,
  } = useAgentChoice(sessionKey);
  const label = match?.label ?? current;

  const choose = (agentId: string): void => {
    void pick(agentId).catch((error: unknown) => {
      toast.error(
        t('agents.moveFailed'),
        error instanceof Error ? error.message : undefined,
      );
    });
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className={
            missing ? 'composer__picker is-missing' : 'composer__picker'
          }
          aria-label={
            missing
              ? t('agents.missingLabel', { id: current })
              : t('agents.pickerLabel', { label })
          }
        >
          {missing ? (
            <TriangleAlert aria-hidden="true" />
          ) : (
            <BrainCircuit aria-hidden="true" />
          )}
          <span className="truncate">{label}</span>
          <ChevronDown className="composer__picker-caret" aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>

      <DropdownMenuContent align="start" side="top" className="floating--menu">
        {/* Said before the list rather than as an empty selection. The radio
            group below matches nothing when the bound agent is gone, and a menu
            with no item checked reads as a rendering bug rather than as a
            conversation pointing at an agent that no longer exists. */}
        {missing && (
          <DropdownMenuLabel className="composer__picker-notice" role="alert">
            {t('agents.missingNotice', { id: current })}
          </DropdownMenuLabel>
        )}

        <DropdownMenuLabel>
          {bound === undefined
            ? t('agents.forThisSession')
            : t('agents.moveThisSession')}
        </DropdownMenuLabel>

        <DropdownMenuRadioGroup value={current} onValueChange={choose}>
          {rows.map((agent) => (
            <DropdownMenuRadioItem key={agent.id} value={agent.id}>
              <span className="truncate">{agent.label}</span>
              {agent.model !== '' && (
                <span className="composer__picker-hint">{agent.model}</span>
              )}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>

        <DropdownMenuSeparator />

        <DropdownMenuItem asChild>
          <Link to="/agents">
            <Settings2 />
            {t('agents.manage')}
          </Link>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
