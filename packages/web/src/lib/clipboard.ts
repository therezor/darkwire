/**
 * Copying, in the one place that knows how it fails.
 *
 * Two surfaces copy — a fenced code block and a message's action bar — and they
 * look nothing alike, so what is shared here is the behaviour rather than the
 * markup. The failure handling is the part worth having once: it is subtle, and
 * a second copy of it written from memory would get it wrong.
 */

/**
 * Copies, and says whether it worked.
 *
 * `navigator.clipboard` is typed as always present and is absent on an insecure
 * origin and in jsdom, where reading `.writeText` off it throws synchronously —
 * so the whole call is inside the `try`, not just the promise. A refusal is a
 * button that does not claim success, not an error the user has to dismiss.
 */
export async function copyToClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
