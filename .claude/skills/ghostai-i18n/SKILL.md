---
name: ghostai-i18n
description: The translation layer — how user-facing copy becomes a key, how the JSON bundles type themselves, how errors carry a translatable key across packages, and the two CI gates that catch the opposite halves of the problem. Use when adding or changing any string a person reads: web components, CLI output, GhostError messages, panel labels, placeholders and aria-labels. Also use when `pnpm i18n:check` fails or a key does not type-check.
---

# GhostAI translations

Every sentence a person reads goes through `@ghostwire/i18n`. The layer is small,
and almost all of its design is aimed at one failure: a string that quietly never
became a key, and is therefore English forever in every locale.

## The shape of it

```
packages/i18n/
  locales/en/shared.json   # strings more than one surface says — errors, mostly
  locales/en/web.json      # the browser
  locales/en/cli.json      # the terminal
  src/resources.ts         # imports that JSON, so the JSON *is* the type
  src/keys.ts              # dotted-path key union derived from the resources
  src/web.ts               # browser instance: web + shared
  src/cli.ts               # terminal instance: cli + shared
```

Three namespaces, and the split is about payload rather than tidiness: the CLI
parses one bundle on every `--help` instead of three, and the browser never ships
the terminal's strings.

## The one invariant worth understanding

**The JSON is the type.** `resources.ts` imports the bundles, `ResourceKeys<T>`
walks them into a union of dotted paths, and `t()` is typed against that union. A
misspelled key is a compile error, not a key rendered on a nav item.

Two consequences that look like trivia and are not:

- **`resources.ts` imports by package self-reference** (`@ghostwire/i18n/locales/en/shared.json`),
  never by relative path. `tsc` copies the specifier into `dist/` verbatim and
  does not emit JSON, so a relative path resolves to nothing from inside `dist`.
  The failure is silent in the worst way: `skipLibCheck` swallows it,
  `SharedResources` degrades to `any`, `SharedMessageKey` widens to
  `shared:${string}`, and every consumer compiles with nothing checked.
- **Plural suffixes are stripped in `keys.ts`.** i18next resolves `files_one` /
  `files_other` from a base name of `files` that appears nowhere in the JSON, so
  without the strip the one call site that pluralises would be the one that fails
  to type-check.

## Adding a string

### In a React component

```tsx
import { useTranslation } from 'react-i18next';

const { t } = useTranslation(); // defaultNS is 'web'
<h2>{t('settings.panels.tools.label')}</h2>;
```

Add the key to `packages/i18n/locales/en/web.json`, or let
`pnpm i18n:extract` write it and then fill in the English.

**Interpolation** uses i18next's `{{name}}` slots:

```tsx
t('agents.sandbox.refused', { profile: id });
```

**Keys held as data must be typed `WebKey`**, not `string`. A table like

```ts
const NAV: readonly NavItem[] = [{ to: '/agents', label: 'nav.agents' }];
```

widens the literal on the way into the array, and by `t(label)` the compiler
knows only that it is a string — so a deleted key compiles and renders as itself.
`label: WebKey` keeps the literal narrow and puts the error at the wrong entry.

### In the CLI

`packages/cli/src/i18n.ts` exposes `t` (the `cli` namespace) and `ts` (the
`shared` one), both already scoped. Add the key to `cli.json`.

### In an error thrown outside web and cli

`GhostError` takes an optional `messageKey` and `messageParams`:

```ts
throw new GhostError('config', `Agent "${id}" asks for network "${mode}"…`, {
  messageKey: 'shared:agents.networkWithoutProfile',
  messageParams: { agentId: id, mode },
  details: { agentId: id, mode },
});
```

Both halves are carried on purpose, and the rule is in `cli/src/i18n.ts`:
**`message` stays authoritative for logs and pipes; `messageKey` is the same
sentence for a terminal or a browser that has a locale.** Everything that prints
an error to a person goes through `describeError`; everything that writes one to
a log does not.

The key is fully qualified (`shared:…`) because a package with no namespace of
its own cannot name a string unambiguously any other way. `SharedMessageKey` is
what stops a throw in `@ghostwire/runtime` from naming a string that only exists in
the UI bundle.

The import of `@ghostwire/i18n` in `packages/core/src/errors.ts` is **type-only and
must stay that way** — a value import would put the whole translation layer in
front of every package that touches an error.

## The two gates

They catch opposite failures and neither substitutes for the other.

```bash
pnpm i18n:check    # a key used in the source but missing from the bundle
pnpm test          # includes untranslated.test.ts: prose that never became a key
```

`i18n:check` runs the extractor and then `git diff --exit-code` on
`packages/i18n/locales`. A dirty tree means the bundles are out of step — run
`pnpm i18n:extract`, fill in any new English, and commit the result.

`packages/web/test/i18n/untranslated.test.ts` sweeps `.tsx` sources for English
prose in JSX text and in copy-carrying attributes (`aria-label`, `placeholder`,
`title`, `alt`, `description`, `label`). It is deliberately conservative — two or
more words — so `{'·'}` and `qwen3:8b` do not trip it. It has an allowlist, and
the allowlist is itself tested: an entry naming a file that no longer holds
untranslated copy fails, so an exemption cannot outlive its reason.

**Note:** `i18n:check` is a CI step (`.github/workflows/ci.yml`) that
`CLAUDE.md`'s gate list does not yet mention. Run it before claiming a task is
done.

## Failure modes, and what they actually mean

| Symptom                                   | Cause                                                                                                   |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `pnpm i18n:check` fails with a dirty diff | A `t()` call names a key that is not in the bundle. Run `i18n:extract`, fill in the English.            |
| `untranslated.test.ts` lists your file    | Prose sitting in JSX or in a copy attribute. Wrap it in `t()`.                                          |
| A key type-checks that should not         | `SharedResources` has degraded to `any` — check `resources.ts` still imports by package self-reference. |
| A pluralised key does not type-check      | You named `files_other`. Name the base key, `files`.                                                    |
| A key renders as itself in the UI         | It reached `t()` as a widened `string`. Type the field `WebKey`.                                        |

## What is not done yet

The engine is in place ahead of its adoption. `shared.json` carries `runtime.*`
and `server.*` keys, and **nothing in `@ghostwire/runtime`, `@ghostwire/server`,
`@ghostwire/security` or `@ghostwire/tools` currently throws with a `messageKey`** —
those errors are English-only today. That is a gap rather than a decision: when
touching an error in those packages, add the key and the bundle entry. The gates
will not tell you to, because neither of them reads `.ts` outside web and cli.
