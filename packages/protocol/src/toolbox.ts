/**
 * Toolboxes and containers: what an agent may call, and where it runs.
 *
 * Two manifests, approved separately, that deliberately do not know about each
 * other:
 *
 *  - A **toolbox** is the complete set of operations one agent may call. It
 *    names reusable operation definitions and the permission ceiling for each.
 *    It holds no image, no capabilities and no network, so nothing in it can
 *    widen a boundary.
 *  - A **container** is where command operations run: an image, the hardening
 *    around it, its resource budget, and whether agents share one instance. It
 *    holds no tool grants, so approving a place to run commands is not
 *    approving any particular command.
 *
 * Both live in an operator-installed policy directory rather than in
 * `agents.list.<id>`, because an agent's config is *editable* — through the
 * settings route, through a hand-edited file, and through anything that later
 * gains the ability to propose a patch. An agent carries a toolbox name, a
 * container name and its own egress request; every value that decides what an
 * image is or what privileges it holds has no representation in the config tree
 * at all.
 *
 * **Why not "tool".** That word is taken: a tool is a function the model can
 * call, with a schema and a risk band (`ToolDefinition`, `ToolRegistry`,
 * `agents.list.<id>.tools`). A toolbox is the *set* of those an agent was
 * granted, and an operation is the reviewed definition behind one.
 *
 * An operation is a fixed program and a reviewed argument mapping, never a
 * shell string. The JSON Schema it publishes is the same one its inputs are
 * validated against before a process starts, so "what the model was told it
 * could send" and "what the sandbox accepts" cannot drift apart.
 */

import { z } from 'zod';

import { ToolPermissionSchema } from './tools.js';

/**
 * The OCI runtime a container wants.
 *
 * `runc` is the default everywhere. `runsc` (gVisor) trades syscall
 * compatibility for a real isolation boundary and is Linux-only; `kata` is a
 * microVM. Availability is probed when a container is first needed rather than
 * assumed, so a definition naming an absent runtime fails that turn with a
 * sentence instead of the whole install.
 */
export const ContainerRuntimeSchema = z.enum(['runc', 'runsc', 'kata']);
export type ContainerRuntime = z.infer<typeof ContainerRuntimeSchema>;

export const ContainerCapsSchema = z.object({
  /** Almost always `['ALL']`. Listed rather than assumed so a definition is readable. */
  drop: z.array(z.string()).default(['ALL']),
  /**
   * Added back one at a time, with a reason. `NET_ADMIN` is deliberately not
   * grantable, because the egress gateway's rules live in a namespace the
   * container shares and must not be able to flush them.
   */
  add: z.array(z.string()).default([]),
});
export type ContainerCaps = z.infer<typeof ContainerCapsSchema>;

export const ContainerSecuritySchema = z.object({
  noNewPrivileges: z.boolean().default(true),
  /** `default` is the engine's own profile. `unconfined` is surfaced in the review. */
  seccomp: z.enum(['default', 'unconfined']).default('default'),
  readOnlyRoot: z.boolean().default(true),
  /** Mount specs, e.g. `/tmp:rw,nosuid,size=512m`. */
  tmpfs: z.array(z.string()).default([]),
  /** Rootless build needs `/dev/fuse`; nothing else should ask for a device. */
  devices: z.array(z.string()).default([]),
});
export type ContainerSecurity = z.infer<typeof ContainerSecuritySchema>;

export const ContainerLimitsSchema = z.object({
  memoryMb: z.coerce.number().int().min(0).default(2048),
  cpus: z.coerce.number().min(0).default(2),
  pidsMax: z.coerce.number().int().min(0).default(512),
  /** The engine's 64m default produces short writes in build and scan workloads. */
  shmSizeMb: z.coerce.number().int().min(0).default(256),
});
export type ContainerLimits = z.infer<typeof ContainerLimitsSchema>;

/**
 * One operation the toolbox permits an agent to invoke.
 *
 * `permission` is a ceiling: an agent's own `toolbox.tools` map may tighten it
 * to `ask` or `deny`, never widen it. That is the opposite of an ordinary
 * setting, and it is what lets an operator hand out one toolbox to several
 * agents without re-reviewing each of their configs.
 */
export const ToolGrantSchema = z
  .object({
    name: z.string().regex(/^[A-Za-z0-9_-]{1,64}$/),
    definition: z.string().regex(/^[a-z0-9][a-z0-9-]{0,63}$/),
    permission: ToolPermissionSchema.default('ask'),
  })
  .strict();
export type ToolGrant = z.infer<typeof ToolGrantSchema>;

/**
 * An approved toolbox is the complete callable surface of one agent.
 *
 * Every grant names an operation definition installed beside it rather than
 * carrying the definition inline, so one reviewed `git-status` is shared by
 * every toolbox that grants it and is reviewed once. The approval hash covers
 * the toolbox *and* every definition it names, so editing a shared definition
 * revokes each toolbox that reaches it.
 */
export const ToolboxSchema = z
  .object({
    schema: z.literal('ghostai.toolbox/1'),
    name: z.string().regex(/^[a-z0-9][a-z0-9-]{0,63}$/),
    label: z.string().default(''),
    version: z.string().default('0.0.0'),
    /** Caveats about the set as a whole. Model guidance, never an authorisation rule. */
    notes: z.string().default(''),
    tools: z.array(ToolGrantSchema),
  })
  .strict();
export type Toolbox = z.infer<typeof ToolboxSchema>;

/**
 * A reusable, operator-installed operation.
 *
 * The JSON Schema is validated offline at approval time and enforced again
 * before every call, so it can never reference anything the validator would
 * have to fetch.
 */
export const ToolOperationSchema = z
  .object({
    schema: z.literal('ghostai.tool/1'),
    description: z.string(),
    parameters: z.record(z.string(), z.unknown()),
    implementation: z.discriminatedUnion('kind', [
      z.object({ kind: z.literal('transcript') }).strict(),
      z
        .object({
          kind: z.literal('command'),
          executable: z.string().startsWith('/'),
          argv: z
            .array(
              z.union([
                z.string(),
                z
                  .object({
                    input: z.string(),
                    workspacePath: z.boolean().default(false),
                  })
                  .strict(),
              ]),
            )
            .default([]),
          argvInput: z.string().optional(),
        })
        .strict(),
      z
        .object({
          kind: z.literal('registered'),
          tool: z.string(),
          digest: z.string(),
        })
        .strict(),
    ]),
  })
  .strict();
export type ToolOperation = z.infer<typeof ToolOperationSchema>;

/**
 * Where command operations run, chosen independently of the toolbox.
 *
 * **There is no network here, deliberately.** Egress is the agent's own
 * request, configured in one place (`agents.list.<id>.container.network`), and
 * the fields below are what decide whether a restricted egress gateway can be
 * built around it at all: a root or non-numeric `user`, missing
 * `noNewPrivileges` or a capability that can forge packets each make the
 * gateway refuse. So an operator approving a definition is approving the
 * *shape* an agent's network request will be honoured in, not the request.
 */
export const ContainerDefinitionSchema = z
  .object({
    schema: z.literal('ghostai.container/1'),
    name: z.string().regex(/^[a-z0-9][a-z0-9-]{0,63}$/),
    /**
     * Must be digest-pinned: an immutable image ID or a registry digest. A tag
     * is a mutable pointer, and a container approved once and then silently
     * repointed is the approval gate defeated.
     */
    image: z.string().min(1),
    /** Share one instance across agents asking for the same egress. */
    shared: z.boolean().default(false),
    runtime: ContainerRuntimeSchema.default('runc'),
    /** Where the workspace is mounted inside the container. */
    workdir: z.string().default('/workspace'),
    /**
     * `uid:gid` inside the container, non-root by default. Matching the host
     * user is what keeps artefacts written into the workspace editable by the
     * host's own tools — root-owned output is the most common complaint about
     * this pattern.
     */
    user: z.string().default('1000:1000'),
    caps: ContainerCapsSchema.prefault({}),
    security: ContainerSecuritySchema.prefault({}),
    limits: ContainerLimitsSchema.prefault({}),
    /** Host env names passed through. Never a secret — those go via the proxy. */
    env: z.array(z.string()).default([]),
  })
  .strict();
export type ContainerDefinition = z.infer<typeof ContainerDefinitionSchema>;
