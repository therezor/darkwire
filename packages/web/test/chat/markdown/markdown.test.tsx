/**
 * The renderer, with the safety rules first.
 *
 * Every character it is given came from a model, or from a tool result a model
 * read. So the assertions that matter most are not "a heading is an `h1`" but
 * "a `javascript:` link is not a link" and "a tracker's image is not loaded" —
 * the two places where rendering untrusted markdown turns into executing it or
 * leaking on behalf of it.
 */

import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { resetHighlighter } from '@/chat/markdown/highlight.js';
import { Markdown } from '@/chat/markdown/markdown.js';

afterEach(() => {
  resetHighlighter();
});

describe('untrusted markdown', () => {
  it('renders a javascript: link as its own text, hiding nothing', () => {
    render(<Markdown text="[click me](javascript:alert(1))" />);

    expect(screen.getByText('click me')).toBeInTheDocument();
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
  });

  it('opens a real link safely', () => {
    render(<Markdown text="[docs](https://example.com/a)" />);

    const link = screen.getByRole('link', { name: 'docs' });
    expect(link).toHaveAttribute('href', 'https://example.com/a');
    // `noopener` stops the opened page reaching back through `window.opener`;
    // `noreferrer` is the privacy half.
    expect(link.getAttribute('rel')).toContain('noopener');
    expect(link.getAttribute('rel')).toContain('noreferrer');
  });

  it('does not load an off-origin image, and offers it as a link instead', () => {
    render(<Markdown text="![a chart](https://tracker.example/pixel.gif)" />);

    expect(screen.queryByRole('img')).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'a chart' })).toBeInTheDocument();
  });

  it('loads a same-origin image, which is what an upload is', () => {
    render(<Markdown text="![screenshot](/api/media/abc)" />);

    expect(screen.getByRole('img', { name: 'screenshot' })).toHaveAttribute(
      'src',
      `${globalThis.location.origin}/api/media/abc`,
    );
  });

  it('shows raw HTML as source rather than mounting it', () => {
    const { container } = render(
      <Markdown text={'<img src=x onerror="alert(1)">'} />,
    );

    expect(container.querySelector('img')).toBeNull();
    expect(screen.getByText(/onerror/)).toBeInTheDocument();
  });
});

describe('the block renderer', () => {
  it('renders the constructs a model actually emits', () => {
    render(
      <Markdown
        text={[
          '# Title',
          '',
          'Some **bold** and `code` and ~~gone~~.',
          '',
          '> a quote',
          '',
          '1. first',
          '2. second',
          '',
          '| a | b |',
          '| - | - |',
          '| 1 | 2 |',
          '',
          '---',
        ].join('\n')}
      />,
    );

    expect(
      screen.getByRole('heading', { level: 1, name: 'Title' }),
    ).toBeInTheDocument();
    expect(screen.getByText('bold').tagName).toBe('STRONG');
    expect(screen.getByText('code').tagName).toBe('CODE');
    expect(screen.getByText('gone').tagName).toBe('DEL');
    expect(screen.getByText('a quote')).toBeInTheDocument();
    expect(screen.getAllByRole('listitem')).toHaveLength(2);
    expect(screen.getByRole('table')).toBeInTheDocument();
    expect(screen.getByRole('separator')).toBeInTheDocument();
  });

  it('draws a task list as checkboxes nobody can press', () => {
    render(<Markdown text={'- [x] done\n- [ ] not done'} />);

    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.map((box) => (box as HTMLInputElement).checked)).toEqual([
      true,
      false,
    ]);
    // The model's rendering of its own plan, not a control: there is nothing a
    // press could change.
    expect(boxes[0]).toHaveAttribute('readonly');
    expect(boxes[0]).toHaveAttribute('tabindex', '-1');
  });

  it('caps a heading at six levels rather than emitting an h9', () => {
    render(<Markdown text="######### too deep" />);

    // marked lexes this as a paragraph, which is what CommonMark says; the
    // clamp exists for the depths it does produce.
    expect(screen.getByText(/too deep/)).toBeInTheDocument();
  });
});

describe('a code block', () => {
  it('is readable before it is coloured', () => {
    render(<Markdown text={'```ts\nconst a = 1;\n```'} />);

    // Nothing here ever renders a spinner in place of code.
    expect(screen.getByText('const a = 1;')).toBeInTheDocument();
    expect(screen.getByText('ts')).toBeInTheDocument();
  });

  it('is highlighted once the fence has closed', async () => {
    const { container } = render(
      <Markdown text={'```ts\nconst a = 1;\n```'} />,
    );

    await waitFor(() => {
      // Colour arrives as spans with an inline colour from the theme; before
      // it, the `<code>` holds one text node.
      expect(
        container.querySelectorAll('code span[style]').length,
      ).toBeGreaterThan(0);
    });
  });

  it('is not highlighted while the fence is still being written', async () => {
    const { container } = render(
      <Markdown text={'```ts\nconst a = 1;'} streaming />,
    );

    // Re-tokenising a half-written string flickers between two colourings on
    // every delta, which is worse than no colour at all.
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(container.querySelectorAll('code span[style]')).toHaveLength(0);
  });

  it('labels a fence with no language rather than guessing at one', () => {
    render(<Markdown text={'```\nplain\n```'} />);

    expect(screen.getByText('text')).toBeInTheDocument();
  });
});
