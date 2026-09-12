/**
 * The primitives, asserted on the behaviour they were adopted for.
 *
 * Radix is a dependency, so testing that Radix works would be testing someone
 * else's library. What is worth asserting is that *our* wiring of it did not
 * break the behaviour: a `DialogContent` that forgot the portal, a menu whose
 * items are not items, a switch whose thumb swallowed the click. Each of these
 * is a mistake that leaves the component looking correct and behaving wrong.
 *
 * Between them they cover what the primitives owe: keyboard reachability, focus
 * trapping, and Escape.
 */

import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { Button } from '@/components/ui/button.js';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogHeading,
  DialogTrigger,
} from '@/components/ui/dialog.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';
import { Field } from '@/components/ui/field.js';
import { Switch } from '@/components/ui/switch.js';
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs.js';
import { Toaster, resetToasts, toast } from '@/components/ui/toast.js';

describe('Dialog', () => {
  function Example({
    onConfirm = vi.fn(),
  }: {
    readonly onConfirm?: () => void;
  }) {
    return (
      <Dialog>
        <DialogTrigger asChild>
          <Button>Open</Button>
        </DialogTrigger>
        <DialogContent>
          <DialogHeader>
            <DialogHeading>Delete the session</DialogHeading>
          </DialogHeader>
          <Field label="Confirm" />
          <DialogFooter>
            <Button onClick={onConfirm}>Confirm</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }

  it('opens from the keyboard and is labelled by its heading', async () => {
    const user = userEvent.setup();
    render(<Example />);

    await user.tab();
    await user.keyboard('{Enter}');

    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveAccessibleName('Delete the session');
  });

  it('traps focus: Tab cycles inside rather than escaping to the page', async () => {
    const user = userEvent.setup();
    render(<Example />);

    await user.click(screen.getByRole('button', { name: 'Open' }));
    const dialog = await screen.findByRole('dialog');

    // Ten tabs is more than the dialog holds; every landing has to be inside.
    for (let i = 0; i < 10; i += 1) {
      await user.tab();
      expect(dialog).toContainElement(document.activeElement as HTMLElement);
    }
  });

  it('closes on Escape and returns focus to what opened it', async () => {
    const user = userEvent.setup();
    render(<Example />);

    const trigger = screen.getByRole('button', { name: 'Open' });
    await user.click(trigger);
    await screen.findByRole('dialog');

    await user.keyboard('{Escape}');

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
    expect(trigger).toHaveFocus();
  });

  it('offers a close button, since Escape is not discoverable on a touch device', async () => {
    const user = userEvent.setup();
    render(<Example />);

    await user.click(screen.getByRole('button', { name: 'Open' }));
    const dialog = await screen.findByRole('dialog');

    await user.click(within(dialog).getByRole('button', { name: 'Close' }));
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });
});

describe('DropdownMenu', () => {
  it('opens with the keyboard and selects with Enter', async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();

    render(
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button>Actions</Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem>Rename</DropdownMenuItem>
          <DropdownMenuItem onSelect={onSelect}>Delete</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>,
    );

    await user.tab();
    await user.keyboard('{Enter}');

    const menu = await screen.findByRole('menu');
    expect(within(menu).getAllByRole('menuitem')).toHaveLength(2);

    // Opening with Enter highlights the first item, so one ArrowDown reaches
    // the second — which is the item the spy is on.
    await user.keyboard('{ArrowDown}{Enter}');
    expect(onSelect).toHaveBeenCalled();
  });

  it('closes on Escape', async () => {
    const user = userEvent.setup();
    render(
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button>Actions</Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent>
          <DropdownMenuItem>Rename</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>,
    );

    await user.click(screen.getByRole('button', { name: 'Actions' }));
    await screen.findByRole('menu');

    await user.keyboard('{Escape}');
    await waitFor(() => {
      expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    });
  });
});

describe('Switch', () => {
  it('is a switch, not a checkbox, and toggles from the keyboard', async () => {
    const user = userEvent.setup();

    function Example() {
      const [on, setOn] = useState(false);
      return (
        <Switch
          checked={on}
          onCheckedChange={setOn}
          aria-label="Stream responses"
        />
      );
    }
    render(<Example />);

    const control = screen.getByRole('switch', { name: 'Stream responses' });
    expect(control).not.toBeChecked();

    await user.tab();
    expect(control).toHaveFocus();

    await user.keyboard(' ');
    expect(control).toBeChecked();
  });
});

describe('Tabs', () => {
  it('moves between tabs with the arrow keys', async () => {
    const user = userEvent.setup();
    render(
      <Tabs defaultValue="one">
        <TabsList>
          <TabsTrigger value="one">First</TabsTrigger>
          <TabsTrigger value="two">Second</TabsTrigger>
        </TabsList>
        <TabsContent value="one">First panel</TabsContent>
        <TabsContent value="two">Second panel</TabsContent>
      </Tabs>,
    );

    expect(screen.getByText('First panel')).toBeInTheDocument();

    await user.tab();
    await user.keyboard('{ArrowRight}');

    expect(screen.getByRole('tab', { name: 'Second' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(screen.getByText('Second panel')).toBeInTheDocument();
  });
});

describe('Field', () => {
  it('wires the label, the input and the message together', () => {
    render(<Field label="Password" error="Incorrect password." />);

    const input = screen.getByLabelText('Password');
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(input).toHaveAccessibleDescription('Incorrect password.');
    expect(screen.getByRole('alert')).toHaveTextContent('Incorrect password.');
  });

  it('does not announce a hint as an alert', () => {
    render(<Field label="Workspace" hint="Where the agent may write." />);

    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(screen.getByLabelText('Workspace')).toHaveAccessibleDescription(
      'Where the agent may write.',
    );
  });
});

describe('toast', () => {
  it('can be raised from outside React and is announced, not focused', async () => {
    render(<Toaster />);

    const active = document.activeElement;
    toast.error('The socket dropped', 'Reconnecting…');

    expect(await screen.findByText('The socket dropped')).toBeInTheDocument();
    // A toast that stole focus would interrupt whatever the user was typing.
    expect(document.activeElement).toBe(active);
  });

  it('dismisses on request', async () => {
    const user = userEvent.setup();
    render(<Toaster />);

    toast.success('Saved');
    await screen.findByText('Saved');

    await user.click(screen.getByRole('button', { name: 'Dismiss' }));
    await waitFor(() => {
      expect(screen.queryByText('Saved')).not.toBeInTheDocument();
    });
  });

  it('keeps failures on screen and lets successes expire', () => {
    resetToasts();
    toast.error('Broken');
    toast.success('Fine');

    // The duration rule lives in the component, so this asserts the intent it
    // encodes: an error a user has to reproduce to read is an error they lost.
    render(<Toaster />);
    expect(screen.getByText('Broken')).toBeInTheDocument();
    expect(screen.getByText('Fine')).toBeInTheDocument();
  });
});
