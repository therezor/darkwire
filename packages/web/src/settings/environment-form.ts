/**
 * The environment editor's form, and the definition it becomes.
 *
 * Pure functions either side of one shape, the way `mcp-form.ts` is: the editor
 * holds strings because every control produces one, and the save turns them back
 * into the definition the server parses. Nothing here talks to the network.
 *
 * **It produces a whole definition, not a patch.** Every other settings form in
 * this package builds a `ConfigPatch`, because `config.yaml` is a tree that
 * several screens write into. A definition is a file, the file *is* the policy,
 * and a partial one has no meaning: the server writes what it is given and the
 * name is the filename.
 *
 * The lists are newline-separated rather than comma-separated, which is where
 * this differs from `parseList` in `agents-form.ts`. A tmpfs spec has commas
 * inside one entry — `/tmp:rw,nosuid,size=512m` — so splitting on them would cut
 * a single mount into three broken ones.
 */

import type { TFunction } from 'i18next';

import {
  EnvironmentDefinitionSchema,
  type ContainerRuntime,
  type EnvironmentDefinition,
  type EnvironmentSummary,
} from '@ghostwire/protocol';

/**
 * Derived rather than imported.
 *
 * The seccomp profile is an inline enum inside `ContainerSecuritySchema`, and
 * the schema-parity gate exempts it on exactly that basis: the browser spells it
 * as a literal list inside a field. Naming a schema for it here to get a type
 * would make that exemption a lie.
 */
type SeccompProfile = EnvironmentDefinition['security']['seccomp'];

/** The runtimes a definition may name, in the order the picker offers them. */
export const CONTAINER_RUNTIMES: readonly ContainerRuntime[] = [
  'runc',
  'runsc',
  'kata',
];

/** The two seccomp profiles. `unconfined` is surfaced, never refused. */
export const SECCOMP_PROFILES: readonly SeccompProfile[] = [
  'default',
  'unconfined',
];

/**
 * One definition, as text.
 *
 * The numbers are strings too. A number input hands back `''` while someone is
 * clearing it, and a form field typed `number` would have to invent a value for
 * that moment; keeping it text means the empty box is representable and the
 * error is raised on save, where it can be read.
 */
export interface EnvironmentForm {
  readonly name: string;
  readonly image: string;
  readonly runtime: ContainerRuntime;
  readonly workdir: string;
  readonly user: string;
  readonly memoryMb: string;
  readonly cpus: string;
  readonly pidsMax: string;
  readonly shmSizeMb: string;
  readonly noNewPrivileges: boolean;
  readonly seccomp: SeccompProfile;
  readonly readOnlyRoot: boolean;
  readonly tmpfs: string;
  readonly devices: string;
  readonly capsDrop: string;
  readonly capsAdd: string;
  readonly env: string;
}

/** `"a\nb\n\n c "` → `['a','b','c']`. Empty lines dropped, order kept. */
export function parseLines(value: string): string[] {
  return value
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line !== '');
}

/**
 * The schema's own defaults, which is what a new definition starts from.
 *
 * That is the hardened shape: every capability dropped, no new privileges, a
 * read-only root and a non-root uid. An operator weakening one of those is
 * making a decision; one who never opened the section has made none.
 *
 * Seeded through the schema rather than by restating the defaults, so the two
 * cannot drift. `name` and `image` are then blanked, because they are the two
 * fields with no useful default and the schema requires both to parse at all: a
 * placeholder left in either box is a value somebody saves by accident.
 */
export function emptyEnvironmentForm(): EnvironmentForm {
  const defaults = EnvironmentDefinitionSchema.parse({
    schema: 'ghostai.environment/1',
    name: 'placeholder',
    image: 'placeholder',
  });
  return { ...toEnvironmentForm(defaults), name: '', image: '' };
}

export function toEnvironmentForm(
  definition: EnvironmentDefinition,
): EnvironmentForm {
  return {
    name: definition.name,
    image: definition.image,
    runtime: definition.runtime,
    workdir: definition.workdir,
    user: definition.user,
    memoryMb: String(definition.limits.memoryMb),
    cpus: String(definition.limits.cpus),
    pidsMax: String(definition.limits.pidsMax),
    shmSizeMb: String(definition.limits.shmSizeMb),
    noNewPrivileges: definition.security.noNewPrivileges,
    seccomp: definition.security.seccomp,
    readOnlyRoot: definition.security.readOnlyRoot,
    tmpfs: definition.security.tmpfs.join('\n'),
    devices: definition.security.devices.join('\n'),
    capsDrop: definition.caps.drop.join('\n'),
    capsAdd: definition.caps.add.join('\n'),
    env: definition.env.join('\n'),
  };
}

/** The form of an installed environment, or a fresh one when it did not parse. */
export function formOf(
  summary: EnvironmentSummary | undefined,
): EnvironmentForm {
  if (summary?.definition === undefined) return emptyEnvironmentForm();
  return toEnvironmentForm(summary.definition);
}

/**
 * The fields a save could not use, each with the sentence to show under it.
 *
 * Named rather than a string-keyed record so a typo in a field name is a
 * compile error: the editor reads these back by name, and a key nothing renders
 * is a validation that silently does nothing.
 */
export interface EnvironmentErrors {
  readonly name?: string;
  readonly memoryMb?: string;
  readonly cpus?: string;
  readonly pidsMax?: string;
  readonly shmSizeMb?: string;
}

/** The four boxes that must hold a number. */
const NUMBER_FIELDS = ['memoryMb', 'cpus', 'pidsMax', 'shmSizeMb'] as const;

export type EnvironmentResult =
  | { readonly ok: true; readonly definition: EnvironmentDefinition }
  | { readonly ok: false; readonly errors: EnvironmentErrors };

/**
 * The definition this form describes, or the fields that are wrong.
 *
 * Only the two things the schema cannot say in a sentence a person can act on
 * are checked here: a name that is not a slug, and a number box that is not a
 * number. Everything else is left to the server, deliberately — the refusals
 * that matter (an image that is not digest-pinned, a capability that is never
 * grantable) are one sentence each, already written for a human, and
 * reimplementing them in the browser is how the two come to disagree.
 */
export function toEnvironmentDefinition(
  form: EnvironmentForm,
  t: TFunction,
): EnvironmentResult {
  const errors: {
    -readonly [K in keyof EnvironmentErrors]: string;
  } = {};
  if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(form.name)) {
    errors.name = t('settings.environments.nameInvalid');
  }
  for (const field of NUMBER_FIELDS) {
    const raw = form[field];
    const value = Number(raw);
    if (raw.trim() === '' || !Number.isFinite(value) || value < 0) {
      errors[field] = t('settings.environments.numberInvalid');
    }
  }
  if (Object.keys(errors).length > 0) return { ok: false, errors };

  return {
    ok: true,
    definition: {
      schema: 'ghostai.environment/1',
      kind: 'container',
      name: form.name,
      image: form.image.trim(),
      runtime: form.runtime,
      workdir: form.workdir.trim(),
      user: form.user.trim(),
      caps: { drop: parseLines(form.capsDrop), add: parseLines(form.capsAdd) },
      security: {
        noNewPrivileges: form.noNewPrivileges,
        seccomp: form.seccomp,
        readOnlyRoot: form.readOnlyRoot,
        tmpfs: parseLines(form.tmpfs),
        devices: parseLines(form.devices),
      },
      limits: {
        memoryMb: Math.trunc(Number(form.memoryMb)),
        cpus: Number(form.cpus),
        pidsMax: Math.trunc(Number(form.pidsMax)),
        shmSizeMb: Math.trunc(Number(form.shmSizeMb)),
      },
      env: parseLines(form.env),
    },
  };
}
