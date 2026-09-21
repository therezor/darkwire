**What this changes, and why**

**How to see it working**

## Checks

`pnpm check` is the first CI job, not all four: it stops short of coverage, e2e and the
Rust workspace. Tick what you ran, and say so if you skipped something on purpose;
"server-only, so no e2e" is a fine answer and a silent gap is not.

- [ ] `pnpm check`: typecheck, lint, token gates, format, shellcheck, i18n, protocol, test, build
- [ ] `pnpm test:coverage` — stricter than `pnpm test`; thirteen packages have raised bars
- [ ] `pnpm --filter @darkwire/e2e test:e2e` — needs `pnpm build` first
- [ ] `pnpm screenshots` — if the UI changed visibly

<!-- See CONTRIBUTING.md and docs/development.md. -->
