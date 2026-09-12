/**
 * The turn-details popover, driven directly.
 *
 * Most of what it shows is arithmetic over numbers the turn already carries.
 * The one row that is not is **where the session came from**, and that row is
 * here for a reason worth writing down: it used to be a badge under the message
 * box, beside the agent picker. Every session a person opens is `web`, so the
 * badge printed the same word under almost every composer in the app and told
 * nobody anything — the same objection `BADGED_ORIGINS` in
 * `sessions/sessions-page.tsx` already raised against badging `web` in the list.
 *
 * It belongs with the model and the provider instead: one popover answering
 * "what did this actually run on", asked only by someone who wants to know.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { TurnItem } from '@/state/transcript.js';
import { renderWithProviders, stubFetch } from '@testkit/render.js';

import { TurnInfo } from '@/chat/turn-info.js';

const SESSION = 'web:1';

const TURN: TurnItem = {
  kind: 'turn',
  id: 't1',
  sessionKey: SESSION,
  model: 'test-model',
  provider: 'test',
  parts: [],
  stopReason: 'complete',
  // Present, so the body renders its table rather than the "no figures" line.
  usage: { promptTokens: 1200, completionTokens: 88, totalTokens: 1288 },
  iterations: 1,
  elapsedMs: 2000,
  // Absent, so this fixture exercises the fallback to the wall clock — which
  // is what every turn recorded before generation time was measured does.
  generationMs: undefined,
  generationTokens: undefined,
  firstTokenMs: undefined,
  firstSeq: 1,
  lastSeq: 2,
  done: true,
  failure: undefined,
  authoritative: false,
};

function sessionRow(
  origin: string,
  workspaceId = 'default',
): Record<string, [number, unknown]> {
  return {
    [`/api/sessions/${encodeURIComponent(SESSION)}`]: [
      200,
      {
        key: SESSION,
        title: 'Nightly weather',
        messageCount: 2,
        createdAtMs: 1,
        updatedAtMs: 2,
        origin,
        workspaceId,
      },
    ],
  };
}

async function openDetails(turn: TurnItem = TURN): Promise<void> {
  renderWithProviders(<TurnInfo turn={turn} sessionKey={SESSION} />);
  await userEvent.click(screen.getByRole('button', { name: 'Turn details' }));
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('where a session came from', () => {
  it('names the origin, so a scheduled run is not mistaken for one someone had', async () => {
    // A session key is a plain id and says nothing about what made it, which is
    // the right shape for a key — so this is the only place the answer is
    // readable once the session is open.
    stubFetch(sessionRow('automation'));
    await openDetails();

    expect(await screen.findByText('Started by')).toBeInTheDocument();
    expect(screen.getByText('automation')).toBeInTheDocument();
  });

  it('says nothing for a session someone started themselves', async () => {
    // `web` is every session opened by hand. A row that is always present and
    // always says the same word is a row that costs a line and answers nothing.
    stubFetch(sessionRow('web'));
    await openDetails();

    // Waited for through a row that is always there, so the assertion below is
    // about the origin being absent rather than about the fetch being slow.
    expect(await screen.findByText('Model')).toBeInTheDocument();
    expect(screen.queryByText('Started by')).not.toBeInTheDocument();
    expect(screen.queryByText('web')).not.toBeInTheDocument();
  });

  it('says nothing for a session with no row yet', async () => {
    // A fresh tab has not been spoken in, so the server has no row for it and
    // inventing `web` would be a claim rather than a fact.
    stubFetch({
      [`/api/sessions/${encodeURIComponent(SESSION)}`]: [
        404,
        { error: 'not found' },
      ],
    });
    await openDetails();

    expect(await screen.findByText('Model')).toBeInTheDocument();
    expect(screen.queryByText('Started by')).not.toBeInTheDocument();
  });
});

describe('which workspace the turn ran in', () => {
  it('names it once the conversation is somewhere other than the default', async () => {
    stubFetch(sessionRow('web', 'research'));
    await openDetails();

    expect(await screen.findByText('Workspace')).toBeInTheDocument();
    expect(screen.getByText('research')).toBeInTheDocument();
  });

  it('says nothing while the conversation is in the default', async () => {
    // The same objection the origin row answers: a line that is always present
    // and always says `default` is a line nobody reads.
    stubFetch(sessionRow('web'));
    await openDetails();

    expect(await screen.findByText('Model')).toBeInTheDocument();
    expect(screen.queryByText('Workspace')).not.toBeInTheDocument();
  });
});

describe('what the turn actually spent', () => {
  // The bug these exist for: `Rate` divided by the whole turn, so a local model
  // that spent thirty seconds loading its weights and one second generating was
  // reported at a thirtieth of its real speed. `Elapsed` still answers "how long
  // did I wait"; `First token` names the load; `Rate` divides by neither.
  const timed = (over: Partial<TurnItem>): TurnItem => ({ ...TURN, ...over });

  it('divides the rate by generation time, not by the whole turn', async () => {
    stubFetch(sessionRow('web'));
    // 88 tokens in 400ms is 220 tok/s. The same turn against its 2s wall clock
    // would read 44 — the model reported at a fifth of its speed because it was
    // loading for most of the turn.
    await openDetails(
      timed({ generationMs: 400, generationTokens: 88, firstTokenMs: 1600 }),
    );

    expect(await screen.findByText('220.0 tok/s')).toBeInTheDocument();
    expect(screen.queryByText('44.0 tok/s')).not.toBeInTheDocument();
  });

  it('names the wait before the first token', async () => {
    stubFetch(sessionRow('web'));
    await openDetails(
      timed({ generationMs: 400, generationTokens: 88, firstTokenMs: 1600 }),
    );

    expect(await screen.findByText('First token')).toBeInTheDocument();
    // Beside it, unchanged: the whole turn is still a question worth answering.
    expect(screen.getByText('Elapsed')).toBeInTheDocument();
  });

  it('falls back to the wall clock for a turn recorded before this', async () => {
    // Every row an older build wrote. Blanking their rate would be a regression
    // dressed as accuracy.
    stubFetch(sessionRow('web'));
    await openDetails();

    expect(await screen.findByText('44.0 tok/s')).toBeInTheDocument();
    expect(screen.queryByText('First token')).not.toBeInTheDocument();
  });

  it('divides the timed tokens, which are not the Out figure beside them', async () => {
    // A turn that also made a bare tool call. Ollama sends one of those as a
    // single frame, so it is charged for its tokens and measured at zero — and
    // only the 40 tokens that were timed may divide the window. `Out` still
    // says 88, deliberately: it is what the turn cost, not what was measured.
    stubFetch(sessionRow('web'));
    await openDetails(
      timed({ generationMs: 400, generationTokens: 40, firstTokenMs: 1600 }),
    );

    expect(await screen.findByText('100.0 tok/s')).toBeInTheDocument();
    expect(screen.getByText('88')).toBeInTheDocument();
    expect(screen.queryByText('220.0 tok/s')).not.toBeInTheDocument();
  });

  it('treats a single-frame reply as unmeasured rather than as instant', async () => {
    // A real zero and still not a divisor. Without the zero guard this row
    // would read `Infinity tok/s`.
    stubFetch(sessionRow('web'));
    await openDetails(
      timed({ generationMs: 0, generationTokens: 0, firstTokenMs: 1600 }),
    );

    expect(await screen.findByText('44.0 tok/s')).toBeInTheDocument();
  });
});
