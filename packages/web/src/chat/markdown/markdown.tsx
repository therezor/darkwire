/**
 * Markdown → React elements. Never HTML.
 *
 * Every character here came from a model, or from a tool result a model read,
 * and the shortest route from "an agent fetched a web page" to "the page ran
 * something" is a renderer that hands a string to `dangerouslySetInnerHTML`.
 * `marked` is used as a *lexer* only; the tokens are walked into elements, so
 * there is no HTML string anywhere in this file and nothing to sanitise after
 * the fact. Raw `html` tokens render as their own source text, which is both the
 * safe behaviour and — for a model that wrote `<Component />` inside prose —
 * the useful one.
 *
 * The memoisation is the other half of the design, and `blocks.ts` explains it:
 * one `React.memo` per top-level block, compared on the block's own source text.
 * A paragraph that finished ten seconds ago is the same string this frame as
 * last, so React never walks it again, and the selection and scroll position
 * inside it survive the rest of the answer arriving.
 */

import { memo, useMemo, type JSX, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import type { Token, Tokens } from 'marked';

import { cn } from '@/lib/cn.js';
import { safeHref, isSameOrigin } from '@/lib/url.js';
import { fenceLanguage, inlineTokens, splitBlocks } from './blocks.js';
import { CodeBlock } from './code-block.js';

interface MarkdownProps {
  readonly text: string;
  /**
   * True while deltas are still arriving, which makes the last block live.
   * Only the trailing block is affected: everything before it is final whether
   * the answer has finished or not.
   */
  readonly streaming?: boolean;
  readonly className?: string;
}

export function Markdown({
  text,
  streaming = false,
  className,
}: MarkdownProps): JSX.Element {
  const blocks = useMemo(() => splitBlocks(text), [text]);

  return (
    <div className={cn('markdown', className)}>
      {blocks.map((block, index) => (
        <MemoBlock
          key={block.key}
          token={block.token}
          raw={block.raw}
          complete={!streaming || index < blocks.length - 1}
        />
      ))}
    </div>
  );
}

interface BlockProps {
  readonly token: Token;
  /** The comparator's input. Present as a prop so `memo` can see it. */
  readonly raw: string;
  readonly complete: boolean;
}

/**
 * The comparator is the whole point.
 *
 * `token` is a fresh object on every lex, so the default shallow compare would
 * never skip anything. `raw` is the block's source text: if it has not changed,
 * neither has anything the block renders.
 */
const MemoBlock = memo(
  ({ token, complete }: BlockProps): ReactNode => {
    return renderBlock(token, complete);
  },
  (previous, next) =>
    previous.raw === next.raw && previous.complete === next.complete,
);

function renderBlock(token: Token, complete: boolean): ReactNode {
  switch (token.type) {
    case 'heading':
      return renderHeading(token as Tokens.Heading);

    case 'paragraph':
      return <p>{renderInline(inlineTokens(token))}</p>;

    case 'code': {
      const code = token as Tokens.Code;
      return (
        <CodeBlock
          code={code.text}
          lang={fenceLanguage(code)}
          // A fence whose closing backticks have not arrived is still growing,
          // and highlighting a half-written string produces a block that
          // flickers between two colourings on every delta.
          complete={complete && code.raw.trimEnd().endsWith('```')}
        />
      );
    }

    case 'blockquote':
      return (
        <blockquote>
          {(token as Tokens.Blockquote).tokens.map((child, index) => (
            <MemoBlock
              key={index}
              token={child}
              raw={child.raw}
              complete={complete}
            />
          ))}
        </blockquote>
      );

    case 'list':
      return renderList(token as Tokens.List, complete);

    case 'table':
      return renderTable(token as Tokens.Table);

    case 'hr':
      return <hr />;

    // A model writing `<div>` in prose meant to show the tag, not to open one.
    // Rendering the source is both the safe answer and the intended one.
    case 'html':
      return <p className="markdown__html">{(token as Tokens.HTML).raw}</p>;

    // A stray inline run at block level — the tail of a list item, usually.
    case 'text':
      return <p>{renderInline(inlineTokens(token))}</p>;

    // Link reference definitions produce no output; `space` is filtered in
    // `splitBlocks`. Anything else is a token type marked added since.
    default:
      return null;
  }
}

function renderHeading(token: Tokens.Heading): ReactNode {
  const depth = Math.min(Math.max(token.depth, 1), 6);
  const Tag = `h${String(depth)}` as 'h1';

  return <Tag>{renderInline(inlineTokens(token))}</Tag>;
}

function renderList(token: Tokens.List, complete: boolean): ReactNode {
  const Tag = token.ordered ? 'ol' : 'ul';
  const start = typeof token.start === 'number' ? token.start : undefined;
  // A `data-` attribute rather than a class: the markdown stylesheet addresses
  // these elements by tag, and this is the one distinction the tag cannot make.
  const task = token.items.some((item) => item.task);

  return (
    <Tag
      {...(task ? { 'data-task': 'true' as const } : {})}
      {...(start !== undefined && start !== 1 ? { start } : {})}
    >
      {token.items.map((item, index) => (
        <li
          key={index}
          {...(item.task ? { 'data-task': 'true' as const } : {})}
        >
          {item.task && (
            <input
              type="checkbox"
              checked={item.checked === true}
              readOnly
              // The model's rendering of its own plan, not a control: it is
              // announced but never focusable, because there is nothing a press
              // could change.
              tabIndex={-1}
              aria-label={item.checked === true ? 'Done' : 'Not done'}
            />
          )}
          <div>
            {item.tokens.map((child, childIndex) => (
              <MemoBlock
                key={childIndex}
                token={child}
                raw={child.raw}
                complete={complete}
              />
            ))}
          </div>
        </li>
      ))}
    </Tag>
  );
}

function renderTable(token: Tokens.Table): ReactNode {
  const align = (
    index: number,
  ): { readonly 'data-align'?: 'left' | 'center' | 'right' } => {
    const value = token.align[index];
    return value === null || value === undefined ? {} : { 'data-align': value };
  };

  return (
    <div className="markdown__table-scroll">
      <table>
        <thead>
          <tr>
            {token.header.map((cell, index) => (
              <th key={index} {...align(index)}>
                {renderInline(cell.tokens)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {token.rows.map((row, rowIndex) => (
            <tr key={rowIndex}>
              {row.map((cell, index) => (
                <td key={index} {...align(index)}>
                  {renderInline(cell.tokens)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// Inline

function renderInline(tokens: readonly Token[]): ReactNode {
  return tokens.map((token, index) => <Inline key={index} token={token} />);
}

function Inline({ token }: { readonly token: Token }): ReactNode {
  switch (token.type) {
    case 'text':
      // `text` carries nested tokens only inside GFM constructs; otherwise the
      // decoded entity text is the whole content.
      return inlineTokens(token).length > 0 ? (
        renderInline(inlineTokens(token))
      ) : (
        <>{(token as Tokens.Text).text}</>
      );

    case 'escape':
      return <>{(token as Tokens.Escape).text}</>;

    case 'strong':
      return <strong>{renderInline(inlineTokens(token))}</strong>;

    case 'em':
      return <em>{renderInline(inlineTokens(token))}</em>;

    case 'del':
      return <del>{renderInline(inlineTokens(token))}</del>;

    case 'codespan':
      return <code>{(token as Tokens.Codespan).text}</code>;

    case 'br':
      return <br />;

    case 'link':
      return <InlineLink token={token as Tokens.Link} />;

    case 'image':
      return <InlineImage token={token as Tokens.Image} />;

    // The raw source, for the same reason the block case renders it.
    case 'html':
      return <>{(token as Tokens.Tag).raw}</>;

    default:
      return <>{'raw' in token ? (token as { raw: string }).raw : null}</>;
  }
}

function InlineLink({ token }: { readonly token: Tokens.Link }): ReactNode {
  const href = safeHref(token.href);
  const children = renderInline(inlineTokens(token));

  // A scheme we will not link renders as text — including the URL itself, so
  // nothing is concealed from the reader. It simply is not one click away.
  if (href === undefined) return <>{children}</>;

  return (
    <a
      href={href}
      // Model output is untrusted, and `noopener` is what stops the opened page
      // reaching back through `window.opener`. `noreferrer` is the privacy half.
      target="_blank"
      rel="noopener noreferrer nofollow"
      {...(token.title === null || token.title === undefined
        ? {}
        : { title: token.title })}
    >
      {children}
    </a>
  );
}

function InlineImage({ token }: { readonly token: Tokens.Image }): ReactNode {
  const { t } = useTranslation();
  const href = safeHref(token.href);
  if (href === undefined) return <>{token.text}</>;

  // Off-origin images are a link, not an `<img>`. Loading one would tell
  // whoever wrote the markdown — which, in a tool result, is a web page — the
  // reader's IP and the moment they read it, on a product whose whole premise
  // is that it runs on your own machine.
  if (!isSameOrigin(href)) {
    return (
      <a
        href={href}
        target="_blank"
        rel="noopener noreferrer nofollow"
        title={t('chat.externalImage')}
      >
        {token.text === '' ? href : token.text}
      </a>
    );
  }

  return <img src={href} alt={token.text} loading="lazy" />;
}
