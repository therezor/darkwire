// @ts-check
import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import importX from 'eslint-plugin-import-x';

/**
 * Package layering is enforced by two mechanisms working together:
 *
 *  1. pnpm's isolated node_modules. A package can only resolve `@darkwire/x`
 *     if it declares it in `dependencies`, so the dependency graph in the
 *     package manifests *is* the layer graph — an undeclared import fails to
 *     resolve rather than merely failing lint.
 *
 *  2. The rule below, which bans deep relative imports that would reach
 *     across a package boundary and bypass mechanism (1).
 *
 * Together these make an import cycle between packages impossible to
 * introduce accidentally, which matters because the agent loop must never
 * reach back into the HTTP server.
 */
const crossPackageImportRule = {
  'no-restricted-imports': [
    'error',
    {
      patterns: [
        {
          group: ['../../*'],
          message:
            'Deep relative imports across package boundaries are banned. Import the package by name (@darkwire/<pkg>) and declare it in package.json dependencies.',
        },
      ],
    },
  ],
};

/**
 * The Google TypeScript Style Guide, reduced to the subset a linter can check.
 *
 * https://google.github.io/styleguide/tsguide.html
 *
 * Rules the guide states that are already covered elsewhere are not repeated
 * here: `strictTypeChecked` supplies the bans on `any`, `namespace`, `require`,
 * throwing non-`Error`s and empty object types, and prettier supplies quoting,
 * indentation, semicolons and the 80-column limit. What follows is the
 * remainder — the rules neither of those two would have given us.
 *
 * Four of the guide's rules are deliberately *not* checked here, so that "the
 * guide says X and we don't check X" is an answered question rather than an
 * open one:
 *
 *  - **An explanatory comment on every type assertion.** No linter can judge
 *    whether a comment explains anything. Review catches this one or nothing
 *    does.
 *  - **`switch` must carry a `default`.** Omitting it is what makes TypeScript
 *    treat a discriminated-union switch as exhaustive, so requiring one would
 *    turn a compile error on a newly added variant into a silent fallthrough.
 *    `@typescript-eslint/switch-exhaustiveness-check` is the version of this
 *    rule worth adopting, and it is not on yet.
 *  - **Function declarations over arrow-function consts for named functions.**
 *    The guide states it as a preference; `func-style` states it as a law, and
 *    the difference is thousands of call sites.
 *  - **The `@fileoverview` and copyright header order.** Google-internal
 *    convention that buys an OSS repo nothing.
 */
const googleStyleRules = {
  // Source file structure: named exports only, and types re-exported as types.
  // `no-default-export` is switched back off for tool config files, which the
  // tools themselves require to have one.
  'import-x/no-default-export': 'error',
  '@typescript-eslint/consistent-type-exports': 'error',

  // Local variable declarations: `const` by default, `let` when reassigned,
  // never `var`, and one binding per statement.
  'no-var': 'error',
  'prefer-const': 'error',
  'one-var': ['error', 'never'],

  // Array and object literals.
  'no-object-constructor': 'error',
  'guard-for-in': 'error',
  'prefer-spread': 'error',
  'prefer-rest-params': 'error',

  // Classes: no `public` (it is the default), `readonly` on anything never
  // reassigned, and TypeScript's visibility annotations rather than `#private`
  // — the guide prefers the annotation because it is erased rather than
  // mangled, so it stays legible in the emitted output and in a debugger.
  '@typescript-eslint/explicit-member-accessibility': [
    'error',
    { accessibility: 'no-public' },
  ],
  '@typescript-eslint/prefer-readonly': 'error',

  // Functions: arrow functions instead of function expressions, and `this`
  // only where it has a declared type.
  'prefer-arrow-callback': 'error',
  '@typescript-eslint/no-invalid-this': 'error',

  // Control flow. `curly: multi-line` is the guide's rule exactly — braces are
  // required except on an `if` that fits a single line.
  curly: ['error', 'multi-line'],
  eqeqeq: ['error', 'always', { null: 'ignore' }],
  'no-extend-native': 'error',
  'no-new-wrappers': 'error',
  'no-eval': 'error',
  '@typescript-eslint/no-implied-eval': 'error',

  // Type system.
  '@typescript-eslint/consistent-type-definitions': ['error', 'interface'],
  '@typescript-eslint/consistent-type-assertions': [
    'error',
    { assertionStyle: 'as', objectLiteralTypeAssertions: 'never' },
  ],
  // `T[]` for simple element types, `Array<T>` for complex ones. This overrides
  // `stylisticTypeChecked`, which asks for `T[]` unconditionally.
  '@typescript-eslint/array-type': ['error', { default: 'array-simple' }],
};

/**
 * Naming, which is the one part of the guide with no off-the-shelf preset.
 *
 * UpperCamelCase for types, lowerCamelCase for values, CONSTANT_CASE for
 * module-level immutables, and no leading or trailing underscore anywhere —
 * including on an unused parameter, which is why `no-unused-vars` below carries
 * no `argsIgnorePattern`.
 *
 * Members whose spelling is fixed by something outside this repository are
 * exempt: object and type properties and methods are wire formats, HTTP header
 * names, route keys, CSS custom properties and i18n keys, not identifiers the
 * guide is talking about.
 *
 * PascalCase is allowed for values as well as types, because two things in
 * this codebase are PascalCase values by an external convention and cannot be
 * renamed without breaking what reads them:
 *
 *  - a React component, a component passed as a prop and a context object,
 *    which JSX resolves as an intrinsic element unless the identifier is
 *    capitalised;
 *  - a zod schema (`ChatMessageSchema`), whose name mirrors the type it
 *    produces and which is re-exported across every package.
 */
const googleNamingRules = {
  '@typescript-eslint/naming-convention': [
    'error',
    {
      selector: 'default',
      format: ['camelCase'],
      leadingUnderscore: 'forbid',
      trailingUnderscore: 'forbid',
    },
    {
      selector: ['variable', 'function', 'parameter'],
      format: ['camelCase', 'PascalCase', 'UPPER_CASE'],
      leadingUnderscore: 'forbid',
      trailingUnderscore: 'forbid',
    },
    // CONSTANT_CASE is the guide's own casing for an immutable constant; a
    // `static readonly` on a class is one.
    {
      selector: 'classProperty',
      modifiers: ['static', 'readonly'],
      format: ['UPPER_CASE', 'camelCase'],
      leadingUnderscore: 'forbid',
      trailingUnderscore: 'forbid',
    },
    { selector: 'typeLike', format: ['PascalCase'] },
    {
      selector: 'interface',
      format: ['PascalCase'],
      custom: { regex: '^I[A-Z]', match: false },
    },
    { selector: 'typeParameter', format: ['PascalCase'] },
    { selector: 'enumMember', format: ['UPPER_CASE'] },
    { selector: 'import', format: ['camelCase', 'PascalCase'] },
    // See the note above: these names are data, not identifiers.
    {
      selector: [
        'objectLiteralProperty',
        'objectLiteralMethod',
        'typeProperty',
      ],
      format: null,
    },
  ],
};

export default tseslint.config(
  {
    ignores: [
      '**/dist/**',
      '**/node_modules/**',
      '**/coverage/**',
      '**/*.tsbuildinfo',
      // The Rust workspace. Its test fixtures include `.mjs` files — an
      // extension is a script a host spawns, so a fixture extension is a real
      // one — but they are Cargo's inputs, not this repository's TypeScript,
      // and they belong to no tsconfig by construction.
      'crates/**',
      'target/**',
      // Config for `i18next-parser`, which loads them itself. They are outside
      // every package's `tsconfig`, so the type-aware rules have no project to
      // resolve them against — and adding a tsconfig for three declarative
      // objects would cost more than it checks.
      'i18next-parser.*.js',
      'i18next-parser.base.js',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.strictTypeChecked,
  ...tseslint.configs.stylisticTypeChecked,
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: { 'import-x': importX },
    rules: {
      ...crossPackageImportRule,
      ...googleStyleRules,
      ...googleNamingRules,

      // An agent runtime spawns background consolidation, subagents, MCP
      // reconnects and heartbeats. Unhandled floating promises are the #1
      // source of silent failure — this is why we use type-aware linting.
      '@typescript-eslint/no-floating-promises': 'error',
      '@typescript-eslint/no-misused-promises': 'error',
      '@typescript-eslint/await-thenable': 'error',
      '@typescript-eslint/require-await': 'error',
      '@typescript-eslint/return-await': ['error', 'always'],

      // Off because it contradicts `isolatedDeclarations`, which is a build
      // requirement rather than a preference. An exported `const X = /re/`
      // cannot have its declaration emitted without an explicit `: RegExp`,
      // and this rule calls that same annotation redundant.
      '@typescript-eslint/no-inferrable-types': 'off',

      '@typescript-eslint/consistent-type-imports': [
        'error',
        { prefer: 'type-imports', fixStyle: 'inline-type-imports' },
      ],
      // No ignore pattern. The guide forbids a leading underscore outright, so
      // an unused parameter is removed, or moved after the ones that are used,
      // rather than renamed to `_x`.
      //
      // `ignoreRestSiblings` is the one exception, and it is not really one:
      // `const {password, ...rest} = user` has to name the key it drops, and
      // the guide's own advice is to use rest destructuring for exactly this.
      // Without it there is no way to omit a field at all.
      '@typescript-eslint/no-unused-vars': [
        'error',
        { ignoreRestSiblings: true },
      ],

      // The exec tool takes argv: string[] and runs execFile with shell:false.
      // There is no legitimate use of a shell anywhere in this codebase.
      'no-restricted-syntax': [
        'error',
        {
          selector: "Property[key.name='shell'][value.value=true]",
          message:
            'shell: true is banned. The exec tool takes argv: string[] and runs execFile with shell: false.',
        },
        {
          selector:
            "MemberExpression[object.name='Math'][property.name='random']",
          message:
            'Math.random() breaks deterministic tests. Inject a generator, or use node:crypto for security-relevant randomness.',
        },

        // The rest of the Google guide's outright bans, none of which has a
        // dedicated rule.
        {
          selector: "ExportNamedDeclaration > VariableDeclaration[kind='let']",
          message:
            'Mutable exports are banned. Export a const, or a function that returns the current value.',
        },
        {
          selector: 'PropertyDefinition[key.type="PrivateIdentifier"]',
          message:
            "`#private` is banned. Use TypeScript's `private`, which is erased rather than mangled and so stays legible in a debugger.",
        },
        {
          selector: 'TSEnumDeclaration[const=true]',
          message:
            '`const enum` is banned; it does not survive isolatedModules. Use a plain enum, or a union of literals.',
        },
        {
          selector: 'WithStatement',
          message: '`with` is banned.',
        },
      ],
    },
  },
  {
    // Tests may be looser: fixtures use non-null assertions and unsafe casts freely.
    files: [
      '**/*.test.{ts,tsx}',
      '**/test/**/*.{ts,tsx}',
      '**/testkit/**/*.ts',
    ],
    rules: {
      '@typescript-eslint/no-non-null-assertion': 'off',
      // A scripted async generator — a stream fixture — legitimately has
      // nothing to await; the rule only ever fires on those here.
      '@typescript-eslint/require-await': 'off',
      '@typescript-eslint/no-unsafe-assignment': 'off',
      '@typescript-eslint/no-unsafe-member-access': 'off',
      '@typescript-eslint/no-unsafe-argument': 'off',
      'no-restricted-syntax': 'off',
      // `{matches: true} as MediaQueryListEvent` is the point of the fixture:
      // it supplies the one field under test and no other. The annotation the
      // guide asks for instead would be a compile error, so only the object
      // literal case is relaxed — `as` is still the required syntax, and
      // product code still has to annotate.
      '@typescript-eslint/consistent-type-assertions': [
        'error',
        { assertionStyle: 'as', objectLiteralTypeAssertions: 'allow' },
      ],
    },
  },
  {
    // Config files, scripts, and an extension's own entry script live outside
    // every package tsconfig, so type-aware rules cannot resolve them. Lint
    // them syntactically only.
    //
    // The extension case is not an oversight to fix later: an extension is a
    // program the host *spawns*, shipped as the file that runs. Giving it a
    // tsconfig would mean giving it a build, and the whole point of the
    // reference extension is that an author needs neither.
    files: [
      '**/*.config.{js,mjs,ts}',
      'scripts/**/*.{js,mjs,ts}',
      'eslint.config.js',
      'examples/*/index.mjs',
    ],
    extends: [tseslint.configs.disableTypeChecked],
    languageOptions: { globals: { console: 'readonly', process: 'readonly' } },
    rules: {
      'no-restricted-syntax': 'off',
      '@typescript-eslint/no-unsafe-assignment': 'off',
      // vite, vitest, tsup and playwright each load their config by taking the
      // module's default export. The guide's rule is about our own modules; a
      // file whose shape is dictated by the tool reading it is not one.
      'import-x/no-default-export': 'off',
      // Type-aware, so unavailable to these files by definition.
      '@typescript-eslint/consistent-type-exports': 'off',
      '@typescript-eslint/prefer-readonly': 'off',
      '@typescript-eslint/no-implied-eval': 'off',
    },
  },
);
