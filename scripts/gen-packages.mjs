#!/usr/bin/env node
/**
 * Generates package.json + tsconfig.yaml for each workspace package.
 * Idempotent — safe to re-run when the package graph changes.
 *
 * The `development` export condition points at ./src/index.ts so `tsx` runs
 * the workspace with no build step; `default` points at built output.
 */
import { writeFileSync, mkdirSync, readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import prettier from 'prettier';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');

/**
 * One version for the whole workspace, taken from the root manifest.
 *
 * Nothing here is published any more — the product is a binary released from
 * GitHub — so this is no longer about a dependency range resolving. It is about
 * the version being answerable from one place. `crates/cli/tests/version.rs`
 * holds the root `package.json` equal to the root `Cargo.toml`, and this carries
 * that number into the four manifests below so a reader who opens one of them is
 * not told something else. Bumping the root and re-running this is the whole
 * release ceremony.
 */
const { version: VERSION } = JSON.parse(
  readFileSync(join(ROOT, 'package.json'), 'utf8'),
);

/**
 * Where the code lives, for a human reading a manifest.
 *
 * These used to be load-bearing: npm refuses a `--provenance` attestation unless
 * `repository` names the repo the workflow runs in. There is no publish and no
 * attestation now, so they are ordinary metadata and could go — they stay
 * because a manifest that says where it came from costs nothing and a workspace
 * package has no other header.
 */
const REPOSITORY_URL = 'git+https://github.com/therezor/GhostAI.git';
const HOMEPAGE = 'https://github.com/therezor/GhostAI';

/** Write a file through Prettier so regenerating never fails `format:check`. */
async function writeFormatted(path, contents) {
  const config = await prettier.resolveConfig(path);
  writeFileSync(
    path,
    await prettier.format(contents, { ...config, filepath: path }),
  );
}

/**
 * Injects `//` comments above named keys of a serialized tsconfig.
 *
 * JSON has no syntax for a comment and `tsconfig.yaml` does, so the rationale
 * for an override cannot survive `JSON.stringify`. Without this, re-running the
 * generator silently deletes the explanation for every deviation from the base
 * config — which is the half of the file worth reading.
 */
function withNotes(json, notes) {
  let out = json;
  for (const [key, note] of Object.entries(notes)) {
    const comment = note
      .split('\n')
      .map((line) => (line === '' ? '//' : `// ${line}`))
      .join('\n');
    out = out.replace(
      new RegExp(`^(\\s*)"${key}":`, 'm'),
      `\n${comment}\n$1"${key}":`,
    );
  }
  return out;
}

/**
 * The one manifest this generator still owns.
 *
 * It was twelve. `web` and `i18n` were always hand-maintained (see the patch
 * loop at the bottom), and the rest are Rust crates now, where Cargo owns the
 * equivalent file. A table with one row is kept as a table rather than inlined
 * because the *shape* is the point: whatever a second TypeScript package looks
 * like, it should not be a second hand-written manifest.
 */
/** @type {Record<string, { description: string; deps?: Record<string,string>; devDeps?: Record<string,string>; internal?: string[]; scripts?: Record<string,string>; compilerOptions?: Record<string, unknown>; tsconfigNotes?: Record<string,string> }>} */
const PACKAGES = {
  protocol: {
    description:
      'Zod schemas and derived types shared by every GhostAI package.',
    deps: { zod: '^4.0.0' },
    // The browser's half of the schema drift gate: one JSON Schema document
    // per registered schema, committed under `schema/` and diffed in CI.
    scripts: { 'schema:dump': 'tsx scripts/emit-schemas.ts' },
    compilerOptions: { isolatedDeclarations: false },
    tsconfigNotes: {
      isolatedDeclarations: [
        'The one package that cannot honour `isolatedDeclarations`. Every export',
        'here is a Zod schema whose type is the *result* of inference',
        '(`z.object({...})` → a deep `ZodObject<...>`), so the declaration emitter',
        'cannot write a signature without type-checking the expression: TS9010.',
        '',
        'The alternative is to hand-write a TS type beside every schema and',
        'annotate it as `z.ZodType<T>`, which reintroduces exactly the',
        'schema/type drift this package exists to remove. Inference is the more',
        'valuable half of the pair, so it wins here and only here; every other',
        'package keeps the flag on.',
      ].join('\n'),
    },
  },
};

/**
 * The workspace name for a package directory.
 *
 * A plain prefix now. There used to be a `PUBLISHED_AS` table beside this, and
 * it had exactly one row: `cli` published as `@ghostwire/ghostai`, because the
 * CLI was the one package a person typed. The command line is a Rust binary
 * released from GitHub rather than an npm package, so the exception it existed
 * for is gone and the directory name is the package name everywhere.
 */
function packageName(dir) {
  return `@ghostwire/${dir}`;
}

for (const [name, cfg] of Object.entries(PACKAGES)) {
  const dir = join(ROOT, 'packages', name);
  mkdirSync(join(dir, 'src'), { recursive: true });

  const dependencies = { ...(cfg.deps ?? {}) };
  for (const dep of cfg.internal ?? []) {
    dependencies[packageName(dep)] = 'workspace:*';
  }

  const pkg = {
    name: packageName(name),
    version: VERSION,
    description: cfg.description,
    type: 'module',
    // The honest flag, and the useful one. Nothing in this repository is
    // published, and `private` is what makes that a refusal rather than an
    // intention: `pnpm publish` declines to pack the package at all, so the
    // question cannot be reopened by a stray command in a workflow. It also
    // deletes a whole class of manifest field — `publishConfig`, `files`, the
    // source-map negations that kept a tarball small — none of which described
    // anything a workspace does.
    private: true,
    license: 'MIT',
    repository: {
      type: 'git',
      url: REPOSITORY_URL,
      directory: `packages/${name}`,
    },
    homepage: HOMEPAGE,
    bugs: `${HOMEPAGE}/issues`,
    exports: {
      // `development` points at `./src/index.ts`, which is what lets `tsx` and
      // Vite's dev server run the workspace with no build step; `default`
      // points at built output for everything else. Vite *sets* the condition,
      // so the two lines are not alternatives — they are the same package read
      // by two tools that disagree about whether a build has happened.
      '.': {
        development: './src/index.ts',
        types: './dist/index.d.ts',
        default: './dist/index.js',
      },
    },
    imports: { '#src/*': './src/*' },
    main: './dist/index.js',
    types: './dist/index.d.ts',
    scripts: {
      build: 'tsup && tsc -b',
      typecheck: 'tsc -b',
      test: 'vitest run',
      lint: 'eslint src test',
      ...(cfg.scripts ?? {}),
    },
    dependencies: Object.keys(dependencies).length
      ? Object.fromEntries(Object.entries(dependencies).sort())
      : undefined,
    devDependencies: cfg.devDeps
      ? Object.fromEntries(Object.entries(cfg.devDeps).sort())
      : undefined,
  };

  const tsconfig = {
    extends: '../../tsconfig.base.json',
    compilerOptions: {
      rootDir: './src',
      outDir: './dist',
      tsBuildInfoFile: './dist/.tsbuildinfo',
      ...(cfg.compilerOptions ?? {}),
    },
    include: ['src/**/*'],
    // The file, not the directory. `tsc -b` accepts either and resolves a
    // directory to the `tsconfig.yaml` inside it; Playwright's config loader
    // reads these same files to find path aliases and only accepts the explicit
    // form, so a reference written the short way makes the end-to-end suite
    // fail to start with an error about a package it never imported.
    references: (cfg.internal ?? []).map((dep) => ({
      path: `../${dep}/tsconfig.yaml`,
    })),
  };

  // Without a config of its own, a package running `vitest run` from its own
  // directory finds the *root* config and inherits its `projects` globs — which
  // are relative to the root, match nothing from inside `packages/x`, and fail
  // with "No projects were found". So `pnpm --filter @ghostwire/x test` was broken
  // everywhere, which is why the build plan noticed it for `protocol` alone: it
  // is the package a contributor is most likely to run on its own.
  //
  // The `name` is the second half. It is what `vitest --project <name>` selects
  // and what labels a line of output in a run that spans twelve packages.
  const vitest = `import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    name: '${name}',
    include: ['test/**/*.test.ts'],
  },
});
`;

  const tsup = `import { defineConfig } from 'tsup';

export default defineConfig({
  entry: ['src/index.ts'],
  format: ['esm'],
  target: 'node22',
  // tsc -b writes its declarations and .tsbuildinfo into this same dist, so
  // cleaning here would delete them and leave tsc believing they still exist.
  // Removing dist by hand is what forces a full rebuild of both tools.
  clean: false,
  dts: false, // tsc -b emits declarations; rollup-plugin-dts is the slow path
  sourcemap: true,
  // tsup rewrites \`node:sqlite\` to \`sqlite\` otherwise — a compatibility shim for
  // node versions older than 14.18 that turns a builtin into a missing package.
  // \`node:sqlite\` has no unprefixed form at all, so the rewrite is unloadable.
  // The default flips to false in tsup 9; this is only saying so early.
  removeNodeProtocol: false,
});
`;

  await writeFormatted(join(dir, 'package.json'), JSON.stringify(pkg, null, 2));
  await writeFormatted(
    join(dir, 'tsconfig.yaml'),
    withNotes(JSON.stringify(tsconfig, null, 2), cfg.tsconfigNotes ?? {}),
  );
  await writeFormatted(join(dir, 'tsup.config.ts'), tsup);
  await writeFormatted(join(dir, 'vitest.config.ts'), vitest);
  console.log(`generated packages/${name}`);
}

/**
 * The shared fields of the packages this generator does *not* own.
 *
 * Two packages are hand-maintained rather than generated. `web` is a Vite app
 * rather than a tsup library; `i18n` carries a `./web` subpath bundle and ships
 * its `locales/` directory beside `dist`. Regenerating either from the template
 * above would be writing a shape it does not have. (`e2e` is neither generated
 * nor patched: it is already `private`, it is never built, and its version is
 * `0.0.0` on purpose — nothing depends on it, so a number there would be a
 * number nobody reads.)
 *
 * What they do share is a version, and a version left behind used to be a real
 * failure rather than a cosmetic one — `pnpm publish` rewrote `workspace:*` to
 * an **exact** number, so a stale manifest published a dependency that resolved
 * to nothing, and `i18n` was once found sitting at `0.0.0` while everything else
 * had moved. Nothing publishes now, so the stake is smaller: what is left is
 * that `ghostai --version`, the root `Cargo.toml` and every manifest in the
 * repository should agree, because a reader who checks one of them has no way to
 * know it is the odd one out.
 *
 * `private: true` is written here for the same reason it is written above — it
 * is the flag that makes "this is not published" something pnpm enforces rather
 * than something a comment asserts.
 *
 * A package added here later needs adding to this list. The check that catches
 * a miss is `pnpm -r exec node -p "require('./package.json').version"`.
 */
for (const name of ['web', 'i18n']) {
  const path = join(ROOT, 'packages', name, 'package.json');
  // Everything the file already says, minus the three fields that only ever
  // described a tarball. Dropped rather than left alone, because a
  // `publishConfig` on a package that cannot be published is an instruction to
  // nobody — and the next person to read one would reasonably conclude that
  // this repository still ships to a registry.
  const { publishConfig, files, engines, ...pkg } = JSON.parse(
    readFileSync(path, 'utf8'),
  );

  const patched = {
    ...pkg,
    version: VERSION,
    private: true,
    repository: {
      type: 'git',
      url: REPOSITORY_URL,
      directory: `packages/${name}`,
    },
    homepage: HOMEPAGE,
    bugs: `${HOMEPAGE}/issues`,
  };
  await writeFormatted(path, JSON.stringify(patched, null, 2));
  console.log(`patched packages/${name} (shared fields only)`);
}
