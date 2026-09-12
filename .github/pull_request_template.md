**What this changes, and why**

**How to see it working**

## Checks

`pnpm check` is not the gate — it misses `format:check`, the token gates, `i18n:check`,
`build`, coverage and e2e. Tick what you ran, and say so if you skipped something on
purpose; "server-only, so no e2e" is a fine answer and a silent gap is not.

- [ ] `pnpm typecheck`
- [ ] `pnpm lint`
- [ ] `pnpm format:check`
- [ ] `pnpm test`
- [ ] `pnpm build`
- [ ] `pnpm test:coverage` — stricter than `pnpm test`; thirteen packages have raised bars
- [ ] `pnpm --filter @ghostwire/web exec tsx src/tokens/run-gates.ts` — if this touched `packages/web`
- [ ] `pnpm i18n:check` — if this added or changed a string a person reads
- [ ] `pnpm --filter @ghostwire/e2e test:e2e` — needs `pnpm build` first
- [ ] `pnpm screenshots` — if the UI changed visibly

<!-- See CONTRIBUTING.md and docs/development.md. -->
