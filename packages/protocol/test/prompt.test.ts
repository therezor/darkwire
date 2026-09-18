/**
 * The prompt template's substitution rules.
 *
 * Most of these assert what happens when the template is *wrong*, which is the
 * half that matters: an operator now owns the whole system prompt, so a typo in
 * it is a normal event rather than a bug report. The rules are that a mistake
 * stays visible and never takes the agent offline.
 */

import { describe, expect, it } from 'vitest';

import {
  DEFAULT_MEMORY_TEMPLATE,
  DEFAULT_SKILLS_TEMPLATE,
  DEFAULT_SYSTEM_PROMPT_TEMPLATE,
  MEMORY_PROMPT_PLACEHOLDERS,
  SKILLS_PROMPT_PLACEHOLDERS,
  PROMPT_PLACEHOLDERS,
  RAW_PROMPT_PLACEHOLDERS,
  renderPromptTemplate,
  unknownPlaceholders,
  type PromptValues,
} from '#src/prompt.js';

const VALUES: PromptValues = {
  name: 'Reviewer',
  workspaceId: 'acme',
  workspaceRoot: '/home/ghost/.darkwire/workspace/acme',
  runtime: 'Linux x64, Node 22.0.0',
};

describe('renderPromptTemplate', () => {
  it('fills every placeholder it knows', () => {
    const rendered = renderPromptTemplate(
      '{{name}} in {{workspaceId}} at {{workspaceRoot}} on {{runtime}}',
      VALUES,
    );

    expect(rendered).toBe(
      'Reviewer in acme at /home/ghost/.darkwire/workspace/acme on Linux x64, Node 22.0.0',
    );
  });

  it('fills a placeholder used more than once', () => {
    expect(
      renderPromptTemplate('# {{name}}\n\nYou are {{name}}.', VALUES),
    ).toBe('# Reviewer\n\nYou are Reviewer.');
  });

  it('leaves an unknown placeholder verbatim rather than deleting it', () => {
    // The failure this prevents: a typo silently removing the sentence that
    // says where the workspace is, with nothing on screen to show it happened.
    expect(renderPromptTemplate('root is {{workspacRoot}}', VALUES)).toBe(
      'root is {{workspacRoot}}',
    );
  });

  it('treats spaced braces as a literal, which is the escape hatch', () => {
    expect(
      renderPromptTemplate('write {{ name }} to mean the placeholder', VALUES),
    ).toBe('write {{ name }} to mean the placeholder');
  });

  it('never rescans what it substituted', () => {
    // A workspace whose name is itself placeholder-shaped cannot expand.
    const rendered = renderPromptTemplate('{{workspaceId}}', {
      ...VALUES,
      workspaceId: '{{workspaceRoot}}',
    });

    expect(rendered).toBe('{{workspaceRoot}}');
  });

  it('returns a template with no placeholders untouched', () => {
    expect(renderPromptTemplate('Just be helpful.', VALUES)).toBe(
      'Just be helpful.',
    );
  });

  it('does not throw on an empty template', () => {
    expect(renderPromptTemplate('', VALUES)).toBe('');
  });
});

describe('unknownPlaceholders', () => {
  it('finds nothing in the built-in template', () => {
    expect(unknownPlaceholders(DEFAULT_SYSTEM_PROMPT_TEMPLATE)).toEqual([]);
  });

  it('reports a typo once, however often it appears', () => {
    expect(unknownPlaceholders('{{nmae}} and {{nmae}} again')).toEqual([
      'nmae',
    ]);
  });

  it('reports several in the order they appear', () => {
    expect(unknownPlaceholders('{{a}} {{name}} {{b}}')).toEqual(['a', 'b']);
  });

  it('ignores the spaced literal form', () => {
    expect(unknownPlaceholders('{{ nmae }}')).toEqual([]);
  });
});

describe('DEFAULT_MEMORY_TEMPLATE', () => {
  it('names no placeholder that nothing will fill', () => {
    expect(
      unknownPlaceholders(DEFAULT_MEMORY_TEMPLATE, MEMORY_PROMPT_PLACEHOLDERS),
    ).toEqual([]);
  });

  it('places the index, which is the whole of what the section carries', () => {
    // A template without it announces a memory tool and names nothing to use
    // it on, which is the one way this section can be actively misleading.
    expect(DEFAULT_MEMORY_TEMPLATE).toContain('{{index}}');
  });

  it('spends nothing on a folder the model never names', () => {
    // The tool takes a key, so a path in the section would be a second
    // spelling of the address, paid for on every request.
    expect(DEFAULT_MEMORY_TEMPLATE).not.toContain('{{path}}');
    expect(MEMORY_PROMPT_PLACEHOLDERS).not.toContain('path');
  });

  it('offers a count it does not use', () => {
    // The same convention as `{{workspaceRoot}}` above: available for a custom
    // template, declined by the default, and not removable without silently
    // changing every stored template that named it.
    expect(MEMORY_PROMPT_PLACEHOLDERS).toContain('count');
    expect(DEFAULT_MEMORY_TEMPLATE).not.toContain('{{count}}');
  });

  it('names the tool and says the keys are exact, and stops there', () => {
    // A list of keys with no instruction to use them reads as a list of things
    // that exist. The skills index learned that first. What follows it is the
    // one rule the data cannot carry: an unlisted key does not exist, and a
    // small model that invents a near miss spends a turn on the error.
    expect(DEFAULT_MEMORY_TEMPLATE).toContain('`memory` tool');
    expect(DEFAULT_MEMORY_TEMPLATE).toContain('Do not guess a key.');
    // Everything about saving lives in the tool description, which is sent
    // once rather than on every request.
    expect(DEFAULT_MEMORY_TEMPLATE).not.toContain('Saving');
    // One line of prose, then the data.
    expect(DEFAULT_MEMORY_TEMPLATE.split('\n\n')).toHaveLength(3);
  });
});

describe('DEFAULT_SKILLS_TEMPLATE', () => {
  it('names no placeholder that nothing will fill', () => {
    expect(
      unknownPlaceholders(DEFAULT_SKILLS_TEMPLATE, SKILLS_PROMPT_PLACEHOLDERS),
    ).toEqual([]);
  });

  it('places the catalogue, and nothing else', () => {
    // One generated block, not two. A sheet's body is never templated: the model
    // opens the file the index names, long after this section is rendered.
    expect(DEFAULT_SKILLS_TEMPLATE).toContain('{{index}}');
    expect(DEFAULT_SKILLS_TEMPLATE).not.toContain('{{pinned}}');
  });

  it('leaves no gap when the index renders to nothing', () => {
    // `{{index}}` carries its own leading blank line, so the template must not
    // write one for it — that is what would leave the gap.
    expect(DEFAULT_SKILLS_TEMPLATE).toContain('names.{{index}}');
  });

  it('does not claim every line is an index line', () => {
    // The wording has to stay true of a catalogue of one. What it replaced
    // dodged the question with a conditional a template cannot express.
    expect(DEFAULT_SKILLS_TEMPLATE).not.toContain('Each line below');
  });
});

describe('DEFAULT_SYSTEM_PROMPT_TEMPLATE', () => {
  it('names no placeholder that nothing will fill', () => {
    expect(unknownPlaceholders(DEFAULT_SYSTEM_PROMPT_TEMPLATE)).toEqual([]);
  });

  it('withholds the two host facts a model misuses when it is given them', () => {
    // Both are still *available* — a custom prompt may want them — and the
    // default deliberately declines. Handed the absolute root, a model writes
    // `<root>/notes/x` and the jail resolves it inside the workspace again;
    // handed the host OS, a containerized agent needs placement wording instead.
    // See the `PROMPT_PLACEHOLDERS` comment for both failures.
    expect(DEFAULT_SYSTEM_PROMPT_TEMPLATE).not.toContain('{{workspaceRoot}}');
    expect(DEFAULT_SYSTEM_PROMPT_TEMPLATE).not.toContain('{{runtime}}');

    // The two it does use, so this test fails if one is dropped by accident
    // rather than silently rendering a prompt with a hole in it.
    for (const placeholder of ['name', 'workspaceId'] as const) {
      expect(PROMPT_PLACEHOLDERS).toContain(placeholder);
      expect(DEFAULT_SYSTEM_PROMPT_TEMPLATE).toContain(`{{${placeholder}}}`);
    }
  });

  it('names no section it cannot leave out', () => {
    // `{{platformPolicy}}` used to be here, and the command policy is now a
    // section beside the tool-output policy instead. A
    // placeholder always renders to *something* — an empty string still leaves
    // the blank lines the template wrote around it — so a section that may not
    // apply cannot be one. Raw mode keeps the placeholder, because it places
    // every section itself and has nowhere else to ask.
    expect(DEFAULT_SYSTEM_PROMPT_TEMPLATE).not.toContain('{{platformPolicy}}');
    expect(PROMPT_PLACEHOLDERS).not.toContain('platformPolicy');
    expect(RAW_PROMPT_PLACEHOLDERS).toContain('platformPolicy');
  });

  it('renders to a prompt with no braces left in it', () => {
    const rendered = renderPromptTemplate(
      DEFAULT_SYSTEM_PROMPT_TEMPLATE,
      VALUES,
    );

    expect(rendered).not.toContain('{{');
    expect(rendered).toContain('# Reviewer');
    expect(rendered).toContain('It is the only place you');
    expect(rendered).toContain('## Guidelines');
  });
});
