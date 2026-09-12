/**
 * The box that edits one of an agent's six prompt templates.
 *
 * Lifted out of `agent-editor.tsx` whole. It shares no state with the editor
 * around it — everything it needs arrives as props — and it carries a small
 * state machine of its own, which is the reason it was worth its own file.
 */

import { AlertTriangle, RotateCcw, Trash2 } from 'lucide-react';
import { useMemo, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { unknownPlaceholders } from '@ghostwire/protocol';

import { Button } from '@/components/ui/button.js';
import { NoticeBlock } from '@/components/ui/notice.js';
import { CodeEditor } from '@/files/code-editor.js';
import type { WebKey } from '@/i18n/keys.js';

/**
 * One template an agent owns, with the three states it can be in.
 *
 * Six boxes on this screen behave identically, and the behaviour is not obvious
 * enough to reimplement six times: **empty inherits the built-in, and a single
 * space deletes the section.** Both are real answers, and neither is expressible
 * by typing — an operator cannot type "inherit", and a space is invisible — so
 * each gets a button and a state the box says out loud.
 *
 * **Ownership is state, not `value !== ''`.** Deriving it made the box fight the
 * person typing in it: selecting all and deleting — the obvious way to start a
 * short prompt from scratch — emptied the field, which flipped it back to "not
 * owned", which refilled the box with the built-in. The next keystroke landed at
 * the end of a page of text nobody asked for.
 */
export function TemplateEditor({
  name,
  label,
  builtIn,
  value,
  placeholders,
  hint,
  removable = true,
  warning,
  onChange,
}: {
  readonly name: string;
  /** Typed `WebKey` rather than `string`, or a deleted key renders as itself. */
  readonly label: WebKey;
  /** What an empty value renders as. Shown in the box until the first keystroke. */
  readonly builtIn: string;
  readonly value: string;
  readonly placeholders: readonly string[];
  readonly hint?: WebKey;
  /** `systemPrompt` is not: an agent with no identity is never what was meant. */
  readonly removable?: boolean;
  /**
   * A second warning the caller decides, shown beside the stray-placeholder one.
   *
   * A title and a message rather than one string: it renders as a `NoticeBlock`
   * like every other warning in the app, and that shape wants both.
   */
  readonly warning?:
    { readonly title: string; readonly message: string } | undefined;
  readonly onChange: (next: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [owned, setOwned] = useState(() => value.trim() !== '');
  // A stored value that is whitespace but not empty is the deletion. It is the
  // one state with nothing to edit, so it renders as a sentence and a button
  // rather than as a box holding a space nobody can see.
  const removed = value !== '' && value.trim() === '';
  const text = owned && !removed ? value : builtIn;
  const stray = useMemo(
    () => unknownPlaceholders(text, placeholders),
    [text, placeholders],
  );

  return (
    <div className="stack agent-editor__template">
      <div className="cluster agent-editor__prompt-bar">
        <span className="micro-label">{t(label)}</span>
        <span className="agent-editor__template-state">
          {t(
            removed
              ? 'agents.promptRemoved'
              : owned
                ? 'agents.promptOwn'
                : 'agents.promptBuiltIn',
          )}
        </span>
        <span className="spacer" />
        {/* Both buttons are named for their section. Six of each on this screen
            read identically otherwise, which leaves a screen reader — and a
            test — with no way to say which one it means. */}
        {removable && !removed && (
          <Button
            variant="ghost"
            size="sm"
            aria-label={t('agents.promptRemoveFor', { label: t(label) })}
            onClick={() => {
              // A single space, because empty already means "inherit". See the
              // asymmetry documented on `AgentEntry.livePrompt`.
              onChange(' ');
            }}
          >
            <Trash2 aria-hidden="true" />
            {t('agents.promptRemove')}
          </Button>
        )}
        {(owned || removed) && (
          <Button
            variant="ghost"
            size="sm"
            aria-label={t(
              removed ? 'agents.promptRestoreFor' : 'agents.promptResetFor',
              {
                label: t(label),
              },
            )}
            onClick={() => {
              setOwned(false);
              onChange('');
            }}
          >
            <RotateCcw aria-hidden="true" />
            {t(removed ? 'agents.promptRestore' : 'agents.resetBuiltin')}
          </Button>
        )}
      </div>

      {removed ? (
        <p className="agent-editor__hint">{t('agents.promptRemovedHint')}</p>
      ) : (
        <>
          {/* Above the editor, not under it. A box holding a page of Markdown
              pushes anything below it off the screen, so a warning there is one
              the operator scrolls past to reach the text it is about. */}
          {stray.length > 0 && (
            <NoticeBlock
              role="alert"
              tone="warning"
              icon={AlertTriangle}
              title={t('agents.promptStrayTitle')}
              message={t('agents.promptStray', {
                names: stray
                  .map((placeholder) => `{{${placeholder}}}`)
                  .join(', '),
              })}
            />
          )}
          {warning !== undefined && (
            <NoticeBlock
              role="alert"
              tone="warning"
              icon={AlertTriangle}
              title={warning.title}
              message={warning.message}
            />
          )}
          <CodeEditor
            value={text}
            readOnly={false}
            language="markdown"
            label={t('agents.promptEditorFor', { label: t(label), name })}
            onChange={(next) => {
              // The first keystroke is the decision: from here the text is this
              // agent's, including when it is emptied.
              setOwned(true);
              onChange(next);
            }}
          />
          <p className="agent-editor__hint">
            {hint === undefined ? '' : `${t(hint)} `}
            {t(
              owned ? 'agents.promptPlaceholders' : 'agents.promptAdoptHint',
            )}{' '}
            {placeholders.map((placeholder) => `{{${placeholder}}}`).join(', ')}
            .
          </p>
        </>
      )}
    </div>
  );
}
