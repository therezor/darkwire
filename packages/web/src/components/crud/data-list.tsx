/**
 * The list every CRUD screen in the app is made of.
 *
 * This replaced four hand-written `<table>`s — providers, agents, workspaces,
 * files. They shared a stylesheet and nothing else: every `<thead>`/`<tbody>`
 * skeleton was retyped, only two of the four bothered to shed a column on a
 * phone, and Providers put five of them on screen at once with a full URL in
 * the middle. A `<td>` cannot be told to give way; it is sized by its content,
 * so `truncate` on that cell did nothing and the *page* scrolled sideways
 * instead. That is the bug this component exists to make unrepresentable.
 *
 * A row is a card at every width rather than a table row that collapses into
 * one below a breakpoint. Two reasons, and the second is the load-bearing one:
 *
 *  - There is no width at which the collapse could be tested only on the side
 *    it was written for. One layout is one thing to review.
 *  - Turning a `<table>` into a grid *drops the implicit roles* — a `<tr>` that
 *    is `display: grid` stops being a row to a screen reader unless every
 *    element gets an explicit `role` back. A list that is a `<ul>` of `<li>`
 *    from the start owes nothing to a display property.
 *
 * What that costs is column headings, and with them the click-to-sort control
 * that lived in them. Sorting moved to `ListSort` in the toolbar — see that
 * file for why the trade is a fair one. What it means *here* is a rule the
 * screens have to keep: **a meta value has to say what it is.** There is no
 * heading above `12` to explain it any more, so the workspace list renders
 * `12 chats`. Badges and formatted times already read as themselves.
 */

import type { JSX, ReactNode } from 'react';

export function DataList({
  label,
  children,
}: {
  /** Names the list, in place of the caption a table would have had. */
  readonly label: string;
  readonly children: ReactNode;
}): JSX.Element {
  return (
    <ul className="data-list" aria-label={label}>
      {children}
    </ul>
  );
}

export function DataListRow({
  primary,
  meta,
  actions,
}: {
  /** The whole row's open affordance — a link or a button, with its icon. */
  readonly primary: ReactNode;
  /** Everything else the row knows, as a wrapping cluster beneath the name. */
  readonly meta?: ReactNode;
  /** The kebab. Omitted on a row with nothing to do to it. */
  readonly actions?: ReactNode;
}): JSX.Element {
  return (
    <li className="data-list__row">
      <div className="data-list__primary">{primary}</div>
      {meta !== undefined && (
        <div className="cluster data-list__meta">{meta}</div>
      )}
      {actions !== undefined && (
        <div className="data-list__actions">{actions}</div>
      )}
    </li>
  );
}
