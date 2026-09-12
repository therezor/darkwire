/**
 * Writes one JSON Schema file per registered protocol schema.
 *
 * The output under `schema/` is the half of the drift gate the browser's
 * schemas supply: the Rust crate generates the same documents from its own
 * types, and a test compares the two after normalisation. The files are
 * committed, and `pnpm protocol:check` re-runs this and fails on a diff, so a
 * schema edit that was not followed by a regeneration is caught in CI rather
 * than by the Rust suite failing on a stale document.
 *
 * Input mode, because that is the direction the browser writes and the
 * contract the Rust side deserialises under: a field with a default may be
 * omitted, and a plain object carries no `additionalProperties: false`, since
 * unknown keys are stripped rather than refused. `$schema` is dropped for the
 * reason the server drops it from `components`: it is meaningful on a document
 * and noise on one entry of a pool.
 */

import { mkdirSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { z } from 'zod';

import { PROTOCOL_SCHEMAS } from '#src/schemas.js';

const OUT_DIR = fileURLToPath(new URL('../schema/', import.meta.url));

function emit(schema: z.ZodType): Record<string, unknown> {
  const { $schema: dialect, ...rest } = z.toJSONSchema(schema, {
    io: 'input',
  }) as Record<string, unknown> & { $schema?: unknown };
  return rest;
}

mkdirSync(OUT_DIR, { recursive: true });

const wanted = new Set<string>();
for (const [name, schema] of Object.entries(PROTOCOL_SCHEMAS)) {
  const file = `${name}.json`;
  wanted.add(file);
  writeFileSync(
    new URL(file, `file://${OUT_DIR}`),
    `${JSON.stringify(emit(schema), null, 2)}\n`,
  );
}

// A schema that left the registry must leave the directory too, or the Rust
// side keeps comparing against a document nothing publishes any more.
for (const file of readdirSync(OUT_DIR)) {
  if (!wanted.has(file)) rmSync(new URL(file, `file://${OUT_DIR}`));
}

console.log(`wrote ${String(wanted.size)} schemas to ${OUT_DIR}`);
