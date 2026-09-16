/**
 * Container policy: where an agent's built-in `exec` calls run.
 *
 * A container definition is an image, the hardening around it, its resource
 * budget, and whether agents share one instance. It grants nothing: an agent's
 * `tools` permission map is the whole authority for what the model may call,
 * with or without a container, so choosing where commands run cannot widen what
 * an agent can do.
 *
 * It lives in an operator-installed policy directory rather than in
 * `agents.list.<id>`, because an agent's config is *editable* — through the
 * settings route, through a hand-edited file, and through anything that later
 * gains the ability to propose a patch. An agent carries a container name and
 * its own egress request; every value that decides what an image is or what
 * privileges it holds has no representation in the config tree at all.
 */

import { z } from 'zod';

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
 * Where an agent's commands run.
 *
 * **There is no network here, deliberately.** Egress is the agent's own
 * request, configured in one place (`agents.list.<id>.container.network`), and
 * the fields below are what decide whether a restricted egress gateway can be
 * built around it at all: a root or non-numeric `user`, missing
 * `noNewPrivileges` or a capability that can forge packets each make the
 * gateway refuse. So an operator writing a definition is fixing the *shape*
 * an agent's network request will be honoured in, not the request.
 */
export const ContainerDefinitionSchema = z
  .object({
    schema: z.literal('ghostai.container/1'),
    name: z.string().regex(/^[a-z0-9][a-z0-9-]{0,63}$/),
    /**
     * Must be digest-pinned: an immutable image ID or a registry digest. A tag
     * is a mutable pointer, and a container installed once and then silently
     * repointed would run code nobody chose.
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
