/**
 * @ghostwire/protocol — Zod schemas and the types derived from them.
 *
 * Every other package imports its shared shapes from here. The schemas are the
 * single source of truth: TypeScript types come from `z.infer`, JSON Schema for
 * tool definitions comes from `z.toJSONSchema`, and the HTTP layer's OpenAPI
 * document is generated from these same objects — never hand-maintained.
 *
 * No logic and no I/O beyond three pure functions that must behave identically
 * everywhere they run: `isLoopbackHost` (the server and the CLI must agree on what
 * counts as a remote bind before refusing to start without auth),
 * `tokensPerSecond` (the terminal and the browser must report the same rate for
 * the same turn, and the browser cannot import the package that stores it), and
 * `renderPromptTemplate` (the agent composes an agent's system prompt and the
 * browser edits it, so the template text and the substitution rules have to be
 * one definition rather than two that agree until they do not).
 *
 * `newUuid` is the one export that is *not* pure — it reads a clock and a random
 * source. It is here for the same reason `ids.ts` is: the server and the browser
 * both mint ids, and the rule that two of them cannot collide has to be one
 * implementation. It touches no I/O and imports nothing.
 *
 * Runtime dependencies: `zod`, and nothing else — ever.
 */

export * from './messages.js';
export * from './tools.js';
export * from './ids.js';
export * from './uuid.js';
export * from './prompt.js';
export * from './config.js';
export * from './subagent.js';
export * from './toolbox.js';
export * from './preset.js';
export * from './extension.js';
export * from './automation.js';
export * from './ws.js';
export * from './rest.js';
export { PROTOCOL_SCHEMAS, SCHEMA_MODULES } from './schemas.js';
