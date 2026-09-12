/**
 * The two recipes.
 *
 * What is worth testing about a `cva` call is not that it returns strings — it
 * is the rules the strings encode: that a caller's `className` can win over the
 * recipe's, that every role/variant pairing actually resolves to a rule, and
 * that a button is not a submit button by accident.
 *
 * The first of those changed shape when the utility framework went away, and
 * the change is worth stating. A caller's class used to have to *replace* the
 * recipe's, because two conflicting utilities in one attribute are resolved by
 * whichever the framework happened to emit later — so `cn` re-implemented the
 * framework's conflict groups to drop the loser. Now a component's rules and a
 * caller's rules are separate selectors in separate cascade layers, and
 * `app.css` states which layer wins. So the assertion moved: both classes are
 * present, and the layer order is what decides. That is checked here too,
 * because it is the mechanism the escape hatch now rests on.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { Badge, badgeVariants } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { STYLES } from '@testkit/paths.js';

const TONES = [
  'neutral',
  'accent',
  'success',
  'warning',
  'danger',
  'info',
] as const;
const VARIANTS = ['soft', 'solid', 'outline'] as const;

const read = (file: string): string => readFileSync(join(STYLES, file), 'utf8');

describe('Button', () => {
  it('defaults to type=button, so it cannot submit a form by accident', () => {
    render(<Button>Save</Button>);
    expect(screen.getByRole('button', { name: 'Save' })).toHaveAttribute(
      'type',
      'button',
    );
  });

  it('still submits when asked to', () => {
    render(<Button type="submit">Save</Button>);
    expect(screen.getByRole('button', { name: 'Save' })).toHaveAttribute(
      'type',
      'submit',
    );
  });

  it("keeps a caller's class beside the recipe's, for the cascade to resolve", () => {
    render(<Button className="transcript__jump">Jump</Button>);

    const className = screen.getByRole('button').className;
    expect(className).toContain('btn');
    expect(className).toContain('transcript__jump');
  });

  it('is overridable because the layer order says so, not because a class was dropped', () => {
    const app = read('app.css');
    const order = /@layer\s+([^;]+);/.exec(app)?.[1] ?? '';
    const layers = order.split(',').map((name) => name.trim());

    // A screen rule beats a component rule without having to be written more
    // specifically. If these ever swap, every `className` passed to a primitive
    // silently stops taking effect.
    expect(layers).toContain('components');
    expect(layers).toContain('screens');
    expect(layers.indexOf('screens')).toBeGreaterThan(
      layers.indexOf('components'),
    );
  });

  it('renders as its child, keeping the classes, so a link stays a link', () => {
    render(
      <Button asChild variant="primary">
        <a href="/somewhere">Go</a>
      </Button>,
    );

    const link = screen.getByRole('link', { name: 'Go' });
    expect(link).toHaveClass('btn', 'btn--primary');
    expect(link).not.toHaveAttribute('type');
  });

  /**
   * The bug this exists for: `disabled` used to be `opacity-70` over a muted
   * foreground, which on an accent or danger fill is a label nobody can read.
   * A disabled control is a *pairing* now — a measured surface and a measured
   * foreground — so no variant can produce an illegible one.
   */
  it('never expresses disabled as opacity', () => {
    const button = read('components/button.css');
    const disabledRules = button.slice(button.indexOf('.btn:disabled'));

    expect(disabledRules).not.toMatch(/opacity:\s*0?\.\d/);
    expect(disabledRules).toContain('var(--fg-3)');
  });

  it('is reachable and operable from the keyboard', async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    render(<Button onClick={onClick}>Press</Button>);

    await user.tab();
    expect(screen.getByRole('button')).toHaveFocus();

    await user.keyboard('{Enter}');
    await user.keyboard(' ');
    expect(onClick).toHaveBeenCalledTimes(2);
  });

  it('does not fire while disabled', async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    render(
      <Button disabled onClick={onClick}>
        Press
      </Button>,
    );

    await user.click(screen.getByRole('button'));
    expect(onClick).not.toHaveBeenCalled();
  });
});

describe('Badge', () => {
  const badgeCss = read('components/badge.css');

  it('covers every role and variant pairing', () => {
    // A pairing with no rule behind it is an unstyled pill — visually a plain
    // word, which is precisely the kind of gap a screenshot review skims past.
    // Tone and variant compose in CSS now, so "the pairing exists" means both
    // halves have a rule rather than that a compound entry was written.
    for (const tone of TONES) {
      for (const variant of VARIANTS) {
        const classes = badgeVariants({ tone, variant });
        expect(classes, `${tone}/${variant}`).toContain(`badge--${tone}`);
        expect(classes, `${tone}/${variant}`).toContain(`badge--${variant}`);
        expect(badgeCss, `${tone} has no rule`).toContain(`.badge--${tone} {`);
        expect(badgeCss, `${variant} has no rule`).toContain(
          `.badge--${variant} {`,
        );
      }
    }
  });

  it('never colours text with the fill token', () => {
    // The rule the third gate enforces, asserted from the other direction. A
    // tone declares four properties; the two that end up in a text position
    // must resolve to a `-fg` token, never to the bare fill.
    for (const property of ['--tone-fg', '--tone-solid-fg']) {
      for (const [, value] of badgeCss.matchAll(
        new RegExp(String.raw`${property}:\s*var\((--[\w-]+)\)`, 'g'),
      )) {
        expect(value, `${property} is a text position`).toMatch(
          /-fg$|^--fg-|^--on-fill$/,
        );
      }
    }
  });

  it('is soft and neutral by default, because a badge annotates', () => {
    render(<Badge>queued</Badge>);
    expect(screen.getByText('queued')).toHaveClass(
      'badge',
      'badge--neutral',
      'badge--soft',
    );
  });
});
