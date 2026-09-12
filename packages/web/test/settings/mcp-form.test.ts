import { createWebI18n } from '@ghostwire/i18n/web';
import { McpServerConfigSchema } from '@ghostwire/protocol';
import { describe, expect, it } from 'vitest';

import {
  EMPTY_MCP_FORM,
  proposeServerId,
  toDeleteMcpPatch,
  toMcpEnabledPatch,
  toMcpForm,
  toMcpPatch,
  transportOf,
  type McpForm,
} from '@/settings/mcp-form.js';

const t = createWebI18n('en').getFixedT(null, 'web');

function stdioForm(overrides: Partial<McpForm> = {}): McpForm {
  return { ...EMPTY_MCP_FORM, command: 'npx', ...overrides };
}

function httpForm(overrides: Partial<McpForm> = {}): McpForm {
  return {
    ...EMPTY_MCP_FORM,
    transport: 'streamableHttp',
    url: 'https://mcp.example.test/mcp',
    ...overrides,
  };
}

/** The `tools.mcpServers.<id>` half of a patch, for a case that expects one. */
function entryOf(form: McpForm, id = 'demo') {
  const result = toMcpPatch(id, form, t);
  expect(result.ok, result.ok ? '' : JSON.stringify(result.errors)).toBe(true);
  if (!result.ok) throw new Error('unreachable');
  return result.patch.tools?.mcpServers?.[id];
}

describe('transportOf', () => {
  it('runs the same inference the client does', () => {
    // `type` is optional in the schema, so an entry that left it out has to be
    // read the way `resolveSpec` reads it or the editor shows the wrong half.
    expect(transportOf(McpServerConfigSchema.parse({ command: 'npx' }))).toBe(
      'stdio',
    );
    expect(
      transportOf(McpServerConfigSchema.parse({ url: 'https://a.test/mcp' })),
    ).toBe('streamableHttp');
    expect(
      transportOf(
        McpServerConfigSchema.parse({ type: 'sse', url: 'https://a.test/sse' }),
      ),
    ).toBe('sse');
    expect(transportOf(undefined)).toBe('stdio');
  });
});

describe('toMcpForm', () => {
  it('shows an empty box for the schema default of every tool', () => {
    // `['*']` is a character an operator cannot explain; an empty box says the
    // same thing.
    const form = toMcpForm(McpServerConfigSchema.parse({ command: 'npx' }));
    expect(form.enabledTools).toBe('');
  });

  it('lists a narrowed tool set one per line', () => {
    const form = toMcpForm(
      McpServerConfigSchema.parse({
        command: 'npx',
        enabledTools: ['read', 'write'],
      }),
    );
    expect(form.enabledTools).toBe('read\nwrite');
  });

  it('renders records as NAME=value', () => {
    const form = toMcpForm(
      McpServerConfigSchema.parse({
        command: 'npx',
        env: { TOKEN: 'abc', HOME: '/tmp' },
      }),
    );
    expect(form.env).toBe('TOKEN=abc\nHOME=/tmp');
  });

  it('reads the OAuth block back into the form', () => {
    const form = toMcpForm(
      McpServerConfigSchema.parse({
        url: 'https://a.test/mcp',
        oauth: {
          authUrl: 'https://auth.test/a',
          tokenUrl: 'https://auth.test/t',
          clientId: 'x',
          scopes: ['read'],
        },
      }),
    );
    expect(form.usesOAuth).toBe(true);
    expect(form.scopes).toBe('read');
  });
});

describe('toMcpPatch', () => {
  it('sends a stdio server with its arguments and environment', () => {
    const entry = entryOf(
      stdioForm({ args: '-y\nserver', env: 'TOKEN=abc', command: ' npx ' }),
    );
    expect(entry).toMatchObject({
      type: 'stdio',
      command: 'npx',
      args: ['-y', 'server'],
      env: { TOKEN: 'abc' },
    });
  });

  it('clears the other transport half, so the entry never names both', () => {
    // `resolveSpec` refuses an entry with a command *and* a url, so switching
    // transports has to take the old one back out. An omitted field would mean
    // "not mentioned" and leave a config that refuses itself.
    const entry = entryOf(httpForm({ command: 'npx', args: '-y', env: 'A=1' }));
    expect(entry).toMatchObject({ url: 'https://mcp.example.test/mcp' });
    expect(entry?.command).toBe('');
    expect(entry?.args).toEqual([]);
    expect(entry?.env).toEqual({});
  });

  it('turns an empty tool list back into the schema wildcard', () => {
    expect(entryOf(stdioForm())?.enabledTools).toEqual(['*']);
    expect(entryOf(stdioForm({ enabledTools: 'read' }))?.enabledTools).toEqual([
      'read',
    ]);
  });

  it('sends a null to say a server does not use OAuth', () => {
    // `oauth` is genuinely optional, so "unset" is a real state that needs a
    // way to be said — an absent key would mean "not mentioned".
    expect(entryOf(httpForm())?.oauth).toBeNull();
    expect(entryOf(stdioForm())?.oauth).toBeNull();
  });

  it('sends the OAuth block when the switch is on', () => {
    const entry = entryOf(
      httpForm({
        usesOAuth: true,
        authUrl: 'https://auth.test/a',
        tokenUrl: 'https://auth.test/t',
        clientId: 'client',
        scopes: 'read\nwrite',
      }),
    );
    expect(entry?.oauth).toMatchObject({
      authUrl: 'https://auth.test/a',
      clientId: 'client',
      scopes: ['read', 'write'],
    });
  });

  it('converts the timeout out of the seconds an operator thinks in', () => {
    expect(
      entryOf(stdioForm({ toolTimeoutSeconds: '30' }))?.toolTimeoutMs,
    ).toBe(30_000);
  });

  it('refuses a stdio server with no command', () => {
    const result = toMcpPatch('demo', EMPTY_MCP_FORM, t);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.command).toBeDefined();
  });

  it('refuses a URL that is not one, and one this client will not dial', () => {
    for (const url of ['not a url', 'file:///etc/passwd', 'ftp://a.test/x']) {
      const result = toMcpPatch('demo', httpForm({ url }), t);
      expect(result.ok, url).toBe(false);
      if (!result.ok) expect(result.errors.url).toBeDefined();
    }
  });

  it('accepts a loopback URL, which is the common case', () => {
    expect(
      toMcpPatch('demo', httpForm({ url: 'http://127.0.0.1:3001/mcp' }), t).ok,
    ).toBe(true);
  });

  it('names every empty OAuth field rather than one', () => {
    // All three are `.min(1)` on the wire, so an empty one is a save the server
    // refuses without saying which field.
    const result = toMcpPatch('demo', httpForm({ usesOAuth: true }), t);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(Object.keys(result.errors).sort()).toEqual([
        'authUrl',
        'clientId',
        'tokenUrl',
      ]);
    }
  });

  it('does not ask about OAuth on a transport that cannot carry it', () => {
    // A stdio server is a child process and has nobody to authorize with.
    expect(toMcpPatch('demo', stdioForm({ usesOAuth: true }), t).ok).toBe(true);
  });

  it('refuses a negative timeout', () => {
    expect(
      toMcpPatch('demo', stdioForm({ toolTimeoutSeconds: '-1' }), t).ok,
    ).toBe(false);
  });
});

describe('the list patches', () => {
  it('switches one off without restating the rest of it', () => {
    expect(toMcpEnabledPatch('demo', false)).toEqual({
      tools: { mcpServers: { demo: { enabled: false } } },
    });
  });

  it('deletes with the one token the merge reads as removal', () => {
    expect(toDeleteMcpPatch('demo')).toEqual({
      tools: { mcpServers: { demo: null } },
    });
  });
});

describe('proposeServerId', () => {
  it('makes an id out of what was typed', () => {
    expect(proposeServerId('GitHub Issues', [])).toBe('github-issues');
  });

  it('avoids one that is taken', () => {
    expect(proposeServerId('github', ['github'])).toBe('github-2');
    expect(proposeServerId('github', ['github', 'github-2'])).toBe('github-3');
  });

  it('always produces something usable', () => {
    expect(proposeServerId('!!!', [])).toBe('server');
    expect(proposeServerId('', [])).toBe('server');
    expect(proposeServerId('a'.repeat(80), []).length).toBeLessThanOrEqual(32);
  });
});
