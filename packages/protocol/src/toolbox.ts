/**
 * A toolbox: an image, the tools inside it, and the policy for running it.
 *
 * The point of putting these in one operator-installed manifest rather than in
 * `agents.list.<id>` is that an agent's config is *editable* — through the
 * settings route, through a config file someone hand-edits, and through anything
 * that later gains the ability to propose a config patch. An image reference and
 * a capability set are not settings; they are the boundary that makes everything
 * else safe. So they live here, outside the config tree, and an agent carries a
 * toolbox *name* and nothing else that could widen it.
 *
 * **Why not "tool".** That word is taken: a tool is a function the model can
 * call, with a schema and a risk band (`ToolDefinition`, `ToolRegistry`,
 * `agents.list.<id>.tools`). A toolbox is the *environment* those calls run in —
 * a box of programs `exec` can reach. Keeping the two words apart is what stops
 * "the agent's tools" from meaning two things in one sentence.
 *
 * Four fields are load-bearing:
 *
 *  - **`image` must be digest-pinned.** A tag is a mutable pointer, and a toolbox
 *    approved once and then silently repointed is the whole approval gate
 *    defeated. The check lives in `ghostai-security`, where a refusal can carry
 *    a sentence explaining itself.
 *
 *  - **`network.maxMode` is a ceiling, not a setting.** An agent's own request is
 *    intersected with it, never unioned, so a toolbox shipping `none` cannot be
 *    given a network by any config anywhere.
 *
 *  - **`tools` is the toolset advertisement, and it is a list rather than prose.**
 *    A structured entry can be rendered in the UI, shown field by field in the
 *    install review, and composed into the prompt compactly. It replaced a single
 *    `brief` paragraph that could only be pasted whole and reviewed as a blob.
 *
 *  - **`notes` is for what a list cannot say.** "No browser and no JavaScript
 *    engine" is a caveat about the whole box, not about one program in it.
 *
 * What is deliberately *absent* is the boilerplate every toolbox would otherwise
 * repeat — that a shell is available, that only the workspace is mounted, where
 * truncated output goes. Those are properties of running in a toolbox at all, so
 * they are composed in code (`ghostai-agent`'s prompt builder) where they are
 * always true, rather than copied into every manifest where they can drift.
 */

import { z } from 'zod';

import { ToolPermissionSchema } from './tools.js';

/** How much network a toolbox is willing to permit at most. */
export const ToolboxNetworkModeSchema = z.enum(['none', 'allowlist', 'open']);
export type ToolboxNetworkMode = z.infer<typeof ToolboxNetworkModeSchema>;

/**
 * The OCI runtime a toolbox wants.
 *
 * `runc` is the default everywhere. `runsc` (gVisor) trades syscall
 * compatibility for a real isolation boundary and is Linux-only; `kata` is a
 * microVM. Availability is probed when a container is first needed rather than
 * assumed, so a toolbox naming an absent runtime fails that turn with a sentence
 * instead of the whole install.
 */
export const ToolboxRuntimeSchema = z.enum(['runc', 'runsc', 'kata']);

/**
 * One program in the box, as the model is told about it.
 *
 * Four fields rather than one paragraph, because they land in three different
 * places and a model reads them differently:
 *
 *  - **`use`** becomes the tool's own description. Imperative — "Search the web"
 *    — not a definition. The model already knows what `curl` is; what it does not
 *    know is what this box wants it *for*.
 *  - **`args`** becomes the description of the `args` field itself, which is the
 *    text a model is looking at while deciding what to put there. Naming the
 *    required flag here rather than in `use` is the difference between a model
 *    reading it and a model having read it.
 *  - **`example`** is a concrete argv. Models copy examples far more reliably
 *    than they follow prose, and this is the cheapest correctness win available:
 *    one array per entry, and the first call is usually right.
 *  - **`requiresArgs`** makes the schema itself refuse an empty call. Observed:
 *    a model called `fetch` with no URL, got a usage error, and gave up. A
 *    program that cannot do anything without an argument should say so where the
 *    validator can enforce it, not in a sentence.
 */
export const ToolboxEntrySchema = z.object({
  name: z.string().min(1),
  /** One imperative sentence. Becomes the tool's description. */
  use: z.string().default(''),
  /** What the arguments mean. Becomes the `args` field's own description. */
  args: z.string().default(''),
  /** A concrete argv the model can copy, e.g. `["--json","sqlite wal"]`. */
  example: z.array(z.string()).default([]),
  /** When true, a call with no arguments is refused by the schema. */
  requiresArgs: z.boolean().default(false),
  /**
   * What this program should be allowed to do, as the box's author sees it.
   *
   * A **default, not a ceiling** — unlike `network.maxMode` next door, an
   * agent's own `tools` map overrides it in either direction. The asymmetry is
   * deliberate: `maxMode` is a containment boundary that config must not be
   * able to widen, while this is a suggestion about a program that is reachable
   * through `exec` anyway. A toolbox that marked `nmap` as `ask` and could not
   * be overridden would be a manifest edit — and therefore a re-approval —
   * every time an operator wanted their own scanner to run unattended.
   *
   * `ask` by default because these are all `exec` underneath.
   */
  permission: ToolPermissionSchema.default('ask'),
});
export type ToolboxEntry = z.infer<typeof ToolboxEntrySchema>;

export const ToolboxCapsSchema = z.object({
  /** Almost always `['ALL']`. Listed rather than assumed so a manifest is readable. */
  drop: z.array(z.string()).default(['ALL']),
  /**
   * Added back one at a time, with a reason. `NET_RAW` is what `nmap -sS` needs;
   * `NET_ADMIN` is deliberately not grantable, because the egress gateway's rules
   * live in a namespace the container shares and must not be able to flush.
   */
  add: z.array(z.string()).default([]),
});

export const ToolboxSecuritySchema = z.object({
  noNewPrivileges: z.boolean().default(true),
  /** `default` is Docker's own profile. `unconfined` is surfaced in the review. */
  seccomp: z.enum(['default', 'unconfined']).default('default'),
  readOnlyRoot: z.boolean().default(true),
  /** Mount specs, e.g. `/tmp:rw,nosuid,size=512m`. */
  tmpfs: z.array(z.string()).default([]),
  /** Rootless build needs `/dev/fuse`; nothing else should ask for a device. */
  devices: z.array(z.string()).default([]),
});

export const ToolboxLimitsSchema = z.object({
  memoryMb: z.coerce.number().int().min(0).default(2048),
  cpus: z.coerce.number().min(0).default(2),
  pidsMax: z.coerce.number().int().min(0).default(512),
  /** Docker's 64m default produces short writes in build and scan workloads. */
  shmSizeMb: z.coerce.number().int().min(0).default(256),
});

export const ToolboxNetworkSchema = z.object({
  maxMode: ToolboxNetworkModeSchema.default('none'),
  /**
   * Resolvers the gateway permits on port 53. Without one, a CIDR allow-list
   * makes every hostname unresolvable — `127.0.0.11` is Docker's embedded
   * resolver and lives in the namespace the container shares with the gateway.
   */
  dns: z.array(z.string()).default(['127.0.0.11']),
  /**
   * Hostnames the credential/egress proxy permits, for a toolbox whose traffic
   * is all HTTP(S) — a builder fetching packages, say. Useless for raw scanning,
   * which is why a pentest toolbox scopes by CIDR instead.
   */
  proxyAllowHosts: z.array(z.string()).default([]),
});

export const ToolboxSchema = z.object({
  /** Bumped only for a breaking manifest change; refused when unrecognised. */
  schema: z.literal('ghostai.toolbox/1'),
  name: z.string().min(1).max(64),
  version: z.string().default('0.0.0'),
  /** Shown in the UI. Empty falls back to the name. */
  label: z.string().default(''),

  /** What is in the box. See the module header on why this is a list. */
  tools: z.array(ToolboxEntrySchema).default([]),
  /** Caveats about the box as a whole, appended to the prompt section. */
  notes: z.string().default(''),
  /**
   * How the model is told what is in here.
   *
   * `prompt` is one section of about forty tokens, whatever the box holds, and
   * relies on the model reading its instructions. `tools` additionally
   * materialises every `tools[]` entry as a real callable schema beside
   * `read_file` and `exec` — roughly 60–80 tokens each, every request of every
   * turn, and worth it for a model that reads its tool list far more attentively
   * than its prose. See `toolboxTools` for the failure that motivates it.
   */
  expose: z.enum(['prompt', 'tools']).default('prompt'),

  /** Must be digest-pinned. Validated in `ghostai-security`. */
  image: z.string().min(1),
  runtime: ToolboxRuntimeSchema.default('runc'),
  /** Where the workspace is mounted inside the container. */
  workdir: z.string().default('/workspace'),
  /**
   * `uid:gid` inside the container. Matching the host user is what keeps
   * artefacts written into the workspace editable by the host's own tools —
   * root-owned output is the most common complaint about this whole pattern.
   */
  user: z.string().default(''),

  caps: ToolboxCapsSchema.prefault({}),
  security: ToolboxSecuritySchema.prefault({}),
  limits: ToolboxLimitsSchema.prefault({}),
  network: ToolboxNetworkSchema.prefault({}),
  /** Host env names passed through. Never a secret — those go via the proxy. */
  env: z.array(z.string()).default([]),
});
export type Toolbox = z.infer<typeof ToolboxSchema>;
