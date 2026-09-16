/**
 * An extension: a manifest, the code it names, and what it says it contributes.
 *
 * The shape is deliberately the container manifest's, because the two solve the
 * same problem. An extension's capabilities are not *settings* — they are the
 * boundary that decides what running it means — so they live in a file outside
 * the config tree, and `config.extensions` carries an id, an on/off and a block
 * of the extension's own settings, none of which can widen anything.
 *
 * ## Two schema versions, one shape
 *
 * `darkwire.extension/1` named `entry`: a module the host loaded into its own
 * process. `darkwire.extension/2` names `command`: an argv the host spawns as a
 * child process speaking JSON-RPC over its stdio. The difference is a process
 * boundary, so the two cannot be loaded by the same host — a v1 bundle reaching
 * a host that spawns lands on its row as `failed` with a sentence saying so.
 *
 * Both versions parse into *one* object rather than a discriminated union, and
 * that is deliberate: a manifest whose version this build cannot run still has
 * to produce an id, a label and a `contributes` list, because those are what the
 * row explaining the refusal is made of. A union would make the refusal a parse
 * error, and a parse error has no id to hang a row on.
 *
 * Four fields are load-bearing:
 *
 *  - **`id` is the directory name, and the prefix for everything.** A channel,
 *    a provider, a command and (through `extensionToolName`) a tool all have to
 *    be named `<id>` or `<id>-<suffix>`, so one namespace check covers four
 *    registries and two extensions cannot silently fight over a name.
 *
 *  - **`command` is an argv, never a shell line.** `argv[0]` is either a bare
 *    program name resolved on the host `PATH` (`node`, `python3`) or a path
 *    relative to the install directory, which `darkwire-security` resolves and
 *    refuses if it escapes — the same rule `entry` had, and for the same reason:
 *    the approval digest covers the directory, so the code that runs has to live
 *    inside it. A shell binary is refused outright, because a shell turns the
 *    rest of the argv back into a string somebody can inject into.
 *
 *  - **`env` is an allow-list of host variable *names*, never values.** A child
 *    gets `PATH`, `HOME`, `LANG` and `TMPDIR` plus whatever it names here, and
 *    nothing else — so the provider API key in `darkwire serve`'s own environment
 *    does not silently land inside third-party code.
 *
 *  - **`contributes` is disclosure, not enforcement.** It is what the approval
 *    screen shows the operator: this extension wants to add channels and
 *    commands. The host drops a registration whose kind is not listed, which
 *    keeps the declaration honest — but an approved extension is a process
 *    running under the operator's own account, so nothing here is a security
 *    boundary and `docs/security.md` says so in as many words.
 *
 * What is deliberately absent is a `permissions` block. A tool an extension
 * registers is granted exactly the way every other tool is: per agent, in
 * `agents.list.<id>.tools`, where absent means disabled. A second permission
 * vocabulary reachable from a manifest would be a way to grant something the
 * operator never enabled.
 */

import { z } from 'zod';

/**
 * The registries an extension may write into.
 *
 * `context` is the system-prompt contributor seam — the one memory and skills
 * arrive through — and is named for the interface rather than for "prompt",
 * because what it contributes is a section of context and the prompt is what
 * the sections add up to.
 */
export const ExtensionContributionSchema = z.enum([
  'tools',
  'channels',
  'providers',
  'context',
  'commands',
]);
export type ExtensionContribution = z.infer<typeof ExtensionContributionSchema>;

/**
 * The manifest format tag.
 *
 * An enum of the two rather than a literal of the current one, so a v1 bundle
 * still parses far enough to be *described* on the panel that refuses it.
 */
export const ExtensionSchemaVersionSchema = z.enum([
  'darkwire.extension/1',
  'darkwire.extension/2',
]);
export type ExtensionSchemaVersion = z.infer<
  typeof ExtensionSchemaVersionSchema
>;

/**
 * The host variables a child inherits when the manifest names none.
 *
 * Enough to find a program and behave like one run from a terminal, and nothing
 * that could be a credential. The same reasoning, and very nearly the same list,
 * as the MCP stdio connector's inherited set.
 */
export const DEFAULT_EXTENSION_ENV = [
  'PATH',
  'HOME',
  'LANG',
  'TMPDIR',
] as const;

/** Which parameter carries the output cap on a provider's wire. */
export const ExtensionMaxTokensParamSchema = z.enum([
  'max_tokens',
  'max_completion_tokens',
]);
export type ExtensionMaxTokensParam = z.infer<
  typeof ExtensionMaxTokensParamSchema
>;

/**
 * A provider type an extension contributes, as manifest data.
 *
 * Data rather than code, and that is the whole design: registering a provider
 * by handing the host a wire adapter — a function — is something an
 * out-of-process extension cannot do, and would route every generated token
 * through two extra hops if it could. An OpenAI-compatible endpoint needs
 * no code at all, which `docs/extensions.md` already calls the common case, so
 * what an extension contributes is the table entry and the host supplies the
 * adapter it already ships.
 *
 * `wire` is a plain string, not an enum, and that is the load-bearing part: a
 * manifest naming a wire this build has no adapter for has to *parse*, so the
 * host can put "declares a provider on an unknown wire" on the extension's row
 * and register the rest. An enum would make it a manifest that does not load.
 */
export const ExtensionProviderSpecSchema = z.object({
  /** The registry id. Namespaced to the extension, like every other id. */
  id: z.string().min(1),
  /** For a person. */
  displayName: z.string().default(''),
  /** Which adapter speaks to it. Unknown here is a warning, not a refusal. */
  wire: z.string().default('openai-chat'),
  /** Substrings that identify this provider from a bare model name. */
  keywords: z.array(z.string()).default([]),
  /** Environment variable consulted when the vault holds no key. */
  envKey: z.string().default(''),
  /** Used when config supplies no `apiBase`. Empty means one is required. */
  defaultApiBase: z.string().default(''),
  /** Reachable without credentials, on this machine or the LAN. */
  isLocal: z.boolean().default(false),
  /** Fronts many upstream models, so it is matched by key/base, not model. */
  isGateway: z.boolean().default(false),
  /** Credentials arrive from an OAuth flow rather than an API key. */
  isOAuth: z.boolean().default(false),
  /** An API key prefix that identifies this provider unambiguously. */
  detectByKeyPrefix: z.string().default(''),
  /** A substring of `apiBase` that identifies this provider. */
  detectByBaseKeyword: z.string().default(''),
  /** The endpoint wants bare model ids: `openai/gpt-4o` is sent as `gpt-4o`. */
  stripModelPrefix: z.boolean().default(false),
  /** The prefix is part of the model id and must survive: `nvidia/foo`. */
  preserveModelPrefix: z.boolean().default(false),
  /** Newer OpenAI models reject `max_tokens` and require the longer name. */
  maxTokensParam: ExtensionMaxTokensParamSchema.default('max_tokens'),
  /** Headers every request carries: gateway attribution, API versions. */
  defaultHeaders: z.record(z.string(), z.string()).default({}),
  /** The endpoint understands `prompt_cache_key`. */
  supportsPromptCaching: z.boolean().default(false),
  /** The endpoint answers `GET /models` with a catalogue. */
  supportsModelListing: z.boolean().default(false),
});
export type ExtensionProviderSpec = z.infer<typeof ExtensionProviderSpecSchema>;

export const ExtensionManifestSchema = z.object({
  /** Which of the two contracts above this manifest is written against. */
  schema: ExtensionSchemaVersionSchema,
  /** Also the directory name, and the prefix every contributed id carries. */
  id: z.string().min(1).max(40),
  version: z.string().default('0.0.0'),
  /** Shown in the UI. Empty falls back to the id. */
  label: z.string().default(''),
  /** One sentence, shown beside the Approve button. */
  description: z.string().default(''),
  /**
   * The ESM module a `darkwire.extension/1` host imported. Read on v1 only.
   *
   * Kept so that a v1 manifest still describes itself on the row that refuses
   * it; a v2 manifest leaves it at its default and nothing reads it.
   */
  entry: z.string().min(1).default('dist/index.js'),
  /**
   * The argv a `darkwire.extension/2` host spawns. Read on v2 only.
   *
   * Never a shell line: element zero is the program and the rest are its
   * arguments, exactly as they reach `execve`. Empty on a v2 manifest is a
   * refusal — with a sentence, from `darkwire-security`, not a parse error,
   * because the row still needs the id.
   */
  command: z.array(z.string()).default([]),
  /**
   * Host environment variable *names* the child may additionally inherit.
   *
   * Names, never values: a manifest cannot set a variable, only ask for one the
   * host already has. See the module header for what the default four buy.
   */
  env: z.array(z.string()).default([...DEFAULT_EXTENSION_ENV]),
  /** Provider types this extension contributes, as data. */
  providers: z.array(ExtensionProviderSpecSchema).default([]),
  /** What the operator is approving. See the module header. */
  contributes: z.array(ExtensionContributionSchema).default([]),
  engines: z
    .object({
      /** A semver range this build must satisfy. Empty means any. */
      darkwire: z.string().default(''),
    })
    .prefault({}),
});
export type ExtensionManifest = z.infer<typeof ExtensionManifestSchema>;
