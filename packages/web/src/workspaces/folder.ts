/**
 * How a workspace's folder is written on screen.
 *
 * Three places render it — the list's second line, the editor's box, and the
 * create dialog's "Creates …" hint — and they have to agree, because the whole
 * point of showing it is that a reader can match what the UI says against what
 * the app does.
 *
 * **Rooted at `/`, never prefixed with `workspace/`.** Two reasons, and the
 * second is the one that makes it a correctness rule rather than a preference:
 *
 *  - **`workspace` already means something else in this UI.** The Files
 *    breadcrumb calls its root crumb `workspace`, and that root is *the
 *    workspace you are in* — browsing `acme24` shows `workspace / notes.md` for
 *    a file in acme24. A list that also wrote `workspace/acme24` for the tree
 *    acme24 sits in would be one word naming two different directories on two
 *    screens of the same app.
 *  - **The prefix was invented here.** `WorkspaceSummary` carries an id and no
 *    path — deliberately, so that accepting a directory from a client stays
 *    impossible rather than merely unimplemented — and the real tree is
 *    `paths.workspace`, which an install may override. On one that does,
 *    `workspace/acme24` names a directory that is not called that. `/acme24`
 *    is true whatever the root is, because it is stated *relative* to it.
 *
 * So the default gets `/` — it is the root, which is exactly why it holds the
 * others and cannot be moved — and every other workspace gets a segment under
 * it. The nesting is then visible rather than described, which is what replaced
 * an earlier sentence ("the workspace root") and, before that,
 * `workspace/default`: a directory that does not exist.
 */

import type { WorkspaceSummary } from '@ghostwire/protocol';

/** The workspace tree's root, as this package spells it. */
export const WORKSPACE_ROOT_PATH = '/';

/** `/` for the default; `/<folder>` for every other one. */
export function folderLabel(
  workspace: Pick<WorkspaceSummary, 'id' | 'isDefault'>,
): string {
  return workspace.isDefault
    ? WORKSPACE_ROOT_PATH
    : `${WORKSPACE_ROOT_PATH}${workspace.id}`;
}
