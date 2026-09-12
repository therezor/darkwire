/**
 * The pieces of list furniture, tested where their state can be held still.
 *
 * These are the behaviours that are cheap to assert here and expensive to
 * assert anywhere else: that a close arriving by `Escape` looks the same to the
 * caller as a press of Cancel, that a one-field dialog does not open holding
 * the last thing typed into it, and that the kebab announces what row it
 * belongs to. Every one of them shipped wrong in at least one of the four
 * hand-written copies these components replace.
 */

import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { JSX } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { renderWithProviders } from '@testkit/render.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { NameDialog } from '@/components/crud/name-dialog.js';
import { Pagination, pageItems } from '@/components/crud/pagination.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { usePagination } from '@/components/crud/use-pagination.js';

describe('ConfirmDialog', () => {
  const props = {
    open: true,
    title: 'Delete this file?',
    description: 'notes.md is removed from the workspace. There is no undo.',
    confirmLabel: 'Delete',
  };

  it('asks before it acts, and does nothing until the answer is yes', async () => {
    const onConfirm = vi.fn();
    const onOpenChange = vi.fn();
    renderWithProviders(
      <ConfirmDialog
        {...props}
        onConfirm={onConfirm}
        onOpenChange={onOpenChange}
      />,
    );

    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));

    expect(onConfirm).not.toHaveBeenCalled();
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it('reports Escape as the same close a press of Cancel is', async () => {
    // The reason `onOpenChange` is on the outside: the unsaved-edits guard has
    // to see a close that never went through a button.
    const onOpenChange = vi.fn();
    renderWithProviders(
      <ConfirmDialog
        {...props}
        onConfirm={vi.fn()}
        onOpenChange={onOpenChange}
      />,
    );

    await userEvent.keyboard('{Escape}');

    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it('confirms once and leaves closing to the caller', async () => {
    const onConfirm = vi.fn();
    const onOpenChange = vi.fn();
    renderWithProviders(
      <ConfirmDialog
        {...props}
        onConfirm={onConfirm}
        onOpenChange={onOpenChange}
      />,
    );

    await userEvent.click(screen.getByRole('button', { name: 'Delete' }));

    expect(onConfirm).toHaveBeenCalledTimes(1);
    // A delete that fails should leave the question on screen with its error.
    expect(onOpenChange).not.toHaveBeenCalled();
  });

  it('shows what turns the question into a decision', () => {
    renderWithProviders(
      <ConfirmDialog {...props} onConfirm={vi.fn()} onOpenChange={vi.fn()}>
        <p className="notice notice--danger">
          Everything inside goes with it — 47 items.
        </p>
      </ConfirmDialog>,
    );

    expect(screen.getByText(/47 items/)).toBeInTheDocument();
  });

  it('does not offer the confirm while the action is in flight', () => {
    renderWithProviders(
      <ConfirmDialog
        {...props}
        pending
        onConfirm={vi.fn()}
        onOpenChange={vi.fn()}
      />,
    );

    expect(screen.getByRole('button', { name: 'Delete' })).toBeDisabled();
  });
});

describe('NameDialog', () => {
  const props = {
    open: true,
    title: 'New folder',
    fieldLabel: 'Folder name',
  };

  it('submits on Enter, because that is the only thing a one-box dialog is for', async () => {
    const onSubmit = vi.fn();
    renderWithProviders(
      <NameDialog {...props} onSubmit={onSubmit} onOpenChange={vi.fn()} />,
    );

    await userEvent.type(screen.getByLabelText('Folder name'), 'drafts{Enter}');

    expect(onSubmit).toHaveBeenCalledWith('drafts');
  });

  it('trims what it hands back', async () => {
    const onSubmit = vi.fn();
    renderWithProviders(
      <NameDialog {...props} onSubmit={onSubmit} onOpenChange={vi.fn()} />,
    );

    await userEvent.type(
      screen.getByLabelText('Folder name'),
      '  drafts  {Enter}',
    );

    expect(onSubmit).toHaveBeenCalledWith('drafts');
  });

  it('refuses a blank name without asking the server', async () => {
    const onSubmit = vi.fn();
    renderWithProviders(
      <NameDialog {...props} onSubmit={onSubmit} onOpenChange={vi.fn()} />,
    );

    expect(screen.getByRole('button', { name: 'Create' })).toBeDisabled();

    await userEvent.type(screen.getByLabelText('Folder name'), '   {Enter}');
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('opens empty again after being closed, rather than holding the last name', async () => {
    const onSubmit = vi.fn();
    const { rerender } = renderWithProviders(
      <NameDialog {...props} onSubmit={onSubmit} onOpenChange={vi.fn()} />,
    );

    await userEvent.type(screen.getByLabelText('Folder name'), 'drafts');

    rerender(
      <NameDialog
        {...props}
        open={false}
        onSubmit={onSubmit}
        onOpenChange={vi.fn()}
      />,
    );
    rerender(
      <NameDialog {...props} open onSubmit={onSubmit} onOpenChange={vi.fn()} />,
    );

    await waitFor(() => {
      expect(screen.getByLabelText('Folder name')).toHaveValue('');
    });
  });

  it('opens holding the current name when it is renaming something', () => {
    renderWithProviders(
      <NameDialog
        {...props}
        title="Rename"
        initialValue="Client Acme"
        submitLabel="Save"
        onSubmit={vi.fn()}
        onOpenChange={vi.fn()}
      />,
    );

    expect(screen.getByLabelText('Folder name')).toHaveValue('Client Acme');
  });

  it('blocks a name the browser already knows is taken, and says why', async () => {
    const onSubmit = vi.fn();
    renderWithProviders(
      <NameDialog
        {...props}
        validate={(value) =>
          value.trim() === 'taken'
            ? { ok: false, hint: 'There is already one called “taken”.' }
            : { ok: true }
        }
        onSubmit={onSubmit}
        onOpenChange={vi.fn()}
      />,
    );

    const input = screen.getByLabelText('Folder name');
    await userEvent.type(input, 'taken');

    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(
      screen.getByText('There is already one called “taken”.'),
    ).toBeInTheDocument();

    await userEvent.type(input, '{Enter}');
    expect(onSubmit).not.toHaveBeenCalled();
  });
});

describe('RowActions', () => {
  it('names the row it belongs to, so the trigger is not just “button”', async () => {
    const onSelect = vi.fn();
    renderWithProviders(
      <RowActions label="notes.md">
        <DropdownMenuItem onSelect={onSelect}>Delete</DropdownMenuItem>
      </RowActions>,
    );

    const trigger = screen.getByRole('button', {
      name: 'Actions for notes.md',
    });
    await userEvent.click(trigger);

    await userEvent.click(
      await screen.findByRole('menuitem', { name: 'Delete' }),
    );
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
});

describe('DataList', () => {
  it('is a list of rows, so nothing depends on a display property', () => {
    renderWithProviders(
      <DataList label="Providers">
        <DataListRow
          primary={<button type="button">Ollama</button>}
          meta={<span>no key</span>}
        />
        <DataListRow primary={<button type="button">OpenAI</button>} />
      </DataList>,
    );

    // The point of the `<ul>`: a `<tr>` set to `display: grid` stops being a
    // row to a screen reader, and the card layout this replaced a table with
    // needs a grid on every row.
    const rows = within(
      screen.getByRole('list', { name: 'Providers' }),
    ).getAllByRole('listitem');
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent('no key');
  });

  it('leaves out the parts a row does not have', () => {
    renderWithProviders(
      <DataList label="Providers">
        <DataListRow primary={<button type="button">OpenAI</button>} />
      </DataList>,
    );

    // An empty meta cluster would still take a grid row and a gap, which shows
    // up as a card that is taller than the one below it for no reason.
    expect(document.querySelector('.data-list__meta')).toBeNull();
    expect(document.querySelector('.data-list__actions')).toBeNull();
  });
});

describe('ListSort', () => {
  const OPTIONS = [
    { key: 'name', label: 'Name' },
    { key: 'size', label: 'Size' },
  ] as const;

  const render = (descending: boolean, onChange = vi.fn()) =>
    renderWithProviders(
      <ListSort
        options={OPTIONS}
        sort={{ key: 'name', descending }}
        ascendingFirst={['name']}
        onChange={onChange}
      />,
    );

  it('says both halves of the state without being opened', () => {
    render(false);

    // The column *and* the direction. A trigger labelled only "Sort by" is a
    // control you have to open to find out what it is doing.
    expect(
      screen.getByRole('button', { name: 'Sort by Name, Ascending' }),
    ).toBeInTheDocument();
  });

  it('opens a new column in the direction that column is read', async () => {
    const onChange = vi.fn();
    render(false, onChange);

    await userEvent.click(screen.getByRole('button', { name: /Sort by/ }));
    await userEvent.click(
      await screen.findByRole('menuitemradio', { name: 'Size' }),
    );

    // `size` is not in `ascendingFirst`: "which is biggest" is the question
    // that column answers, so it opens largest first.
    expect(onChange).toHaveBeenCalledWith({ key: 'size', descending: true });
  });

  it('reverses without touching the column', async () => {
    const onChange = vi.fn();
    render(false, onChange);

    await userEvent.click(screen.getByRole('button', { name: /Sort by/ }));
    await userEvent.click(
      await screen.findByRole('menuitemradio', { name: 'Descending' }),
    );

    expect(onChange).toHaveBeenCalledWith({ key: 'name', descending: true });
  });

  it('marks the column and the direction currently in force', async () => {
    render(true);

    await userEvent.click(screen.getByRole('button', { name: /Sort by/ }));

    // Two groups, so two marks — which is what makes the second question
    // answerable without first working out the answer to the first.
    expect(
      await screen.findByRole('menuitemradio', { name: 'Name' }),
    ).toBeChecked();
    expect(
      screen.getByRole('menuitemradio', { name: 'Descending' }),
    ).toBeChecked();
    expect(
      screen.getByRole('menuitemradio', { name: 'Ascending' }),
    ).not.toBeChecked();
  });
});

describe('pageItems', () => {
  it('spells out a short run rather than eliding it', () => {
    expect(pageItems(1, 1)).toEqual([1]);
    expect(pageItems(2, 3)).toEqual([1, 2, 3]);
  });

  it('keeps the two ends and a window around where you are', () => {
    expect(pageItems(6, 12)).toEqual([1, 'gap', 5, 6, 7, 'gap', 12]);
    expect(pageItems(1, 12)).toEqual([1, 2, 'gap', 12]);
    expect(pageItems(12, 12)).toEqual([1, 'gap', 11, 12]);
  });

  /**
   * `1 … 3` is absurd: the ellipsis is as wide as the number it replaced, so it
   * turns a destination into a mystery and saves nothing.
   */
  it('renders a lone hidden page instead of eliding one', () => {
    expect(pageItems(1, 4)).toEqual([1, 2, 3, 4]);
    expect(pageItems(4, 6)).toEqual([1, 2, 3, 4, 5, 6]);
  });
});

describe('usePagination', () => {
  /** A probe that renders the hook's state and lets a test drive it. */
  function Probe({
    total,
    resetOn,
  }: {
    total: number;
    resetOn: string;
  }): JSX.Element {
    const pagination = usePagination({ resetOn }).withTotal(total);
    return (
      <>
        <output data-testid="state">
          {pagination.page}/{pagination.pageCount} rows {pagination.start}-
          {pagination.end} offset {pagination.offset}
        </output>
        <Pagination pagination={pagination} total={total} label="Rows" />
      </>
    );
  }

  const state = (): string => screen.getByTestId('state').textContent;

  it('offsets by whole pages', async () => {
    renderWithProviders(<Probe total={120} resetOn="" />);
    expect(state()).toBe('1/5 rows 1-25 offset 0');

    // Two steps rather than a jump: from page 1 the window reaches 2, and page
    // 3 is behind the elision until you are next to it.
    await userEvent.click(screen.getByRole('button', { name: 'Next page' }));
    await userEvent.click(screen.getByRole('button', { name: 'Next page' }));
    expect(state()).toBe('3/5 rows 51-75 offset 50');
  });

  it('reports a short last page honestly', async () => {
    renderWithProviders(<Probe total={52} resetOn="" />);

    await userEvent.click(screen.getByRole('button', { name: 'Page 3' }));
    // Not 51-75: there are two rows on it, and claiming a full page is how a
    // reader concludes rows are missing.
    expect(state()).toBe('3/3 rows 51-52 offset 50');
  });

  /**
   * The bug this hook exists for. Typing into a search box while on page 4
   * otherwise leaves you on page 4 of a result set that now has one page —
   * an empty list under a control insisting there are matches.
   */
  it('returns to the first page when the filter changes', async () => {
    // `update` rather than RTL's `rerender`: the latter swaps the provider stack
    // out too, which remounts the subject and resets the state under test.
    const { update } = renderWithProviders(<Probe total={120} resetOn="" />);

    await userEvent.click(screen.getByRole('button', { name: 'Page 5' }));
    expect(state()).toBe('5/5 rows 101-120 offset 100');

    update(<Probe total={8} resetOn="login" />);
    expect(state()).toBe('1/1 rows 1-8 offset 0');
  });

  /** Delete the only row on the last page and that page stops existing. */
  it('pulls back into range when the total shrinks under it', async () => {
    const { update } = renderWithProviders(<Probe total={120} resetOn="" />);

    await userEvent.click(screen.getByRole('button', { name: 'Page 5' }));
    expect(state()).toBe('5/5 rows 101-120 offset 100');

    // Same filter — the rows themselves went away. One row left on page 5 is
    // still a page 5.
    update(<Probe total={101} resetOn="" />);
    expect(state()).toBe('5/5 rows 101-101 offset 100');

    // That row goes too, and page 5 stops existing.
    update(<Probe total={100} resetOn="" />);
    expect(state()).toBe('4/4 rows 76-100 offset 75');
  });

  it('calls an empty list page 1 of 1, not page 1 of 0', () => {
    renderWithProviders(<Probe total={0} resetOn="" />);
    expect(state()).toBe('1/1 rows 0-0 offset 0');
  });
});

describe('Pagination', () => {
  function render(total: number): void {
    function Harness(): JSX.Element {
      const pagination = usePagination({ resetOn: '' }).withTotal(total);
      return (
        <Pagination pagination={pagination} total={total} label="Sessions" />
      );
    }
    renderWithProviders(<Harness />);
  }

  /** A disabled Previous and Next under every short list is chrome that can never act. */
  it('is absent when there is only one page', () => {
    render(25);
    expect(
      screen.queryByRole('navigation', { name: 'Sessions' }),
    ).not.toBeInTheDocument();
  });

  it('says how much there is, which is what a filter is judged by', () => {
    render(287);
    expect(screen.getByText('Showing 1–25 of 287')).toBeInTheDocument();
  });

  it('marks the current page for a screen reader as well as visually', async () => {
    render(120);

    const current = screen.getByRole('button', { name: 'Page 1' });
    expect(current).toHaveAttribute('aria-current', 'page');

    await userEvent.click(screen.getByRole('button', { name: 'Next page' }));

    expect(screen.getByRole('button', { name: 'Page 2' })).toHaveAttribute(
      'aria-current',
      'page',
    );
    expect(screen.getByRole('button', { name: 'Page 1' })).not.toHaveAttribute(
      'aria-current',
    );
  });

  it('disables the end it is already at', async () => {
    render(60);

    expect(
      screen.getByRole('button', { name: 'Previous page' }),
    ).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Next page' })).toBeEnabled();

    await userEvent.click(screen.getByRole('button', { name: 'Page 3' }));

    expect(screen.getByRole('button', { name: 'Previous page' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Next page' })).toBeDisabled();
  });

  /** Reading "ellipsis" between two numbers says nothing anyone can act on. */
  it('hides the elision from the accessibility tree', () => {
    render(500);
    expect(
      screen.getByRole('navigation', { name: 'Sessions' }),
    ).toHaveTextContent('…');
    expect(screen.queryByRole('button', { name: '…' })).not.toBeInTheDocument();
  });
});
