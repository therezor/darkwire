/**
 * The composer.
 *
 * Four behaviours, each of which is invisible when it breaks and infuriating
 * when it does: Enter sends and Shift+Enter does not, the box grows with the
 * text, an attachment is uploaded when it is chosen rather than when Send is
 * pressed, and the `/` popover is drivable without touching the mouse.
 */

import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type { Attachment } from '@ghostwire/protocol';

import { parseCommand } from '@/chat/commands.js';
import { Composer } from '@/chat/composer.js';
import { AGENTS } from '@testkit/fixtures.js';
import { renderWithProviders, stubFetch } from '@testkit/render.js';

interface Sent {
  readonly text: string;
  readonly attachments: readonly Attachment[];
}

function mount(
  overrides: Partial<React.ComponentProps<typeof Composer>> = {},
): {
  readonly sent: Sent[];
  readonly stops: number[];
  readonly commands: string[];
} {
  const sent: Sent[] = [];
  const stops: number[] = [];
  const commands: string[] = [];

  renderWithProviders(
    <Composer
      busy={false}
      queueDepth={0}
      connected
      configured
      onSend={(text, attachments) => sent.push({ text, attachments })}
      onStop={() => stops.push(1)}
      // The real parser behind a fake dispatcher. What is under test here is
      // what the box does with each answer, and deciding *which* answer a line
      // earns is `commands.test.ts`'s job — a hand-rolled stub would be a
      // second, quietly divergent copy of that rule.
      onCommand={(text) => {
        if (parseCommand(text) === undefined) return false;
        commands.push(text);
        return true;
      }}
      {...overrides}
    />,
  );

  return { sent, stops, commands };
}

const box = (): HTMLElement => screen.getByRole('textbox', { name: 'Message' });

/** The hidden `<input type="file">` the Attach button clicks for the user. */
const filePicker = (): HTMLInputElement => {
  const input = document.body.querySelector('input[type="file"]');
  if (!(input instanceof HTMLInputElement)) {
    throw new Error('no file input rendered');
  }
  return input;
};

describe('sending', () => {
  it('sends on Enter and starts a new line on Shift+Enter', async () => {
    const user = userEvent.setup();
    const { sent } = mount();

    await user.type(box(), 'first line{Shift>}{Enter}{/Shift}second line');
    expect(sent).toEqual([]);

    await user.keyboard('{Enter}');

    expect(sent).toEqual([
      { text: 'first line\nsecond line', attachments: [] },
    ]);
    // The composer clears, because the message is gone.
    expect(box()).toHaveValue('');
  });

  it('refuses to send nothing', async () => {
    const user = userEvent.setup();
    mount();

    expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled();

    await user.type(box(), '   ');
    // Whitespace is nothing.
    expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled();
  });

  it('grows with the text without measuring anything', async () => {
    const user = userEvent.setup();
    mount();

    await user.type(box(), 'one{Shift>}{Enter}{/Shift}two');

    // The mirror is what sizes the grid cell; the textarea stretches to fill
    // it. No `scrollHeight` read, and no pixel height written anywhere.
    const mirror = document.body.querySelector('.composer__mirror');
    expect(mirror?.textContent).toBe('one\ntwo\n');
    expect(box().getAttribute('style')).toBeNull();
  });

  it('offers Stop instead of Send while a turn is running, and still queues', async () => {
    const user = userEvent.setup();
    const { sent, stops } = mount({ busy: true, queueDepth: 2 });

    expect(screen.getByText(/2 messages waiting/)).toBeInTheDocument();
    await user.click(
      screen.getByRole('button', { name: 'Stop the current turn' }),
    );
    expect(stops).toHaveLength(1);

    // Enter still sends: the hub queues it, and disabling the box would lose
    // the sentence the user is halfway through.
    await user.type(box(), 'and another thing{Enter}');
    expect(sent).toEqual([{ text: 'and another thing', attachments: [] }]);
  });

  it('says so when the socket is down', () => {
    mount({ connected: false });

    expect(screen.getByText(/Offline/)).toBeInTheDocument();
  });

  it('says nothing at all when there is nothing happening', () => {
    // The line under the box carries transient state only. A keyboard hint that
    // is identical on every render for the life of the install is documentation,
    // and it was taking the width the context budget needed — it lives on the
    // welcome screen now.
    mount();

    expect(screen.queryByText(/Enter to send/)).not.toBeInTheDocument();
    expect(screen.queryByText(/A turn is running/)).not.toBeInTheDocument();
  });

  it('still says when a turn is running, because that changes what Enter does', () => {
    mount({ busy: true });

    expect(screen.getByText(/A turn is running/)).toBeInTheDocument();
  });

  it('counts what is queued alongside whatever else is true', () => {
    mount({ busy: true, queueDepth: 2 });

    expect(screen.getByText(/A turn is running/)).toBeInTheDocument();
    expect(screen.getByText(/2 messages waiting/)).toBeInTheDocument();
  });
});

describe('the / autocomplete', () => {
  it('opens on a slash, moves with the arrow keys and accepts with Enter', async () => {
    const user = userEvent.setup();
    const { sent } = mount();

    await user.type(box(), '/');

    const options = screen.getAllByRole('option');
    expect(options[0]?.textContent).toContain('/new');
    // Focus stays in the textarea; `aria-activedescendant` is what tells a
    // screen reader which option the arrow keys are on.
    expect(box()).toHaveAttribute(
      'aria-activedescendant',
      'composer-commands-0',
    );

    await user.keyboard('{ArrowDown}');
    expect(box()).toHaveAttribute(
      'aria-activedescendant',
      'composer-commands-1',
    );

    await user.keyboard('{Enter}');

    // Enter accepted the highlighted suggestion rather than sending.
    expect(sent).toEqual([]);
    expect(box()).toHaveValue('/clear ');
  });

  it('closes on Escape and stays closed until the next keystroke', async () => {
    const user = userEvent.setup();
    mount();

    await user.type(box(), '/re');
    expect(screen.getAllByRole('option')).toHaveLength(1);

    await user.keyboard('{Escape}');
    expect(screen.queryByRole('option')).not.toBeInTheDocument();

    await user.keyboard('n');
    expect(screen.getAllByRole('option')).toHaveLength(1);
  });

  it('offers nothing once a command that takes free text is chosen', async () => {
    const user = userEvent.setup();
    mount();

    await user.type(box(), '/rename a title');

    // `/rename` takes prose, so there is nothing to complete. A menu reading
    // "no results" there would look broken where an absent one reads as absent.
    expect(screen.queryByRole('option')).not.toBeInTheDocument();
  });

  it('accepting /agent narrows the menu to the configured agents', async () => {
    // The bug this guards: the caret was read from a ref during render, so the
    // render after an accept saw the *old* DOM position, decided the caret was
    // outside a command and closed the popover. Moving a caret does not
    // re-render, so nothing ever reopened it.
    const user = userEvent.setup();
    stubFetch({ '/api/agents': [200, AGENTS] });
    mount();

    await user.type(box(), '/agent');
    await user.keyboard('{Enter}');
    expect(box()).toHaveValue('/agent ');

    const options = await screen.findAllByRole('option');
    expect(options.map((option) => option.textContent)).toEqual([
      expect.stringContaining('default'),
      expect.stringContaining('researcher'),
    ]);
  });

  it('opens the effort list on the level in force, and says which it is', async () => {
    // The list is six words anyone could have typed — what makes it worth
    // opening is that it says which one is running. The cursor starts there
    // too, so accepting without touching an arrow key is a no-op rather than a
    // silent change to whatever sat at the top.
    const user = userEvent.setup();
    stubFetch({ '/api/agents': [200, AGENTS] });
    mount({ agent: { effort: 'high' } });

    await user.type(box(), '/effort ');

    const options = await screen.findAllByRole('option');
    const at = options.findIndex((option) =>
      option.textContent.startsWith('high'),
    );
    expect(box()).toHaveAttribute(
      'aria-activedescendant',
      `composer-commands-${String(at)}`,
    );
    expect(options[at]).toHaveTextContent('current');
    // The ramp is left in its own order: hoisting the current row to the top
    // would scramble minimal → xhigh to save a glance.
    expect(options[0]?.textContent).toContain('default');
  });

  it('marks default when the agent states no effort of its own', async () => {
    const user = userEvent.setup();
    stubFetch({ '/api/agents': [200, AGENTS] });
    mount({ agent: {} });

    await user.type(box(), '/effort ');

    const options = await screen.findAllByRole('option');
    expect(options[0]).toHaveTextContent('default');
    expect(options[0]).toHaveTextContent('current');
    expect(box()).toHaveAttribute(
      'aria-activedescendant',
      'composer-commands-0',
    );
  });

  it('accepts a value with the mouse and closes', async () => {
    const user = userEvent.setup();
    stubFetch({ '/api/agents': [200, AGENTS] });
    const { sent } = mount();

    await user.type(box(), '/agent res');
    const option = await screen.findByRole('option');
    await user.click(option);

    // The trailing space closes the argument, so what is typed next is not
    // more of the id.
    await waitFor(() => {
      expect(box()).toHaveValue('/agent researcher ');
    });
    expect(screen.queryByRole('option')).not.toBeInTheDocument();
    expect(sent).toEqual([]);
  });
});

describe('running a command', () => {
  it('sends a command to the dispatcher instead of to the model', async () => {
    const user = userEvent.setup();
    const { sent, commands } = mount();

    // One keypress. `/stop` is typed in full and takes no argument, so the
    // list has already closed and Enter submits rather than inserting a space.
    await user.type(box(), '/stop{Enter}');

    expect(commands).toEqual(['/stop']);
    expect(sent).toEqual([]);
    expect(box()).toHaveValue('');
  });

  it('leaves prose alone', async () => {
    const user = userEvent.setup();
    const { sent, commands } = mount();

    // The trap the parser exists for. Nothing here opens a popover, and the
    // sentence reaches the model intact.
    await user.type(box(), '/usr/bin/env is on the path{Enter}');

    expect(commands).toEqual([]);
    expect(sent).toEqual([
      { text: '/usr/bin/env is on the path', attachments: [] },
    ]);
  });

  it('never treats a message carrying a file as a command', async () => {
    const user = userEvent.setup();
    stubFetch({
      '/api/files/upload': [
        201,
        { path: 'uploads/a-x.txt', sizeBytes: 1, mimeType: 'text/plain' },
      ],
    });
    const { sent, commands } = mount();

    await user.upload(filePicker(), new File(['x'], 'x.txt'));
    await screen.findByText('1 B');

    await user.type(box(), '/stop{Enter}');

    // Running the command would have discarded the upload without saying so.
    expect(commands).toEqual([]);
    expect(sent[0]?.text).toBe('/stop');
  });

  it('opens the whole list on a bare slash', async () => {
    const user = userEvent.setup();
    mount();

    await user.type(box(), '/');

    expect(screen.getAllByRole('option').length).toBeGreaterThan(1);
  });
});

describe('attachments', () => {
  it('uploads on selection, not on send, and sends the workspace path', async () => {
    const user = userEvent.setup();
    stubFetch({
      '/api/files/upload': [
        201,
        {
          path: 'uploads/abc-note.txt',
          sizeBytes: 5,
          mimeType: 'text/plain',
          signedUrl: { url: '/api/media/token', expiresAtMs: 2_000 },
        },
      ],
    });
    const { sent } = mount();

    await user.upload(
      filePicker(),
      new File(['hello'], 'note.txt', { type: 'text/plain' }),
    );

    // A four-second upload starting when Send is pressed is four seconds of a
    // button that appears to have done nothing.
    expect(await screen.findByText('5 B')).toBeInTheDocument();

    await user.type(box(), 'look at this{Enter}');

    expect(sent[0]?.attachments).toEqual([
      // The workspace path, never the signed URL. The server reads the bytes
      // off disk when it builds the provider request, which happens long after
      // a ten-minute token would have expired -- and the file tools address
      // this same path, which is what lets the model open what it cannot see.
      {
        mimeType: 'text/plain',
        path: 'uploads/abc-note.txt',
        name: 'note.txt',
        sizeBytes: 5,
      },
    ]);
  });

  it('refuses a file larger than the server would accept, without uploading it', async () => {
    // The server's `bodyLimit` fires only after the whole body is on the wire,
    // so a 60 MB video would spend a minute uploading to earn a 413 and a chip
    // that says "failed". Refusing here costs nothing and can say why.
    const user = userEvent.setup();
    // No routes at all: `stubFetch` rejects anything it was not told about, so
    // an upload that got as far as the network would surface as a "failed"
    // chip rather than the refusal under test.
    stubFetch({});
    mount();

    const huge = new File(['x'], 'huge.bin');
    Object.defineProperty(huge, 'size', { value: 26 * 1024 * 1024 });
    await user.upload(filePicker(), huge);

    await waitFor(() => {
      expect(screen.getByText(/larger than/)).toBeInTheDocument();
    });
    expect(screen.queryByText('huge.bin')).not.toBeInTheDocument();
  });

  it('attaches a pasted screenshot', async () => {
    const user = userEvent.setup();
    stubFetch({
      '/api/files/upload': [
        201,
        { path: 'uploads/abc-image.png', sizeBytes: 5, mimeType: 'image/png' },
      ],
    });
    mount();

    await user.click(box());
    await user.paste({
      files: [new File(['hello'], 'image.png', { type: 'image/png' })],
    } as unknown as DataTransfer);

    expect(await screen.findByText('image.png')).toBeInTheDocument();
  });

  it('leaves a text paste alone', async () => {
    // The guard that makes the paste handler safe. Claiming every paste would
    // break pasting a URL into the message, which is the far commoner gesture.
    const user = userEvent.setup();
    stubFetch({});
    mount();

    await user.click(box());
    await user.paste('https://example.test/a-very-long-url');

    expect(box()).toHaveValue('https://example.test/a-very-long-url');
  });

  it('reports a failed upload on the chip rather than losing it silently', async () => {
    const user = userEvent.setup();
    stubFetch({
      '/api/files/upload': [
        413,
        { error: { code: 'bad_request', message: 'Too large' } },
      ],
    });
    mount();

    await user.upload(filePicker(), new File(['x'], 'big.bin'));

    expect(await screen.findByText('failed')).toBeInTheDocument();
  });

  it('drops a staged file on request', async () => {
    const user = userEvent.setup();
    stubFetch({
      '/api/files/upload': [
        201,
        { path: 'uploads/a-x.txt', sizeBytes: 1, mimeType: 'text/plain' },
      ],
    });
    mount();

    await user.upload(filePicker(), new File(['x'], 'x.txt'));

    await user.click(
      await screen.findByRole('button', { name: 'Remove x.txt' }),
    );

    await waitFor(() => {
      expect(screen.queryByText('x.txt')).not.toBeInTheDocument();
    });
  });
});

describe('the file picker', () => {
  it('is opened by a labelled button rather than by a bare input', async () => {
    const user = userEvent.setup();
    mount();

    const click = vi
      .spyOn(filePicker(), 'click')
      .mockImplementation(() => undefined);

    await user.click(screen.getByRole('button', { name: 'Attach a file' }));

    expect(click).toHaveBeenCalledOnce();
  });
});

describe('with no model configured', () => {
  it('says what is missing rather than failing on send', () => {
    // Chat stays in the nav and the route still renders: everything else on a
    // fresh install works, and a nav that changes shape underneath the user is
    // a worse answer than a control that says why it is off.
    mount({ configured: false });

    expect(box()).toBeDisabled();
    expect(box()).toHaveAttribute('placeholder', 'No model configured yet');
  });

  it('cannot send, however the text got there', () => {
    const { sent } = mount({ configured: false });
    expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled();
    expect(sent).toHaveLength(0);
  });
});
