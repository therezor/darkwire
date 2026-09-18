/**
 * The shipping binary, on a port, with a model that cannot reach the network.
 *
 * The product is one Rust binary, and the only honest way for a browser suite
 * to test it is the way an operator runs it — `darkwire serve`, as a child
 * process, over a temporary home. That is a better test than composing the
 * server in this process, and a slower one, and both halves are worth stating:
 *
 *  - **Nothing here can drift from `serve`.** There is no second copy of the
 *    server's wiring to keep in step, so the suite cannot go blind to a whole
 *    feature — the scheduler, per-agent loops — because the harness wired it
 *    differently.
 *  - **Timing is genuinely variable.** A child process does not answer inside a
 *    frame. Anything asserted here that is only true because the answer arrived
 *    before the next paint is a flake waiting for a loaded CI runner. Assert
 *    what a step *settles into*.
 *
 * Two substitutions remain and both are the binary's own, compiled in behind
 * the `test-hooks` feature and armed by `DARKWIRE_TEST_HOOKS=1`: the password
 * hasher is a comparison rather than argon2id, and the credential vault's key
 * comes from the key file rather than the operating system's keychain. Minting
 * a keychain entry from a test suite is not a thing a test suite may do — on
 * macOS the first run simply hangs on a prompt nobody is there to answer.
 *
 * Everything else is per-test: a fresh home, a fresh workspace, a fresh
 * process, a fresh listener on a port the OS picks. Specs therefore share no
 * state at all, which is what lets them run in parallel and lets each one
 * choose its own settings — the approval matrix in particular, since a spec
 * about the prompt and a spec about Stop want opposite answers from it.
 */

import { spawn, type ChildProcess } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  DEFAULT_AGENT_TOOLS,
  DEFAULT_USERNAME,
  DEFAULT_WORKSPACE_ID,
  type ConfigPatch,
} from '@darkwire/protocol';

import { startFakeProvider } from './provider.js';
import { ROUTES } from './script.js';

/** The repository root, which is what the binary and the bundle are found from. */
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../../../..');

export const PASSWORD = 'e2e-password';

/**
 * The login name the harness signs in with.
 *
 * The default rather than a chosen one, because that is what an install the
 * harness never reconfigured actually has — a constant here that disagreed with
 * `DEFAULT_USERNAME` would be a test suite proving a login that no fresh
 * install can perform.
 */
export const USERNAME = DEFAULT_USERNAME;

/**
 * The provider instance every agent resolves to.
 *
 * `ollama` as the *type* and `e2e` as the id, and both halves are load-bearing.
 * The type, because instance resolution falls back from an unknown instance id
 * to any enabled instance of that provider type — so the specs that configure
 * an agent with `provider: 'ollama'` reach this endpoint without naming it. The
 * id, because `providers.spec.ts` asserts that `config.providers.ollama` does
 * not exist until the operator creates it, and a harness instance squatting on
 * that id would make the assertion pass for the wrong reason.
 */
export const INSTANCE_ID = 'e2e';

/** How long a debug binary's first spawn is allowed to take. */
const READY_TIMEOUT_MS = 15_000;

/** How long SIGTERM is given before SIGKILL. */
const GRACE_MS = 2_000;

/** The workspace the file browser and `ls` both see. */
const FIXTURE_FILES: Readonly<Record<string, string>> = {
  'notes.md': '# Notes\n\nOne file, so the browser has something to list.\n',
  'src/main.ts': 'export const answer = 42;\n',
};

export interface HarnessOptions {
  /**
   * Merged over the defaults before the process starts.
   *
   * Top-level keys *replace* rather than merge, which is what lets a spec say
   * "no providers at all" by naming an empty map. The approval matrix is the
   * setting specs actually move: the prompt spec wants `exec: 'ask'` (which is
   * the default, and what a browser-facing server ships with), and a spec that
   * only needs a tool to run wants it out of the way.
   */
  readonly config?: ConfigPatch;
  /** Sessions to write into the store before the browser connects. */
  readonly sessions?: readonly SeedSession[];
  /** Notifications the centre should already be holding. */
  readonly notifications?: readonly SeedNotification[];
  /**
   * `null` starts the server unclaimed, as a first run.
   *
   * The default sets one, because every other spec wants to be past the door.
   * A spec about the setup wizard wants the state the wizard exists for: no
   * password, and a one-time code on `Harness.setupCode`.
   */
  readonly password?: string | null;
}

export interface SeedSession {
  readonly key: string;
  readonly title?: string;
  /** Alternating user/assistant text, starting with the user. */
  readonly turns?: readonly string[];
}

export interface SeedNotification {
  readonly title: string;
  readonly body?: string;
  readonly level?: 'info' | 'warning' | 'error';
}

export interface Harness {
  /** The origin to point a browser at. */
  readonly url: string;
  /** A session token that authenticates, for seeding through the API. */
  readonly token: string;
  /** The binary under test, for a failure that needs to name it. */
  readonly bin: string;
  /** The server process, so a spec could signal it. */
  readonly pid: number;
  /** The fake model's endpoint, for a spec that configures a second instance. */
  readonly providerUrl: string;
  /** The default workspace's folder, for a spec that wants to look at what a
   * tool wrote. Its siblings are beside it, not inside it. */
  readonly workspace: string;
  /** The one-time code, on an unclaimed harness. `undefined` once claimed. */
  readonly setupCode: string | undefined;
  /**
   * Writes `config.yaml` behind the server's back, the way a hand edit does.
   *
   * The settings route validates and heals what it is given — that is its job —
   * so a spec about a file nobody validated cannot go through it. This is the
   * only way to reach the state a text editor can leave the install in, which
   * is exactly the state that used to stop it booting.
   *
   * Pair it with `POST /api/settings/reload`, which is what an operator presses
   * after editing the file.
   */
  writeConfig(patch: Record<string, unknown>): void;
  close(): Promise<void>;
}

/** What `--ready-file` puts on disk once the listener is bound. */
interface ReadyRecord {
  readonly port: number;
  readonly setupCode: string | null;
  readonly pid: number;
}

/**
 * The binary under test.
 *
 * `DARKWIRE_BIN` first, so CI can hand over a release build with the feature on;
 * the debug build otherwise, which is what a laptop has. A missing binary fails
 * here with a sentence rather than as twenty timed-out specs — the same service
 * the old harness's `resolveUiRoot` did for a missing `dist/`.
 */
function binary(): string {
  const named = process.env.DARKWIRE_BIN;
  const path =
    named === undefined || named === ''
      ? join(ROOT, 'target/debug/darkwire')
      : resolve(ROOT, named);
  if (!existsSync(path)) {
    throw new Error(
      `No darkwire binary at ${path}. Run \`cargo build -p darkwire --features test-hooks\`, ` +
        'or set DARKWIRE_BIN to one that exists.',
    );
  }
  return path;
}

/**
 * The built SPA.
 *
 * Passed with `--ui` rather than left to the copy embedded in the binary, so a
 * rebuilt bundle is under test without a rebuilt binary. "The UI is not built"
 * then fails at startup with the server's own sentence rather than as a blank
 * page forty assertions later.
 */
function uiRoot(): string {
  const root = join(ROOT, 'packages/web/dist');
  if (!existsSync(join(root, 'index.html'))) {
    throw new Error(`No built UI at ${root}. Run \`pnpm build\` first.`);
  }
  return root;
}

function seedWorkspace(root: string): void {
  for (const [relative, contents] of Object.entries(FIXTURE_FILES)) {
    const path = join(root, relative);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, contents);
  }
}

/**
 * The config the process boots on.
 *
 * Written as a partial rather than a schema-parsed whole: the server
 * default-fills and strips what it does not know, which is the behaviour a
 * hand-edited file gets and therefore the one a harness should rely on.
 */
function harnessConfig(
  options: HarnessOptions,
  workspaces: string,
  providerUrl: string,
): Record<string, unknown> {
  const patch = options.config ?? {};
  return {
    ...patch,
    workspaces,
    // Replaced wholesale when a spec names it, so a spec can say "nothing is
    // configured" by naming an empty map — which is a state the wizard has a
    // step for and which a harness that always added its own endpoint could
    // never reach.
    providers: patch.providers ?? {
      [INSTANCE_ID]: {
        type: 'ollama',
        // Labelled, so the providers list shows a name of its own rather than
        // the type's. `providers.spec.ts` picks a row by the text in it, and
        // two endpoints both reading `Ollama` would make every such locator
        // ambiguous.
        label: 'E2E model',
        apiBase: providerUrl,
        models: ['qwen3', 'gpt-oss'],
      },
    },
    agents: {
      ...(patch.agents ?? {}),
      list: {
        ...(patch.agents?.list ?? {}),
        // The default agent gets an explicit tool map, because permission is
        // per tool and absent means disabled — the seed covers the built-ins,
        // and `e2e_wait` is the binary's `test-hooks` tool, so nothing else
        // would ever enable it and every spec that waits would stall.
        //
        // `provider: 'auto'` rather than the instance id: a spec that replaces
        // the providers map with one of its own still has to resolve, and
        // naming `e2e` there would leave it unconfigured for the wrong reason.
        default: {
          provider: 'auto',
          model: 'qwen3',
          tools: { ...DEFAULT_AGENT_TOOLS, e2e_wait: 'allow' },
          ...(patch.agents?.list?.default ?? {}),
        },
      },
    },
    // The host is a config decision — the boot policy refuses a non-loopback
    // bind with authentication off, and it reads the config it was handed. The
    // port is not: `0` means "ask the OS", which the schema cannot express, so
    // it stays a flag exactly as it is for an operator.
    server: { ...(patch.server ?? {}), host: '127.0.0.1' },
    // The install's own answer, pinned for the same reason the browser's is:
    // once a session exists, `config.ui.locale` outranks `Accept-Language`, so
    // leaving it to the schema default would make the language the suite
    // asserts in depend on which side of sign-in a spec happens to be.
    ui: { ...(patch.ui ?? {}), locale: 'en' },
  };
}

/** Waits for the record the bound listener writes, or says what it saw instead. */
async function awaitReady(
  path: string,
  child: ChildProcess,
  log: () => string,
): Promise<ReadyRecord> {
  const deadline = Date.now() + READY_TIMEOUT_MS;
  for (;;) {
    if (existsSync(path)) {
      try {
        return JSON.parse(readFileSync(path, 'utf8')) as ReadyRecord;
      } catch {
        // Written and renamed atomically, so a parse failure means the write is
        // still in flight on a filesystem that reordered it. Look again.
      }
    }
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(`darkwire serve exited before it was ready.\n${log()}`);
    }
    if (Date.now() > deadline) {
      throw new Error(
        `darkwire serve did not bind within ${String(READY_TIMEOUT_MS)}ms.\n${log()}`,
      );
    }
    await new Promise((settle) => setTimeout(settle, 25));
  }
}

/** The session token the login route set, read off the cookie it sent. */
function tokenOf(setCookie: readonly string[]): string {
  for (const header of setCookie) {
    const match = /(?:^|;\s*)darkwire_session=([^;]+)/u.exec(header);
    if (match?.[1] !== undefined) return decodeURIComponent(match[1]);
  }
  throw new Error('The login response carried no darkwire_session cookie.');
}

export async function startHarness(
  options: HarnessOptions = {},
): Promise<Harness> {
  const bin = binary();
  const ui = uiRoot();
  const home = mkdtempSync(join(tmpdir(), 'darkwire-e2e-home-'));
  // The folder that holds them all, and the default's own folder inside it.
  // Every workspace is a directory in here, the default included, so the
  // fixture files go one level down rather than at the top.
  const workspaces = mkdtempSync(join(tmpdir(), 'darkwire-e2e-work-'));
  const workspace = join(workspaces, DEFAULT_WORKSPACE_ID);
  seedWorkspace(workspace);

  const provider = await startFakeProvider(ROUTES);

  const configFile = join(home, 'config.yaml');
  let config = harnessConfig(options, workspaces, provider.url);
  writeFileSync(configFile, JSON.stringify(config, null, 2));

  const readyFile = join(home, 'ready.json');
  const password = options.password === undefined ? PASSWORD : options.password;
  const child = spawn(
    bin,
    [
      'serve',
      '--home',
      home,
      '--workspaces',
      workspaces,
      '--host',
      '127.0.0.1',
      '--port',
      '0',
      '--ui',
      ui,
      '--ready-file',
      readyFile,
      ...(password === null ? [] : ['--password', password]),
    ],
    {
      // Not a spread of this process's environment, and that is deliberate: an
      // exported `OPENAI_API_KEY` in the operator's shell synthesises a
      // provider instance inside the server, which would silently undo the spec
      // that asserts an install with nothing configured. `PATH` is here because
      // the approval spec runs `node --version` through `exec`.
      env: {
        PATH: process.env.PATH ?? '',
        HOME: home,
        DARKWIRE_WORKSPACES: workspaces,
        DARKWIRE_TEST_HOOKS: '1',
        NO_COLOR: '1',
        DARKWIRE_LOG_LEVEL: 'warn',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    },
  );

  // Kept for the failure message and nowhere else: logs go to stderr precisely
  // so that reading them is a choice.
  let output = '';
  const collect = (piece: Buffer): void => {
    output = (output + piece.toString('utf8')).slice(-4_000);
  };
  child.stdout.on('data', collect);
  child.stderr.on('data', collect);

  const stop = async (): Promise<void> => {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
      const exited = await Promise.race([
        new Promise<boolean>((settle) => {
          child.once('exit', () => {
            settle(true);
          });
        }),
        new Promise<boolean>((settle) => {
          setTimeout(() => {
            settle(false);
          }, GRACE_MS);
        }),
      ]);
      if (!exited) child.kill('SIGKILL');
    }
    await provider.close();
    rmSync(home, { recursive: true, force: true });
    rmSync(workspaces, { recursive: true, force: true });
  };

  let ready: ReadyRecord;
  try {
    ready = await awaitReady(readyFile, child, () => output);
  } catch (error) {
    await stop();
    throw error;
  }

  const url = `http://127.0.0.1:${String(ready.port)}`;
  const token = await signIn(url, password);
  for (const session of options.sessions ?? []) {
    await seedSession(url, token, session);
  }
  for (const notification of options.notifications ?? []) {
    await seedNotification(url, token, notification);
  }

  return {
    url,
    token,
    bin,
    pid: ready.pid,
    providerUrl: provider.url,
    workspace,
    setupCode: ready.setupCode ?? undefined,
    writeConfig: (patch) => {
      config = { ...config, ...patch };
      writeFileSync(configFile, JSON.stringify(config, null, 2));
    },
    close: stop,
  };
}

/**
 * The token every seed travels on.
 *
 * An unclaimed install has no password to present, and nothing seeds one: the
 * setup spec is about the wizard, and a wizard with a session already open is
 * not the screen it exists for.
 */
async function signIn(url: string, password: string | null): Promise<string> {
  if (password === null) return '';
  const response = await fetch(`${url}/api/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: USERNAME, password }),
  });
  if (!response.ok) {
    throw new Error(
      `The harness login failed with ${String(response.status)}: ${await response.text()}`,
    );
  }
  return tokenOf(response.headers.getSetCookie());
}

/** One seed, through the hook route the `test-hooks` build serves. */
async function post(
  url: string,
  token: string,
  path: string,
  body: unknown,
): Promise<void> {
  const response = await fetch(`${url}${path}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      authorization: `Bearer ${token}`,
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(
      `${path} answered ${String(response.status)}: ${await response.text()}`,
    );
  }
}

async function seedSession(
  url: string,
  token: string,
  session: SeedSession,
): Promise<void> {
  await post(url, token, '/api/_test/sessions', {
    key: session.key,
    ...(session.title === undefined ? {} : { title: session.title }),
    messages: (session.turns ?? []).map((text, index) => ({
      role: index % 2 === 0 ? 'user' : 'assistant',
      text,
    })),
  });
}

async function seedNotification(
  url: string,
  token: string,
  notification: SeedNotification,
): Promise<void> {
  await post(url, token, '/api/_test/notifications', {
    title: notification.title,
    ...(notification.body === undefined ? {} : { body: notification.body }),
    ...(notification.level === undefined ? {} : { level: notification.level }),
  });
}
