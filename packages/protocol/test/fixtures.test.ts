/**
 * The parity oracle for this package's pure functions and wire shapes.
 *
 * Every case here is run through the TypeScript implementation and written to
 * `fixtures/` under `DARKWIRE_UPDATE_FIXTURES=1`; otherwise the committed file
 * has to match byte for byte. The Rust crate reads the same files and asserts
 * its own results against them, so a function that drifts between the two
 * languages fails a test on whichever side moved.
 *
 * Three families:
 *
 * - `fixtures/protocol/<function>.json` — one file per dual-implemented
 *   function, `{function, cases: [{name, input, output}]}`. The input is the
 *   function's named arguments; the output is what it returned, with
 *   `undefined` spelled `null`.
 * - `fixtures/ws/frames/<type>.json` — one valid, fully populated frame per
 *   client and server message variant, so the Rust side can prove it parses
 *   and re-serialises every variant unchanged.
 * - `fixtures/uuid/v7.json` — an instant and ten random bytes in, the id out,
 *   pinning the version and variant masking.
 */

import { readdirSync } from 'node:fs';

import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  ClientMessageSchema,
  ConfigSchema,
  DEFAULT_LIVE_STATE_TEMPLATE,
  DEFAULT_MEMORY_TEMPLATE,
  DEFAULT_PLATFORM_TEMPLATE,
  DEFAULT_SKILLS_TEMPLATE,
  DEFAULT_SYSTEM_PROMPT_TEMPLATE,
  DEFAULT_TOOL_POLICY_TEMPLATE,
  DEFAULT_WRAP_UP_TEMPLATE,
  LIVE_PROMPT_PLACEHOLDERS,
  MEMORY_PROMPT_PLACEHOLDERS,
  PLATFORM_PROMPT_PLACEHOLDERS,
  PROMPT_PLACEHOLDERS,
  PROTOCOL_VERSION,
  RAW_PROMPT_PLACEHOLDERS,
  RESERVED_AGENT_IDS,
  SKILLS_PROMPT_PLACEHOLDERS,
  ServerMessageSchema,
  TOOL_POLICY_PLACEHOLDERS,
  agentSettingsPatch,
  applyToolPrompts,
  defaultSubagentPrompt,
  deriveAgentId,
  deriveWorkspaceId,
  effectiveToolPolicy,
  isLoopbackHost,
  isSequencedServerMessage,
  isSlugId,
  namesDelimiter,
  newUuid,
  platformTemplate,
  renderPromptTemplate,
  renderWrapUp,
  slugify,
  subagentRunsOf,
  subagentToolName,
  tokensPerSecond,
  toolPolicyUsesNonce,
  turnRate,
  unknownPlaceholders,
  withSubagentRun,
  type AgentSettingsChange,
  type ClientMessage,
  type ServerMessage,
  type SubagentRunRef,
  type ToolDefinition,
  type ToolPromptOverrides,
  type TurnTiming,
  type Usage,
} from '#src/index.js';
import { checkJsonFixture, fixturePath } from './fixture-file.js';

type Json = Record<string, unknown>;

interface Fixture {
  readonly cases: ReadonlyArray<{
    readonly name: string;
    readonly input: Json;
  }>;
  readonly run: (input: Json) => unknown;
}

/** `undefined` has no JSON spelling; the fixture writes `null`. */
function orNull(value: unknown): unknown {
  return value === undefined ? null : value;
}

/** What an environment definition says about its own image. */
const IMAGE_NOTES =
  'Alpine 3.23. git, util-linux and ca-certificates are installed.\n' +
  'The shell is ash, not bash.';

const VALUES = {
  name: 'Reviewer',
  workspaceId: 'acme',
  workspaceRoot: '/home/ghost/.darkwire/workspace/acme',
  runtime: 'Linux x64, Node 22.0.0',
};

const USAGE: Usage = {
  promptTokens: 100,
  completionTokens: 250,
  totalTokens: 350,
};

const DEFINITION: ToolDefinition = {
  name: 'read_file',
  description: 'Reads a file in the workspace.',
  risk: 'safe',
  source: 'builtin',
  parameters: {
    type: 'object',
    additionalProperties: false,
    required: ['path'],
    properties: {
      path: { type: 'string', description: 'Workspace-relative path.' },
      limit: { type: 'number' },
    },
  },
};

const CODER_CONFIG = {
  agents: {
    list: {
      coder: {
        label: 'Coder',
        systemPrompt: 'Write code.',
        model: 'llama3',
        provider: 'ollama',
        temperature: 0.7,
        tools: { read_file: 'allow' },
      },
    },
  },
};

const RUN: SubagentRunRef = {
  sessionKey: 'sub-1',
  agentId: 'researcher',
  label: 'Researcher',
};

const FIXTURES: Readonly<Record<string, Fixture>> = {
  renderPromptTemplate: {
    cases: [
      {
        name: 'fills every placeholder it knows',
        input: {
          template:
            '{{name}} in {{workspaceId}} at {{workspaceRoot}} on {{runtime}}',
          values: VALUES,
        },
      },
      {
        name: 'fills a placeholder used more than once',
        input: { template: '# {{name}}\n\nYou are {{name}}.', values: VALUES },
      },
      {
        name: 'leaves an unknown placeholder verbatim',
        input: { template: 'root is {{workspacRoot}}', values: VALUES },
      },
      {
        name: 'treats spaced braces as a literal',
        input: {
          template: 'write {{ name }} to mean the placeholder',
          values: VALUES,
        },
      },
      {
        name: 'never rescans what it substituted',
        input: {
          template: '{{workspaceId}}',
          values: { ...VALUES, workspaceId: '{{workspaceRoot}}' },
        },
      },
      {
        name: 'returns a template with no placeholders untouched',
        input: { template: 'Just be helpful.', values: VALUES },
      },
      {
        name: 'does not throw on an empty template',
        input: { template: '', values: VALUES },
      },
      {
        name: 'keeps astral-plane characters in the template and the values',
        input: {
          template: '𝔘𝔫𝔦𝔠𝔬𝔡𝔢 {{name}} 🚀 {{workspaceId}}',
          values: { ...VALUES, name: '😀 Reviewer', workspaceId: 'Ω' },
        },
      },
      {
        name: 'preserves CRLF line endings',
        input: { template: '# {{name}}\r\n\r\nLine two.\r\n', values: VALUES },
      },
      {
        name: 'default: DEFAULT_SYSTEM_PROMPT_TEMPLATE',
        input: { template: DEFAULT_SYSTEM_PROMPT_TEMPLATE, values: VALUES },
      },
      {
        name: 'default: DEFAULT_LIVE_STATE_TEMPLATE',
        input: {
          template: DEFAULT_LIVE_STATE_TEMPLATE,
          values: {
            time: '2026-07-30T13:05:40.935Z (host time zone: Europe/London)',
            tag: 'ghost-output-a1b2c3',
            wrapUp: '',
          },
        },
      },
      {
        name: 'default: DEFAULT_WRAP_UP_TEMPLATE',
        input: {
          template: DEFAULT_WRAP_UP_TEMPLATE,
          values: { iterationsLeft: '2' },
        },
      },
      {
        // Named and still filled, so a stored template that uses them keeps
        // working. The default itself names neither.
        name: 'default: DEFAULT_PLATFORM_TEMPLATE',
        input: {
          template: DEFAULT_PLATFORM_TEMPLATE,
          values: {
            runtime: 'Linux x64, DarkWire 0.5.0',
            platform: 'linux',
            workspaceId: 'acme',
            shellPolicy:
              '\n\nA POSIX shell is available through `["sh","-c","…"]`.',
          },
        },
      },
      {
        name: 'default: DEFAULT_TOOL_POLICY_TEMPLATE',
        input: {
          template: DEFAULT_TOOL_POLICY_TEMPLATE,
          values: { nonce: 'a1b2c3', tag: 'ghost-output-a1b2c3' },
        },
      },
      {
        name: 'default: DEFAULT_SKILLS_TEMPLATE',
        input: {
          template: DEFAULT_SKILLS_TEMPLATE,
          values: {
            path: 'skills',
            index: '\n\n- skills/code-review/SKILL.md — Review a change.',
            indexLines: '- skills/code-review/SKILL.md — Review a change.',
            count: '1',
          },
        },
      },
      {
        name: 'default: DEFAULT_MEMORY_TEMPLATE',
        input: {
          template: DEFAULT_MEMORY_TEMPLATE,
          values: {
            path: 'memory/',
            index: '- memory/deploy.md — deploy: how the site ships.',
            count: '1',
          },
        },
      },
    ],
    run: (input) =>
      renderPromptTemplate(
        input.template as string,
        input.values as Record<string, string>,
      ),
  },
  unknownPlaceholders: {
    cases: [
      {
        name: 'finds nothing in the built-in template',
        input: { template: DEFAULT_SYSTEM_PROMPT_TEMPLATE },
      },
      {
        name: 'reports a typo once',
        input: { template: '{{nmae}} and {{nmae}} again' },
      },
      {
        name: 'reports several in the order they appear',
        input: { template: '{{a}} {{name}} {{b}}' },
      },
      {
        name: 'ignores the spaced literal form',
        input: { template: '{{ nmae }}' },
      },
      {
        name: 'ignores an astral-plane name, which is not placeholder-shaped',
        input: { template: '{{😀}} {{nmae}}\r\n{{b2}}' },
      },
      {
        name: 'vocabulary: PROMPT_PLACEHOLDERS',
        input: {
          template: DEFAULT_SYSTEM_PROMPT_TEMPLATE,
          known: PROMPT_PLACEHOLDERS,
        },
      },
      {
        name: 'vocabulary: LIVE_PROMPT_PLACEHOLDERS',
        input: {
          template: DEFAULT_LIVE_STATE_TEMPLATE,
          known: LIVE_PROMPT_PLACEHOLDERS,
        },
      },
      {
        name: 'vocabulary: PLATFORM_PROMPT_PLACEHOLDERS',
        input: {
          template: `${DEFAULT_PLATFORM_TEMPLATE}\n{{runtime}} {{shellPolicy}}`,
          known: PLATFORM_PROMPT_PLACEHOLDERS,
        },
      },
      {
        name: 'vocabulary: TOOL_POLICY_PLACEHOLDERS',
        input: {
          template: DEFAULT_TOOL_POLICY_TEMPLATE,
          known: TOOL_POLICY_PLACEHOLDERS,
        },
      },
      {
        name: 'vocabulary: MEMORY_PROMPT_PLACEHOLDERS',
        input: {
          template: DEFAULT_MEMORY_TEMPLATE,
          known: MEMORY_PROMPT_PLACEHOLDERS,
        },
      },
      {
        name: 'vocabulary: SKILLS_PROMPT_PLACEHOLDERS',
        input: {
          template: DEFAULT_SKILLS_TEMPLATE,
          known: SKILLS_PROMPT_PLACEHOLDERS,
        },
      },
      {
        name: 'vocabulary: RAW_PROMPT_PLACEHOLDERS',
        input: {
          template:
            '{{time}} {{toolPolicy}} {{platformPolicy}} {{workspacRoot}}',
          known: RAW_PROMPT_PLACEHOLDERS,
        },
      },
    ],
    run: (input) =>
      input.known === undefined
        ? unknownPlaceholders(input.template as string)
        : unknownPlaceholders(
            input.template as string,
            input.known as readonly string[],
          ),
  },
  renderWrapUp: {
    cases: [
      {
        name: 'renders the default with a leading blank line',
        input: { template: DEFAULT_WRAP_UP_TEMPLATE, iterationsLeft: 2 },
      },
      {
        name: 'clamps a negative count to zero',
        input: { template: DEFAULT_WRAP_UP_TEMPLATE, iterationsLeft: -3 },
      },
      {
        name: 'a single space renders to nothing at all',
        input: { template: ' ', iterationsLeft: 1 },
      },
      {
        name: 'an empty template renders to nothing at all',
        input: { template: '', iterationsLeft: 1 },
      },
      {
        name: 'trims surrounding whitespace before adding the break',
        input: {
          template: '  \n{{iterationsLeft}} left  \n',
          iterationsLeft: 5,
        },
      },
      {
        name: 'keeps astral-plane text',
        input: { template: '🚀 {{iterationsLeft}} 𝔩𝔢𝔣𝔱', iterationsLeft: 1 },
      },
      {
        name: 'trims CRLF at the ends and keeps it inside',
        input: { template: '\r\nA\r\nB\r\n', iterationsLeft: 1 },
      },
    ],
    run: (input) =>
      renderWrapUp(input.template as string, input.iterationsLeft as number),
  },
  platformTemplate: {
    cases: [
      {
        name: 'an image that says nothing inherits the default template',
        input: { notes: '' },
      },
      {
        name: 'whitespace says nothing either',
        input: { notes: '   \n  ' },
      },
      {
        name: "the definition's own words, under a heading it did not write",
        input: { notes: IMAGE_NOTES },
      },
      {
        name: 'surrounding whitespace is trimmed off the notes',
        input: { notes: '\n\t  Alpine 3.23.  \n\n' },
      },
      {
        name: 'CRLF and astral text survive',
        input: { notes: 'A\r\nB 😀' },
      },
    ],
    run: (input) => platformTemplate(input.notes as string),
  },
  effectiveToolPolicy: {
    cases: [
      { name: 'unset means the built-in', input: {} },
      { name: 'empty means the built-in', input: { template: '' } },
      {
        name: 'a single space is a policy of its own',
        input: { template: ' ' },
      },
      {
        name: 'anything else is used verbatim',
        input: { template: 'Trust nothing. {{tag}}' },
      },
      {
        name: 'CRLF and astral text survive',
        input: { template: 'A\r\nB 😀' },
      },
    ],
    run: (input) => effectiveToolPolicy(input.template as string | undefined),
  },
  toolPolicyUsesNonce: {
    cases: [
      { name: 'the built-in does not', input: {} },
      { name: 'empty falls back to the built-in', input: { template: '' } },
      {
        name: 'a template naming the tag does',
        input: { template: 'Delimiter: {{tag}}' },
      },
      {
        name: 'a template naming the nonce does',
        input: { template: 'Nonce: {{nonce}}' },
      },
      {
        name: 'a template naming neither does not',
        input: { template: 'No delimiter here.' },
      },
      {
        name: 'a spaced placeholder is a literal',
        input: { template: '{{ tag }}' },
      },
    ],
    run: (input) => toolPolicyUsesNonce(input.template as string | undefined),
  },
  namesDelimiter: {
    cases: [
      { name: 'tag', input: { template: 'see {{tag}}' } },
      { name: 'nonce', input: { template: 'see {{nonce}}' } },
      { name: 'neither', input: { template: 'see {{name}}' } },
      { name: 'empty', input: { template: '' } },
      { name: 'CRLF around it', input: { template: '\r\n{{tag}}\r\n' } },
      { name: 'astral text around it', input: { template: '😀{{nonce}}😀' } },
    ],
    run: (input) => namesDelimiter(input.template as string),
  },
  tokensPerSecond: {
    cases: [
      {
        name: 'divides completion tokens by wall time',
        input: { usage: USAGE, elapsedMs: 1000 },
      },
      { name: 'two seconds', input: { usage: USAGE, elapsedMs: 2000 } },
      { name: 'a fractional rate', input: { usage: USAGE, elapsedMs: 3000 } },
      {
        name: 'nothing for zero elapsed',
        input: { usage: USAGE, elapsedMs: 0 },
      },
      {
        name: 'nothing for negative elapsed',
        input: { usage: USAGE, elapsedMs: -1 },
      },
      {
        name: 'nothing for a turn that produced nothing',
        input: {
          usage: { promptTokens: 10, completionTokens: 0, totalTokens: 10 },
          elapsedMs: 1000,
        },
      },
    ],
    run: (input) =>
      orNull(tokensPerSecond(input.usage as Usage, input.elapsedMs as number)),
  },
  turnRate: {
    cases: [
      {
        name: 'divides by generation time',
        input: {
          usage: USAGE,
          timing: {
            generationMs: 1000,
            generationTokens: 250,
            elapsedMs: 10_000,
          },
        },
      },
      {
        name: 'divides the timed tokens only',
        input: {
          usage: USAGE,
          timing: {
            generationMs: 1000,
            generationTokens: 150,
            elapsedMs: 10_000,
          },
        },
      },
      {
        name: 'falls back to the wall clock',
        input: { usage: USAGE, timing: { elapsedMs: 2000 } },
      },
      {
        name: 'a zero window is unmeasured',
        input: {
          usage: USAGE,
          timing: { generationMs: 0, generationTokens: 0, elapsedMs: 2000 },
        },
      },
      {
        name: 'a window without tokens falls back',
        input: {
          usage: USAGE,
          timing: { generationMs: 1000, elapsedMs: 2000 },
        },
      },
      {
        name: 'a window with zero tokens falls back',
        input: {
          usage: USAGE,
          timing: { generationMs: 1000, generationTokens: 0, elapsedMs: 2000 },
        },
      },
      {
        name: 'tokens without a window fall back',
        input: {
          usage: USAGE,
          timing: { generationTokens: 150, elapsedMs: 2000 },
        },
      },
      { name: 'nothing with no divisor', input: { usage: USAGE, timing: {} } },
      {
        name: 'nothing with only a zero window',
        input: { usage: USAGE, timing: { generationMs: 0 } },
      },
      {
        name: 'nothing with only a window',
        input: { usage: USAGE, timing: { generationMs: 1000 } },
      },
      {
        name: 'nothing for a turn that produced no tokens',
        input: {
          usage: { promptTokens: 10, completionTokens: 0, totalTokens: 10 },
          timing: { generationMs: 1000 },
        },
      },
      {
        name: 'a fractional rate',
        input: {
          usage: USAGE,
          timing: { generationMs: 3000, generationTokens: 100 },
        },
      },
    ],
    run: (input) =>
      orNull(turnRate(input.usage as Usage, input.timing as TurnTiming)),
  },
  isLoopbackHost: {
    cases: [
      ...[
        '127.0.0.1',
        '127.1.2.3',
        '127.255.255.255',
        'localhost',
        'LOCALHOST',
        '::1',
        '[::1]',
        '  127.0.0.1 ',
      ].map((host) => ({ name: `loopback: ${host}`, input: { host } })),
      ...[
        '0.0.0.0',
        '::',
        '',
        '192.168.1.10',
        '10.0.0.1',
        'example.com',
        '128.0.0.1',
        '127.0.0.1.evil.com',
        '1270.0.0.1',
        '127.0.0',
        '127.0.0.1000',
        '127.a.b.c',
        '\u{1F600}',
      ].map((host) => ({
        name: `remote: ${host === '' ? '(empty)' : host}`,
        input: { host },
      })),
    ],
    run: (input) => isLoopbackHost(input.host as string),
  },
  agentSettingsPatch: {
    cases: [
      {
        name: 'the default agent, moved to a new model',
        input: {
          config: {},
          agentId: 'default',
          changes: { model: 'gpt-4o', provider: 'openai' },
        },
      },
      {
        name: 'a named agent is sent back whole',
        input: {
          config: CODER_CONFIG,
          agentId: 'coder',
          changes: { model: 'gpt-4o', provider: 'openai' },
        },
      },
      {
        name: 'null clears the temperature',
        input: {
          config: CODER_CONFIG,
          agentId: 'coder',
          changes: { temperature: null },
        },
      },
      {
        name: 'a number sets the temperature',
        input: {
          config: CODER_CONFIG,
          agentId: 'coder',
          changes: { temperature: 1.5 },
        },
      },
      {
        name: 'null clears the reasoning effort',
        input: {
          config: { agents: { list: { coder: { reasoningEffort: 'high' } } } },
          agentId: 'coder',
          changes: { reasoningEffort: null },
        },
      },
      {
        name: 'a value sets the reasoning effort',
        input: {
          config: CODER_CONFIG,
          agentId: 'coder',
          changes: { reasoningEffort: 'low' },
        },
      },
      {
        name: 'an unknown id writes a whole agent',
        input: {
          config: CODER_CONFIG,
          agentId: 'ghost',
          changes: { model: 'gpt-4o' },
        },
      },
      {
        name: 'nothing mentioned sends the entry back unchanged',
        input: { config: CODER_CONFIG, agentId: 'coder', changes: {} },
      },
    ],
    run: (input) =>
      agentSettingsPatch(
        ConfigSchema.parse(input.config),
        input.agentId as string,
        input.changes as AgentSettingsChange,
      ),
  },
  slugify: {
    cases: [
      {
        name: 'lowercases and hyphenates',
        input: { name: 'Code Reviewer', reserved: [], fallback: 'x' },
      },
      {
        name: 'collapses runs and trims the ends',
        input: { name: '  Spaced  out  ', reserved: [], fallback: 'x' },
      },
      {
        name: 'drops non-ASCII letters',
        input: { name: 'Ünïcödé', reserved: [], fallback: 'x' },
      },
      {
        name: 'falls back when nothing usable is left',
        input: { name: '///', reserved: [], fallback: 'thing' },
      },
      {
        name: 'falls back on empty',
        input: { name: '', reserved: [], fallback: 'thing' },
      },
      {
        name: 'falls back on a reserved result',
        input: { name: 'Nul', reserved: ['nul'], fallback: 'thing' },
      },
      {
        name: 'truncates to forty characters and trims a trailing hyphen',
        input: { name: 'a'.repeat(39) + '-b', reserved: [], fallback: 'x' },
      },
      {
        name: 'truncates a long name',
        input: { name: 'x'.repeat(50), reserved: [], fallback: 'x' },
      },
      {
        name: 'astral-plane characters are separators',
        input: { name: '😀 Agent 😀 One 𝔗𝔴𝔬', reserved: [], fallback: 'x' },
      },
      {
        name: 'CRLF is a separator',
        input: { name: 'one\r\ntwo', reserved: [], fallback: 'x' },
      },
    ],
    run: (input) =>
      slugify(input.name as string, {
        reserved: new Set(input.reserved as string[]),
        fallback: input.fallback as string,
      }),
  },
  deriveAgentId: {
    cases: [
      ...[
        ['Code Reviewer', 'plain'],
        ['  Spaced  out  ', 'spaced'],
        ['Ünïcödé', 'accented'],
        ['///', 'slashes'],
        ['', 'empty'],
        ['default', 'the default id'],
        ['CON', 'a device name'],
        ['Default', 'the default id, capitalised'],
        ['Reviewer', 'capitalised'],
        ['😀 Reviewer 😀', 'astral'],
        ['line\r\nbreak', 'CRLF'],
      ].map(([label, name]) => ({ name: name ?? '', input: { label } })),
    ],
    run: (input) => deriveAgentId(input.label as string),
  },
  deriveWorkspaceId: {
    cases: [
      ...[
        ['Client Acme', 'plain'],
        ['  Spaced  out  ', 'spaced'],
        ['Ünïcödé', 'accented'],
        ['///', 'slashes'],
        ['', 'empty'],
        ['default', 'the default id'],
        ['CON', 'a device name'],
        ['Work', 'capitalised'],
        ['😀 Work 😀', 'astral'],
        ['a\r\nb', 'CRLF'],
      ].map(([label, name]) => ({ name: name ?? '', input: { name: label } })),
    ],
    run: (input) => deriveWorkspaceId(input.name as string),
  },
  isSlugId: {
    cases: [
      ...['a', '2024', 'code-reviewer', 'default', 'a'.repeat(40)].map(
        (value) => ({ name: `legal: ${value}`, input: { value } }),
      ),
      ...[
        '',
        '..',
        'a/b',
        'a\\b',
        'c:',
        'a\0b',
        '~agent',
        '-agent',
        'agent-',
        'Reviewer',
        'my agent',
        'a'.repeat(41),
        '😀',
        'a\r\nb',
      ].map((value) => ({
        name: `illegal: ${JSON.stringify(value)}`,
        input: { value },
      })),
    ],
    run: (input) => isSlugId(input.value as string),
  },
  subagentToolName: {
    cases: [
      { name: 'a plain id', input: { agentId: 'researcher' } },
      {
        name: 'hyphens become underscores',
        input: { agentId: 'code-review-bot' },
      },
      { name: 'a single character', input: { agentId: 'a' } },
    ],
    run: (input) => subagentToolName(input.agentId as string),
  },
  subagentRunsOf: {
    cases: [
      { name: 'an empty bag', input: { metadata: {} } },
      {
        name: 'a well-formed map',
        input: {
          metadata: {
            subagentRuns: {
              c1: RUN,
              c2: { ...RUN, sessionKey: 'sub-2', label: 'Second' },
            },
          },
        },
      },
      {
        name: 'ignores a non-object map',
        input: { metadata: { subagentRuns: 'nope' } },
      },
      {
        name: 'ignores an array',
        input: { metadata: { subagentRuns: [RUN] } },
      },
      {
        name: 'skips an entry with no session key',
        input: {
          metadata: { subagentRuns: { c1: { agentId: 'x' }, c2: RUN } },
        },
      },
      {
        name: 'skips an entry with an empty session key',
        input: {
          metadata: {
            subagentRuns: { c1: { ...RUN, sessionKey: '' }, c2: RUN },
          },
        },
      },
      {
        name: 'skips a non-object entry',
        input: { metadata: { subagentRuns: { c1: 5, c2: null, c3: RUN } } },
      },
      {
        name: 'fills a missing agent and label with empty strings',
        input: { metadata: { subagentRuns: { c1: { sessionKey: 'sub-1' } } } },
      },
      {
        name: 'coerces a non-string agent and label to empty strings',
        input: {
          metadata: {
            subagentRuns: {
              c1: { sessionKey: 'sub-1', agentId: 7, label: true },
            },
          },
        },
      },
      {
        name: 'keeps astral-plane text',
        input: {
          metadata: {
            subagentRuns: { 'c-😀': { ...RUN, label: '𝔏𝔞𝔟𝔢𝔩\r\n' } },
          },
        },
      },
    ],
    run: (input) => subagentRunsOf(input.metadata as Record<string, unknown>),
  },
  withSubagentRun: {
    cases: [
      {
        name: 'adds to an empty bag',
        input: { metadata: {}, callId: 'c1', run: RUN },
      },
      {
        name: 'keeps other keys',
        input: {
          metadata: { other: 1, subagent: { parentSessionKey: 'p' } },
          callId: 'c1',
          run: RUN,
        },
      },
      {
        name: 'appends to an existing map',
        input: {
          metadata: { subagentRuns: { c1: RUN } },
          callId: 'c2',
          run: { ...RUN, sessionKey: 'sub-2' },
        },
      },
      {
        name: 'replaces an existing call',
        input: {
          metadata: { subagentRuns: { c1: RUN } },
          callId: 'c1',
          run: { ...RUN, label: 'Renamed' },
        },
      },
      {
        name: 'drops malformed entries on the way through',
        input: {
          metadata: { subagentRuns: { bad: 'x', c1: RUN } },
          callId: 'c2',
          run: RUN,
        },
      },
    ],
    run: (input) =>
      withSubagentRun(
        input.metadata as Record<string, unknown>,
        input.callId as string,
        input.run as SubagentRunRef,
      ),
  },
  defaultSubagentPrompt: {
    cases: [
      { name: 'a label', input: { label: 'Researcher' } },
      { name: 'an empty label', input: { label: '' } },
      { name: 'a label with quotes', input: { label: 'The "Fixer"' } },
      { name: 'an astral-plane label', input: { label: '😀 𝔉𝔦𝔵𝔢𝔯' } },
    ],
    run: (input) => defaultSubagentPrompt(input.label as string),
  },
  applyToolPrompts: {
    cases: [
      {
        name: 'no overrides',
        input: { definitions: [DEFINITION], overrides: {} },
      },
      {
        name: 'replaces a description',
        input: {
          definitions: [DEFINITION],
          overrides: {
            read_file: {
              description: 'Read a file. Prefer this over `cat`.',
              fields: {},
            },
          },
        },
      },
      {
        name: 'empty inherits the built-in description',
        input: {
          definitions: [DEFINITION],
          overrides: { read_file: { description: '', fields: {} } },
        },
      },
      {
        name: 'a single space deletes the description',
        input: {
          definitions: [DEFINITION],
          overrides: { read_file: { description: ' ', fields: {} } },
        },
      },
      {
        name: 'trims a description',
        input: {
          definitions: [DEFINITION],
          overrides: {
            read_file: { description: '  Padded. \r\n', fields: {} },
          },
        },
      },
      {
        name: 'replaces a field description',
        input: {
          definitions: [DEFINITION],
          overrides: {
            read_file: {
              description: '',
              fields: { path: 'Relative to the workspace root.' },
            },
          },
        },
      },
      {
        name: 'adds a description to a field that had none',
        input: {
          definitions: [DEFINITION],
          overrides: {
            read_file: {
              description: '',
              fields: { limit: 'Maximum lines to return.' },
            },
          },
        },
      },
      {
        name: 'reports an unknown tool',
        input: {
          definitions: [DEFINITION],
          overrides: { exec: { description: 'Run a program.', fields: {} } },
        },
      },
      {
        name: 'reports an unknown field',
        input: {
          definitions: [DEFINITION],
          overrides: {
            read_file: { description: '', fields: { pat: 'A typo.' } },
          },
        },
      },
      {
        name: 'reports every field of a tool with no properties',
        input: {
          definitions: [
            {
              ...DEFINITION,
              name: 'ping',
              parameters: { type: 'object', additionalProperties: false },
            },
          ],
          overrides: {
            ping: {
              description: '',
              fields: { host: 'Where to ping.', port: 'Which port.' },
            },
          },
        },
      },
      {
        name: 'reports a field whose property is not an object',
        input: {
          definitions: [
            {
              ...DEFINITION,
              parameters: { type: 'object', properties: { path: 'string' } },
            },
          ],
          overrides: { read_file: { description: '', fields: { path: 'x' } } },
        },
      },
      {
        name: 'leaves other definitions untouched',
        input: {
          definitions: [DEFINITION, { ...DEFINITION, name: 'exec' }],
          overrides: { read_file: { description: 'Changed.', fields: {} } },
        },
      },
      {
        name: 'astral-plane prose',
        input: {
          definitions: [DEFINITION],
          overrides: {
            read_file: {
              description: '😀 Read 𝔞 file.',
              fields: { path: '🚀' },
            },
          },
        },
      },
    ],
    run: (input) =>
      applyToolPrompts(
        input.definitions as ToolDefinition[],
        input.overrides as ToolPromptOverrides,
      ),
  },
  isSequencedServerMessage: {
    cases: [
      {
        name: 'connected',
        input: {
          message: {
            type: 'connected',
            protocolVersion: PROTOCOL_VERSION,
            sessionKey: 's',
            serverTimeMs: 0,
            lastSeq: 0,
          },
        },
      },
      { name: 'pong', input: { message: { type: 'pong', serverTimeMs: 0 } } },
      {
        name: 'error',
        input: { message: { type: 'error', code: 'internal', message: 'x' } },
      },
      {
        name: 'assistant.delta',
        input: {
          message: { type: 'assistant.delta', seq: 1, turnId: 't', text: 'x' },
        },
      },
      {
        name: 'session.status',
        input: {
          message: {
            type: 'session.status',
            seq: 2,
            sessionKey: 's',
            busy: false,
          },
        },
      },
    ],
    run: (input) =>
      isSequencedServerMessage(ServerMessageSchema.parse(input.message)),
  },
};

describe('fixtures/protocol', () => {
  for (const [name, fixture] of Object.entries(FIXTURES)) {
    it(`pins ${name}`, () => {
      const cases = fixture.cases.map(({ name: caseName, input }) => ({
        name: caseName,
        input,
        output: orNull(fixture.run(input)),
      }));
      const names = new Set(cases.map((c) => c.name));
      expect(names.size).toBe(cases.length);
      checkJsonFixture(`protocol/${name}.json`, { function: name, cases });
    });
  }

  it('has one file per function, and no file without a function', () => {
    const files = readdirSync(fixturePath('protocol')).sort();
    expect(files).toEqual(
      Object.keys(FIXTURES)
        .map((name) => `${name}.json`)
        .sort(),
    );
  });

  it('derives only reserved-free ids', () => {
    for (const { input } of FIXTURES.deriveAgentId?.cases ?? []) {
      expect(RESERVED_AGENT_IDS.has(deriveAgentId(input.label as string))).toBe(
        false,
      );
    }
  });
});

// WebSocket frames

const STORED_MESSAGE = {
  id: 'm1',
  sessionKey: 'web:abc',
  seq: 1,
  createdAtMs: 1_700_000_000_000,
  turnId: 't1',
  message: {
    role: 'assistant',
    content: [{ type: 'text', text: 'calling' }],
    toolCalls: [
      { id: 'call_1', name: 'read_file', argumentsJson: '{"path":"a.txt"}' },
    ],
  },
};

const CLIENT_FRAMES: readonly Json[] = [
  { type: 'ping' },
  {
    type: 'user.message',
    sessionKey: 'web:abc',
    content: 'hello 😀',
    attachments: [
      {
        mimeType: 'image/png',
        path: 'uploads/a.png',
        name: 'a.png',
        sizeBytes: 12,
      },
    ],
    agentId: 'coder',
    clientMessageId: 'cm-1',
  },
  {
    type: 'turn.regenerate',
    sessionKey: 'web:abc',
    seq: 4,
    clientMessageId: 'cm-2',
  },
  {
    type: 'user.edit',
    sessionKey: 'web:abc',
    seq: 4,
    content: 'again',
    attachments: [],
    agentId: 'coder',
    clientMessageId: 'cm-3',
  },
  { type: 'turn.stop', sessionKey: 'web:abc' },
  {
    type: 'session.new',
    sessionKey: 'web:new',
    workspaceId: 'acme',
    agentId: 'coder',
  },
  { type: 'session.switch', sessionKey: 'web:abc' },
  { type: 'session.resume', sessionKey: 'web:abc', lastSeq: 41 },
  { type: 'tool.approve', callId: 'c1', approved: true, scope: 'session' },
  { type: 'turn.steer', sessionKey: 'web:abc', content: 'shorter' },
];

const SERVER_FRAMES: readonly Json[] = [
  {
    type: 'connected',
    protocolVersion: PROTOCOL_VERSION,
    sessionKey: 'web:abc',
    serverTimeMs: 1_700_000_000_000,
    lastSeq: 41,
    workspaceId: 'acme',
  },
  { type: 'pong', serverTimeMs: 1_700_000_000_000 },
  {
    type: 'error',
    code: 'session_busy',
    message: 'a turn is running',
    retryable: true,
    turnId: 't1',
  },
  {
    type: 'message.ack',
    seq: 42,
    sessionKey: 'web:abc',
    messageId: 'm7',
    clientMessageId: 'cm-1',
  },
  { type: 'message.queued', seq: 43, sessionKey: 'web:abc', queueDepth: 1 },
  {
    type: 'turn.start',
    seq: 44,
    sessionKey: 'web:abc',
    turnId: 't1',
    firstSeq: 7,
    agentId: 'coder',
    model: 'qwen3',
    provider: 'ollama',
  },
  { type: 'assistant.delta', seq: 45, turnId: 't1', text: 'Hel' },
  { type: 'reasoning.delta', seq: 46, turnId: 't1', text: 'thinking' },
  {
    type: 'tool.call',
    seq: 47,
    turnId: 't1',
    callId: 'c1',
    name: 'exec',
    args: { argv: ['ls'] },
    risk: 'exec',
  },
  {
    type: 'tool.progress',
    seq: 48,
    turnId: 't1',
    callId: 'c1',
    elapsedMs: 15_000,
    message: 'still running',
  },
  {
    type: 'tool.result',
    seq: 49,
    turnId: 't1',
    callId: 'c1',
    ok: true,
    content: 'a.txt',
    truncated: false,
    durationMs: 20,
  },
  {
    type: 'tool.approvalRequest',
    seq: 50,
    turnId: 't1',
    callId: 'c2',
    name: 'exec',
    args: 'not json',
    risk: 'exec',
    expiresAtMs: 1_700_000_300_000,
  },
  {
    type: 'notice',
    seq: 51,
    kind: 'prompt_injection',
    message: 'suspicious content in tool output',
    turnId: 't1',
    callId: 'c1',
  },
  {
    type: 'turn.end',
    seq: 52,
    turnId: 't1',
    stopReason: 'complete',
    usage: {
      promptTokens: 100,
      completionTokens: 250,
      totalTokens: 350,
      cachedTokens: 80,
      reasoningTokens: 10,
    },
    iterations: 2,
    elapsedMs: 10_000,
    generationMs: 1000,
    generationTokens: 250,
    firstTokenMs: 400,
    firstSeq: 7,
    lastSeq: 9,
  },
  {
    type: 'subagent.event',
    seq: 53,
    turnId: 't1',
    parentSessionKey: 'web:abc',
    parentCallId: 'c3',
    agentId: 'researcher',
    label: 'Researcher',
    sessionKey: 'sub-1',
    depth: 1,
    event: { type: 'assistant.delta', turnId: 't1', text: 'inner' },
  },
  {
    type: 'context.usage',
    seq: 54,
    sessionKey: 'web:abc',
    estimatedTokens: 1200,
    contextWindowTokens: 65_536,
    breakdown: { systemPrompt: 400, messages: 800 },
  },
  {
    type: 'session.status',
    seq: 55,
    sessionKey: 'web:abc',
    busy: true,
    queueDepth: 0,
    workspaceId: 'acme',
    turnId: 't1',
  },
  { type: 'session.reset', seq: 56, sessionKey: 'web:abc' },
  {
    type: 'session.replay',
    seq: 57,
    sessionKey: 'web:abc',
    messages: [STORED_MESSAGE],
    complete: false,
    resumingTurnId: 't1',
  },
  {
    type: 'session.truncated',
    seq: 58,
    sessionKey: 'web:abc',
    upToSeq: 3,
    messages: [STORED_MESSAGE],
  },
  {
    type: 'notification',
    seq: 59,
    id: 'n1',
    title: 'Done',
    body: 'The job ran.',
    level: 'success',
    createdAtMs: 1_700_000_000_000,
    sessionKey: 'automation:j1:r1',
    jobId: 'j1',
  },
  {
    type: 'tools.changed',
    seq: 60,
    tools: [
      {
        name: 'read_file',
        description: 'Reads a file.',
        parameters: { type: 'object' },
        risk: 'safe',
        source: 'builtin',
        annotations: { title: 'Read', readOnlyHint: true },
      },
    ],
  },
  { type: 'steer', seq: 61, sessionKey: 'web:abc', content: 'shorter' },
];

describe('fixtures/ws/frames', () => {
  const clientTypes = ClientMessageSchema.options.map(
    (o) => o.shape.type.value,
  );
  const serverTypes = ServerMessageSchema.options.map(
    (o) => o.shape.type.value,
  );

  it('covers every client variant exactly once', () => {
    expect(CLIENT_FRAMES.map((f) => f.type).sort()).toEqual(
      [...clientTypes].sort(),
    );
  });

  it('covers every server variant exactly once', () => {
    expect(SERVER_FRAMES.map((f) => f.type).sort()).toEqual(
      [...serverTypes].sort(),
    );
  });

  for (const frame of CLIENT_FRAMES) {
    it(`pins client ${String(frame.type)}`, () => {
      const parsed: ClientMessage = ClientMessageSchema.parse(frame);
      checkJsonFixture(`ws/frames/${String(frame.type)}.json`, {
        direction: 'client',
        frame: parsed,
      });
    });
  }

  for (const frame of SERVER_FRAMES) {
    it(`pins server ${String(frame.type)}`, () => {
      const parsed: ServerMessage = ServerMessageSchema.parse(frame);
      checkJsonFixture(`ws/frames/${String(frame.type)}.json`, {
        direction: 'server',
        frame: parsed,
      });
    });
  }

  it('has no frame file without a variant', () => {
    const files = readdirSync(fixturePath('ws/frames')).sort();
    expect(files).toEqual(
      [...clientTypes, ...serverTypes].map((type) => `${type}.json`).sort(),
    );
  });
});

// UUIDv7

const UUID_CASES: ReadonlyArray<{
  readonly name: string;
  readonly nowMs: number;
  readonly random: string;
}> = [
  {
    name: 'all-zero randomness shows the version and variant masks',
    nowMs: 0,
    random: '00000000000000000000',
  },
  {
    name: 'all-one randomness is masked, not replaced',
    nowMs: 0,
    random: 'ffffffffffffffffffff',
  },
  {
    name: 'an ordinary instant',
    nowMs: Date.UTC(2026, 7, 1, 12, 34, 56, 789),
    random: '0123456789abcdef0123',
  },
  {
    name: 'the epoch plus one millisecond',
    nowMs: 1,
    random: 'a1b2c3d4e5f60718293a',
  },
  {
    name: 'a 48-bit timestamp',
    nowMs: 2 ** 48 - 1,
    random: '8badf00d8badf00d8bad',
  },
  {
    name: 'a version nibble of 7 already',
    nowMs: 1_700_000_000_000,
    random: '7f00000000bf00000000',
  },
];

describe('fixtures/uuid/v7', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('pins the byte layout', () => {
    const cases = UUID_CASES.map(({ name, nowMs, random }) => {
      vi.useFakeTimers();
      vi.setSystemTime(new Date(nowMs));
      const bytes = Uint8Array.from(random.match(/../g) ?? [], (h) =>
        Number.parseInt(h, 16),
      );
      expect(bytes).toHaveLength(10);
      vi.spyOn(globalThis.crypto, 'getRandomValues').mockImplementation(
        (array) => {
          const out = array as Uint8Array;
          out.fill(0);
          out.set(bytes, 6);
          return array;
        },
      );
      return { name, nowMs, random, uuid: newUuid() };
    });
    checkJsonFixture('uuid/v7.json', { cases });
  });
});
