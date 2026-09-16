/**
 * Environment policy: where an agent's built-in `exec` calls run.
 *
 * An environment is the place; a container is one kind of place. `kind` is what
 * says which, and today it has one arm, so every other field below still
 * describes a container and sits flat rather than inside the variant.
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
 * gains the ability to propose a patch. An agent carries an environment name and
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
  /**
   * Mount specs, e.g. `/tmp:rw,nosuid,size=512m`.
   *
   * **Defaults to a writable `/tmp`, and that is load-bearing.** `exec` records
   * its pid under `/tmp` so a timeout or a cancel has something to signal;
   * under a read-only root with no tmpfs the redirect fails, the script carries
   * on, and the kill script then reads an empty file and exits 0. Cancellation
   * silently stops working while the command keeps running. Every catalogue
   * definition set one and the default did not, so the hole was real and
   * invisible.
   *
   * 64m rather than the catalogue's 256m because a tmpfs is RAM and counts
   * against the memory limit. An image that wants more says so.
   */
  tmpfs: z.array(z.string()).default(['/tmp:rw,nosuid,size=64m']),
  /** Rootless build needs `/dev/fuse`; nothing else should ask for a device. */
  devices: z.array(z.string()).default([]),
});
export type ContainerSecurity = z.infer<typeof ContainerSecuritySchema>;

/**
 * What one container may spend. Zero means no limit, not zero.
 *
 * **Sized for a small board.** This runs on a Raspberry Pi 5, four cores and
 * 8 GB shared with the server and the model, and one place is now one container
 * rather than one per agent and session. Two gigabytes and two cores each was a
 * quarter of such a machine per container. Anyone on bigger hardware raises
 * them in the editor, which is what it is for.
 */
export const ContainerLimitsSchema = z.object({
  memoryMb: z.coerce.number().int().min(0).default(512),
  cpus: z.coerce.number().min(0).default(1),
  pidsMax: z.coerce.number().int().min(0).default(256),
  /** The engine's 64m default produces short writes in build and scan workloads. */
  shmSizeMb: z.coerce.number().int().min(0).default(64),
});
export type ContainerLimits = z.infer<typeof ContainerLimitsSchema>;

/**
 * Where an agent's commands run.
 *
 * **There is no network here, deliberately.** Egress is the agent's own
 * request, configured in one place (`agents.list.<id>.environment.network`), and
 * the fields below are what decide whether a restricted egress gateway can be
 * built around it at all: a root or non-numeric `user`, missing
 * `noNewPrivileges` or a capability that can forge packets each make the
 * gateway refuse. So an operator writing a definition is fixing the *shape*
 * an agent's network request will be honoured in, not the request.
 */
export const EnvironmentKindSchema = z.enum(['container']);
export type EnvironmentKind = z.infer<typeof EnvironmentKindSchema>;

export const EnvironmentDefinitionSchema = z
  .object({
    schema: z.literal('ghostai.environment/1'),
    /** What kind of place this is. Omitting it means a container. */
    kind: EnvironmentKindSchema.default('container'),
    name: z.string().regex(/^[a-z0-9][a-z0-9-]{0,63}$/),
    /**
     * @deprecated Read by nothing. What the model is told about where its
     * commands run is one section now, `agents.list.<id>.platformPrompt`, which
     * says both where they run and what is installed there.
     *
     * Still parsed because every installed definition predates the change and
     * the preset catalogue ships on its own release cycle. A definition setting
     * it is reported on its row. It goes one release after that.
     */
    prompt: z.string().optional(),
    /**
     * Must be digest-pinned: an immutable image ID or a registry digest. A tag
     * is a mutable pointer, and a container installed once and then silently
     * repointed would run code nobody chose.
     */
    image: z.string().min(1),
    /**
     * @deprecated Read by nothing. An environment is a place, and everything
     * asking for the same place gets the same container: an instance is keyed
     * on the workspace, the definition and the network, with neither the agent
     * nor the session in it.
     *
     * Parsed and reported for the same reason `prompt` above is.
     */
    shared: z.boolean().optional(),
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
export type EnvironmentDefinition = z.infer<typeof EnvironmentDefinitionSchema>;
