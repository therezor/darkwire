/**
 * The living style guide: every token at the size it is used, and every
 * primitive beside it.
 *
 * It started as a swatch grid and kept its job — a seed change is something you
 * can look at rather than something you discover in the chat view later — and
 * the primitives joined it afterwards. That is what makes
 * "reviewed in both themes" a five-second check on one page rather than a tour
 * of the application, and it is why every recipe is exercised here in every
 * variant instead of only in the two places the shell happens to use.
 *
 * It is also the widest gate target in the package: every token is instantiated
 * here by name, so a token that was renamed or removed without its uses being
 * updated shows up on this page before it shows up in the app.
 */

import type { CSSProperties, JSX, ReactNode } from 'react';

import type { NoticeKind } from '@ghostwire/protocol';

import { Notice } from '@/chat/notice.js';
import { ToolCard } from '@/chat/tool-card.js';
import type { ToolPart } from '@/state/transcript.js';
import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogHeading,
  DialogSubheading,
  DialogTrigger,
} from '@/components/ui/dialog.js';
import { Field } from '@/components/ui/field.js';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select.js';
import { Switch } from '@/components/ui/switch.js';
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs.js';
import { Tooltip } from '@/components/ui/tooltip.js';
import { toast } from '@/components/ui/toast.js';

const SURFACES = ['surface-0', 'surface-1', 'surface-2', 'surface-3'] as const;
const TEXT = ['fg-1', 'fg-2', 'fg-3'] as const;
const OVERLAYS = ['hover', 'line', 'line-strong'] as const;
const ROLES = ['accent', 'success', 'warning', 'danger', 'info'] as const;
const SIZES = ['2xs', 'sm', 'base', 'md', 'lg', 'xl'] as const;
const RADII = ['sm', 'md', 'full'] as const;
const ROLE_VARIANTS = ['soft', 'solid', 'outline'] as const;
const BUTTON_VARIANTS = [
  'primary',
  'secondary',
  'ghost',
  'danger',
  'link',
] as const;

/**
 * A custom property, as an inline style.
 *
 * The one place in the package where a style is written in TSX rather than in a
 * stylesheet, and it is unavoidable: this page's subject *is* the list of token
 * names, so the mapping has to be a loop over data. The stylesheet still owns
 * every rule — these only hand it which token to read.
 */
function swatch(values: Readonly<Record<string, string>>): CSSProperties {
  return values;
}

export function TokensRoute(): JSX.Element {
  return (
    <div className="stack style-guide">
      <header className="style-guide__header">
        <h1 className="style-guide__title">Tokens and primitives</h1>
        <p className="style-guide__lede">
          The living style guide. Flip the theme in the header and everything
          here has to hold — which is what makes reviewing in both themes
          something you can actually do.
        </p>
      </header>

      <Section
        title="Surfaces"
        hint="An even OKLCH ramp: page, sunken, card, elevated."
      >
        <div className="style-guide__grid">
          {SURFACES.map((name) => (
            <div
              key={name}
              className="style-guide__surface"
              style={swatch({ '--swatch': `var(--${name})` })}
            >
              <Label>{name}</Label>
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Text"
        hint="Every tier meets WCAG AA on every surface, in both themes."
      >
        <div className="style-guide__grid--single">
          {SURFACES.map((surface) => (
            <div
              key={surface}
              className="stack style-guide__text-card"
              style={swatch({ '--swatch': `var(--${surface})` })}
            >
              {TEXT.map((tier) => (
                <p
                  key={tier}
                  className="style-guide__text-line"
                  style={swatch({ '--swatch-fg': `var(--${tier})` })}
                >
                  {tier} on {surface}
                </p>
              ))}
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Overlays"
        hint="Alpha fills, not ramp stops — they lighten in dark and darken in light."
      >
        <div className="style-guide__grid">
          {OVERLAYS.map((name) => (
            <div key={name} className="style-guide__overlay-card">
              <div
                className="style-guide__overlay-chip"
                style={swatch({ '--swatch': `var(--${name})` })}
              />
              <Label>{name}</Label>
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Roles"
        hint="Fill, text, and the soft fill behind a badge. Text never uses the fill token."
      >
        <div className="stack">
          {ROLES.map((role) => (
            <div key={role} className="cluster style-guide__role-row">
              <span
                className="style-guide__role-fill"
                style={swatch({ '--swatch': `var(--${role})` })}
              >
                {role}
              </span>
              <span
                className="style-guide__role-soft"
                style={swatch({
                  '--swatch': `var(--${role}-soft)`,
                  '--swatch-fg': `var(--${role}-fg)`,
                })}
              >
                {role}-soft
              </span>
              <span
                className="style-guide__role-text"
                style={swatch({ '--swatch-fg': `var(--${role}-fg)` })}
              >
                {role}-fg on the page
              </span>
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Type"
        hint="rem throughout; the root font size is never overridden."
      >
        <div className="stack style-guide__panel">
          {SIZES.map((size) => (
            <p
              key={size}
              className="style-guide__type-line"
              style={swatch({
                '--swatch-size': `var(--text-${size})`,
                '--swatch-leading': `var(--leading-${size})`,
              })}
            >
              <span className="style-guide__type-name">text-{size}</span> — The
              quick brown fox jumps over the lazy dog
            </p>
          ))}
          <p className="style-guide__mono-line">
            font-mono — const nonce = randomUUID(); // 0O1lI!=
          </p>
        </div>
      </Section>

      <Section
        title="Radius"
        hint="Two stops and a pill. Elevation is a surface and a stroke, not a shadow — there is no shadow scale."
      >
        <div className="cluster style-guide__chips">
          {RADII.map((radius) => (
            <div key={radius} className="stack style-guide__chip">
              <div
                className="style-guide__radius-box"
                style={swatch({ '--swatch-radius': `var(--radius-${radius})` })}
              />
              <Label>{radius}</Label>
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Buttons"
        hint="One recipe. Tab through them — the ring is the base layer's, not each button's. The disabled row is the one to check: it is a measured pairing, not a faded one."
      >
        <div className="stack">
          {BUTTON_VARIANTS.map((variant) => (
            <div key={variant} className="cluster style-guide__role-row">
              <Button variant={variant} size="sm">
                {variant} sm
              </Button>
              <Button variant={variant}>{variant} md</Button>
              <Button variant={variant} disabled>
                disabled
              </Button>
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Badges"
        hint="One recipe parameterised on role — not twenty-five pills."
      >
        <div className="stack">
          {ROLE_VARIANTS.map((variant) => (
            <div key={variant} className="cluster style-guide__stack">
              <Label>{variant}</Label>
              <Badge variant={variant}>neutral</Badge>
              {ROLES.map((role) => (
                <Badge key={role} tone={role} variant={variant}>
                  {role}
                </Badge>
              ))}
            </div>
          ))}
        </div>
      </Section>

      <Section
        title="Interaction"
        hint="Radix behaviour, our classes. Every one of these is keyboard-operable."
      >
        <div className="cluster style-guide__interaction">
          <Dialog>
            <DialogTrigger asChild>
              <Button variant="secondary">Open dialog</Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogHeading>A modal dialog</DialogHeading>
                <DialogSubheading>
                  Focus is trapped here, Escape closes it, and focus returns to
                  the button that opened it.
                </DialogSubheading>
              </DialogHeader>
              <Field
                label="Something to focus"
                placeholder="Tab cycles inside the dialog"
              />
              <DialogFooter>
                <Button variant="ghost">Cancel</Button>
                <Button variant="primary">Confirm</Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>

          <Tooltip label="Tooltips label; they never explain">
            <Button variant="ghost">Hover or focus me</Button>
          </Tooltip>

          <Button
            variant="secondary"
            onClick={() => {
              toast.success('Saved', 'Toasts announce without stealing focus.');
            }}
          >
            Raise a toast
          </Button>

          <label className="style-guide__switch-label">
            <Switch defaultChecked />
            Applies immediately
          </label>

          <Select defaultValue="anthropic">
            <SelectTrigger className="style-guide__select">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="anthropic">Anthropic</SelectItem>
              <SelectItem value="ollama">Ollama</SelectItem>
              <SelectItem value="openai-compatible">
                OpenAI-compatible
              </SelectItem>
            </SelectContent>
          </Select>
        </div>

        <Tabs defaultValue="one">
          <TabsList>
            <TabsTrigger value="one">First</TabsTrigger>
            <TabsTrigger value="two">Second</TabsTrigger>
            <TabsTrigger value="three" disabled>
              Disabled
            </TabsTrigger>
          </TabsList>
          <TabsContent value="one" className="style-guide__tab-body">
            Arrow keys move between tabs; the panel follows.
          </TabsContent>
          <TabsContent value="two" className="style-guide__tab-body">
            The active tab is a surface change and a text tier, never colour
            alone.
          </TabsContent>
        </Tabs>

        <div className="stack style-guide__fields">
          <Field
            label="Text field"
            placeholder="Label, input and message are wired together"
          />
          <Field
            label="Invalid field"
            defaultValue="not-an-email"
            error="That is not valid."
          />
        </div>
      </Section>

      <Section
        title="Notices"
        hint="Five kinds, two meanings: two describe a refusal, three describe a degradation."
      >
        <div className="stack style-guide__stack">
          {NOTICES.map(([kind, message]) => (
            <Notice key={kind} kind={kind} message={message} />
          ))}
        </div>
      </Section>

      <Section
        title="Tool cards"
        hint="Every status a call passes through, including the one where it is waiting on you."
      >
        <div className="stack style-guide__stack">
          {TOOL_CARDS.map((tool) => (
            <ToolCard
              key={tool.id}
              tool={tool}
              onApprove={(callId, approved) => {
                toast(
                  approved
                    ? { title: 'Approved', role: 'success' }
                    : { title: 'Denied', role: 'warning' },
                );
              }}
            />
          ))}
        </div>
      </Section>
    </div>
  );
}

const NOTICES: ReadonlyArray<readonly [NoticeKind, string]> = [
  ['prompt_injection', 'The fetched page contained instruction_override text.'],
  ['approval_denied', 'exec was refused by the operator.'],
  ['degraded', 'Dropped images to fit the provider’s request limit.'],
  ['truncated_history', 'Trimmed 12 older messages to fit the context window.'],
  [
    'provider_fallback',
    'The streaming request failed; retried without streaming.',
  ],
];

/**
 * One card per status, because the states differ by more than a colour: the
 * approval prompt is a different shape, and a failure is the one where the
 * output is the point.
 */
const TOOL_CARDS: readonly ToolPart[] = [
  {
    kind: 'tool',
    id: 'guide-running',
    name: 'exec',
    args: { command: 'pnpm test' },
    risk: 'exec',
    status: 'running',
    elapsedMs: 42_000,
    durationMs: undefined,
    content: undefined,
    truncated: false,
    approval: undefined,
    notices: [],
    subagent: undefined,
  },
  {
    kind: 'tool',
    id: 'guide-approval',
    name: 'exec',
    args: { command: 'rm -rf build' },
    risk: 'exec',
    status: 'awaiting-approval',
    elapsedMs: 0,
    durationMs: undefined,
    content: undefined,
    truncated: false,
    // Far enough out that the countdown is not the thing being reviewed.
    approval: { expiresAtMs: Date.now() + 3_600_000, answered: undefined },
    notices: [],
    subagent: undefined,
  },
  {
    kind: 'tool',
    id: 'guide-ok',
    name: 'read_file',
    args: { path: 'package.json' },
    risk: 'safe',
    status: 'ok',
    elapsedMs: 0,
    durationMs: 8,
    content: '{\n  "name": "@ghostwire/web"\n}',
    truncated: true,
    approval: undefined,
    notices: [],
    subagent: undefined,
  },
  {
    kind: 'tool',
    id: 'guide-error',
    name: 'fetch',
    // Loopback rather than a real host: `self-contained.test.ts` sweeps the
    // shipped source for off-origin URLs and does not care that this one is a
    // string in a style guide.
    args: { url: 'http://localhost:8080/health' },
    risk: 'network',
    status: 'error',
    elapsedMs: 0,
    durationMs: 1_400,
    content: 'ECONNREFUSED',
    truncated: false,
    approval: undefined,
    notices: [
      {
        kind: 'notice',
        id: 'guide-notice',
        notice: 'prompt_injection',
        message: 'The response body looked like an instruction.',
      },
    ],
    subagent: undefined,
  },
];

function Section({
  title,
  hint,
  children,
}: {
  readonly title: string;
  readonly hint?: string;
  readonly children: ReactNode;
}): JSX.Element {
  return (
    <section className="stack style-guide__section">
      <div>
        <h2 className="style-guide__section-title">{title}</h2>
        {hint !== undefined && <p className="style-guide__hint">{hint}</p>}
      </div>
      {children}
    </section>
  );
}

function Label({ children }: { readonly children: ReactNode }): JSX.Element {
  return <span className="style-guide__label">{children}</span>;
}
