/**
 * An `exec` rule's argv pattern, as one line of text a person can edit.
 *
 * The wire carries the pattern as an array, so nothing needs quoting there.
 * Here it is one field, split on whitespace, with `'` and `"` quoting and a
 * backslash escape for an argument that holds a space. It is not a shell: no
 * variable, glob or operator means anything, and a final `*` is the only
 * wildcard, as it is on the server.
 *
 * `patternMatches` mirrors the server's matcher so the prompt can refuse to
 * save a rule that would not cover its own call. The server checks again, and
 * its answer is the one that counts.
 */

export const REST = '*';

export type PatternError = 'empty' | 'unclosed' | 'star';

export type ParsedPattern =
  | { readonly ok: true; readonly argv: readonly string[] }
  | { readonly ok: false; readonly error: PatternError };

export function parsePattern(text: string): ParsedPattern {
  const argv: string[] = [];
  let token = '';
  let inToken = false;
  let quote: '"' | "'" | undefined;

  for (let index = 0; index < text.length; index += 1) {
    const char = text.charAt(index);
    if (quote !== undefined) {
      if (char === quote) {
        quote = undefined;
      } else if (char === '\\' && quote === '"' && index + 1 < text.length) {
        index += 1;
        token += text.charAt(index);
      } else {
        token += char;
      }
    } else if (char === '"' || char === "'") {
      quote = char;
      inToken = true;
    } else if (char === '\\' && index + 1 < text.length) {
      index += 1;
      token += text.charAt(index);
      inToken = true;
    } else if (/\s/.test(char)) {
      if (inToken) argv.push(token);
      token = '';
      inToken = false;
    } else {
      token += char;
      inToken = true;
    }
  }
  if (quote !== undefined) return { ok: false, error: 'unclosed' };
  if (inToken) argv.push(token);
  if (argv.length === 0) return { ok: false, error: 'empty' };
  if (argv.slice(0, -1).includes(REST)) return { ok: false, error: 'star' };
  return { ok: true, argv };
}

/** The inverse of `parsePattern`, quoting only what needs it. */
export function formatPattern(argv: readonly string[]): string {
  return argv
    .map((token) => {
      if (token === '') return "''";
      if (!/[\s'"\\]/.test(token)) return token;
      return `"${token.replace(/(["\\])/g, '\\$1')}"`;
    })
    .join(' ');
}

const EXECUTABLE_EXTENSIONS = ['.exe', '.com', '.bat', '.cmd', '.ps1'];

/** The server's `binary_name`: the basename, without a Windows extension. */
export function binaryName(program: string): string {
  const name = program.replace(/\\/g, '/').split('/').pop() ?? '';
  const lower = name.toLowerCase();
  const extension = EXECUTABLE_EXTENSIONS.find(
    (candidate) => lower.endsWith(candidate) && lower.length > candidate.length,
  );
  return extension === undefined ? name : name.slice(0, -extension.length);
}

export function patternMatches(
  pattern: readonly string[],
  argv: readonly string[],
): boolean {
  const rest = pattern.at(-1) === REST;
  const literals = rest ? pattern.slice(0, -1) : pattern;
  if (rest ? argv.length < literals.length : argv.length !== literals.length) {
    return false;
  }
  return literals.every((literal, index) =>
    index === 0
      ? binaryName(literal) === binaryName(argv[0] ?? '')
      : literal === argv[index],
  );
}

/**
 * Rules worth offering for one call, narrowest first: the exact command, then
 * shorter prefixes with a `*`, down to the program alone. The program is
 * reduced to its basename, because a rule may not name a path.
 */
export function suggestPatterns(
  argv: readonly string[],
): ReadonlyArray<readonly string[]> {
  const [program, ...args] = argv;
  if (program === undefined) return [];
  const head = [binaryName(program), ...args];
  const suggestions: string[][] = [head];
  for (let keep = Math.min(head.length, 3); keep >= 1; keep -= 1) {
    suggestions.push([...head.slice(0, keep), REST]);
  }
  return suggestions;
}
