import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  DEFAULT_LIVE_STATE_TEMPLATE,
  DEFAULT_MEMORY_TEMPLATE,
  DEFAULT_SKILLS_TEMPLATE,
  platformTemplate,
  DEFAULT_SYSTEM_PROMPT_TEMPLATE,
  DEFAULT_TOOL_POLICY_TEMPLATE,
  DEFAULT_WRAP_UP_TEMPLATE,
  LIVE_PROMPT_PLACEHOLDERS,
  MEMORY_PROMPT_PLACEHOLDERS,
  SKILLS_PROMPT_PLACEHOLDERS,
  PLATFORM_PROMPT_PLACEHOLDERS,
  PROMPT_PLACEHOLDERS,
  RAW_PROMPT_PLACEHOLDERS,
  TOOL_POLICY_PLACEHOLDERS,
  namesDelimiter,
} from '@darkwire/protocol';

import { Section, SwitchRow } from '@/components/form/controls.js';
import { TemplateEditor } from '@/agents/template-editor.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

/**
 * The prompt.
 *
 * Seven templates, one component. Empty means "use the built-in", so each box
 * shows the built-in and typing into it is what makes the wording this
 * agent's own. An operator cannot choose to edit something they have never
 * been shown. See `TemplateEditor` for the three states each one holds.
 *
 * The system prompt is open; the rest sit in one disclosure below it, for the
 * reason given there.
 */
export function PromptSection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const {
    name,
    form,
    update,
    formEpoch,
    raw,
    promptUncacheable,
    policyUnfenced,
    platformHint,
    chosenEnvironment,
    toolsOff,
    memoryOff,
    skillsOff,
  } = editor;

  return (
    <Section
      title={t('agents.systemPrompt')}
      description={t('agents.promptDesc')}
    >
      <div className="stack agent-editor__prompt">
        <TemplateEditor
          key={`${String(formEpoch)}-system`}
          name={name}
          label="agents.promptSystem"
          builtIn={DEFAULT_SYSTEM_PROMPT_TEMPLATE}
          value={form.systemPrompt}
          placeholders={raw ? RAW_PROMPT_PLACEHOLDERS : PROMPT_PLACEHOLDERS}
          removable={false}
          hint={raw ? 'agents.promptSystemRawHint' : 'agents.promptSystemHint'}
          {...(promptUncacheable
            ? {
                // Not an error: a clock in the prompt is a legitimate thing to
                // want. It is a price, and one an operator cannot see on the
                // bill, so it is said here instead.
                warning: {
                  title: t('agents.promptUncacheableTitle'),
                  message: t('agents.promptUncacheable'),
                },
              }
            : {})}
          onChange={(next) => {
            update('systemPrompt', next);
          }}
        />

        {/* A disclosure, not a second mode picker. The sections below are the
            rest of the prompt, and an operator who has not gone looking for
            them should not have to answer a question about them first. The
            old "Assembly: Sections / Raw" select made a rare decision the
            opening move of an ordinary screen.

            `<details>` rather than a button and state: it is what the element
            is for, and the keyboard behaviour and `aria-expanded` come out
            correct without being written. */}
        <details className="agent-editor__advanced">
          <summary>{t('agents.promptAdvanced')}</summary>
          <div className="stack agent-editor__advanced-body">
            {/* Phrased as what it does rather than as a mode with a name. An
                operator reaching for this wants "stop adding things to my
                prompt", which is a behaviour; `raw` is our word for it. */}
            <SwitchRow
              label={t('agents.promptOnlySystem')}
              hint={t('agents.promptOnlySystemHint')}
              checked={raw}
              onCheckedChange={(next) => {
                update('promptMode', next ? 'raw' : 'template');
              }}
            />

            {/* Hidden rather than disabled: nothing places them, so a box that
                still edited one would be a control with no effect on screen.
                The stored values survive: switching back restores them. */}
            {!raw && (
              <>
                {/* First in the disclosure because it is second in the
                    prompt. One default text wherever the turn lands, replaced
                    by the definition's own words when the image has any,
                    because only the image knows what it holds. Editing seeds
                    the box with whichever applies, so narrowing an image's
                    tool list is opening it on that list and deleting. */}
                <TemplateEditor
                  key={`${String(formEpoch)}-platform`}
                  name={name}
                  label="agents.promptPlatform"
                  builtIn={platformTemplate(
                    chosenEnvironment?.definition?.prompt ?? '',
                  )}
                  value={form.platformPrompt}
                  placeholders={PLATFORM_PROMPT_PLACEHOLDERS}
                  hint={platformHint}
                  // Tool-shaped like the policy box below: every line it
                  // renders describes running a command, and the file tools it
                  // names are tools too. With none of them there is nothing
                  // left for the section to be about.
                  {...(toolsOff
                    ? {
                        warning: {
                          title: t('agents.toolsOffTitle'),
                          message: t('agents.promptNotPlacedNoTools'),
                        },
                      }
                    : {})}
                  onChange={(next) => {
                    update('platformPrompt', next);
                  }}
                />
                <TemplateEditor
                  key={`${String(formEpoch)}-policy`}
                  name={name}
                  label="agents.promptToolPolicy"
                  builtIn={DEFAULT_TOOL_POLICY_TEMPLATE}
                  value={form.toolPolicyPrompt}
                  placeholders={TOOL_POLICY_PLACEHOLDERS}
                  hint="agents.promptToolPolicyHint"
                  // Ahead of the two delimiter warnings, and it replaces them:
                  // neither says anything while the section is not placed, and
                  // a box carrying three lines about a prompt nothing sends is
                  // three chances to act on the wrong one.
                  {...(toolsOff
                    ? {
                        warning: {
                          title: t('agents.toolsOffTitle'),
                          message: t('agents.promptNotPlacedNoTools'),
                        },
                      }
                    : policyUnfenced
                      ? {
                          warning: {
                            title: t('agents.promptToolPolicyUnfencedTitle'),
                            // Passed rather than written into the bundle:
                            // i18next does not rescan an interpolated value,
                            // which is the only way a literal `{{…}}` survives
                            // to the screen.
                            message: t('agents.promptToolPolicyUnfenced', {
                              tag: '{{tag}}',
                              nonce: '{{nonce}}',
                            }),
                          },
                        }
                      : namesDelimiter(form.toolPolicyPrompt)
                        ? {
                            // Naming the tag here is legal and costs the cache:
                            // this section is otherwise identical for the life
                            // of a session, so it rides the cached prefix. That
                            // holds unless it spells out a delimiter that
                            // changes every turn, at which point the whole of
                            // it is re-sent per step.
                            warning: {
                              title: t(
                                'agents.promptToolPolicyUncacheableTitle',
                              ),
                              message: t('agents.promptToolPolicyUncacheable', {
                                tag: '{{tag}}',
                              }),
                            },
                          }
                        : {})}
                  onChange={(next) => {
                    update('toolPolicyPrompt', next);
                  }}
                />
                <TemplateEditor
                  key={`${String(formEpoch)}-skills`}
                  name={name}
                  label="agents.promptSkills"
                  builtIn={DEFAULT_SKILLS_TEMPLATE}
                  value={form.skillsPrompt}
                  placeholders={SKILLS_PROMPT_PLACEHOLDERS}
                  hint="agents.promptSkillsHint"
                  {...(toolsOff
                    ? {
                        warning: {
                          title: t('agents.toolsOffTitle'),
                          message: t('agents.promptNotPlacedNoTools'),
                        },
                      }
                    : skillsOff
                      ? {
                          warning: {
                            title: t('agents.toolsOffTitle'),
                            message: t('agents.promptNotPlacedNoSkills'),
                          },
                        }
                      : {})}
                  onChange={(next) => {
                    update('skillsPrompt', next);
                  }}
                />
                <TemplateEditor
                  key={`${String(formEpoch)}-memory`}
                  name={name}
                  label="agents.promptMemory"
                  builtIn={DEFAULT_MEMORY_TEMPLATE}
                  value={form.memoryPrompt}
                  placeholders={MEMORY_PROMPT_PLACEHOLDERS}
                  hint="agents.promptMemoryHint"
                  // Two conditions, broadest first. `toolsEnabled` off means
                  // the request advertises no tools at all, so nothing can
                  // open a memory and the section is not placed whatever the
                  // `memory` permission says. Naming the narrower reason
                  // there would send an operator to fix the wrong switch.
                  {...(toolsOff
                    ? {
                        warning: {
                          title: t('agents.toolsOffTitle'),
                          message: t('agents.promptNotPlacedNoTools'),
                        },
                      }
                    : memoryOff
                      ? {
                          warning: {
                            title: t('agents.toolsOffTitle'),
                            message: t('agents.promptNotPlacedNoMemory'),
                          },
                        }
                      : {})}
                  onChange={(next) => {
                    update('memoryPrompt', next);
                  }}
                />
                <TemplateEditor
                  key={`${String(formEpoch)}-live`}
                  name={name}
                  label="agents.promptLive"
                  builtIn={DEFAULT_LIVE_STATE_TEMPLATE}
                  value={form.livePrompt}
                  placeholders={LIVE_PROMPT_PLACEHOLDERS}
                  hint="agents.promptLiveHint"
                  onChange={(next) => {
                    update('livePrompt', next);
                  }}
                />
                <TemplateEditor
                  key={`${String(formEpoch)}-wrapup`}
                  name={name}
                  label="agents.promptWrapUp"
                  builtIn={DEFAULT_WRAP_UP_TEMPLATE}
                  value={form.wrapUpPrompt}
                  placeholders={['iterationsLeft']}
                  hint="agents.promptWrapUpHint"
                  onChange={(next) => {
                    update('wrapUpPrompt', next);
                  }}
                />
              </>
            )}
          </div>
        </details>
      </div>
    </Section>
  );
}
