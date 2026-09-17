/**
 * How a workspace's folder is written on screen.
 *
 * Three places render it — the list's second line, the editor's box, and the
 * create dialog's "Creates …" hint — and they have to agree, because the whole
 * point of showing it is that a reader can match what the UI says against what
 * the app does.
 *
 * **Rooted at `/`, never prefixed with `workspaces/`.** Two reasons, and the
 * second is the one that makes it a correctness rule rather than a preference:
 *
 *  - **`workspace` already means something else in this UI.** The Files
 *    breadcrumb calls its root crumb `workspace`, and that root is *the
 *    workspace you are in* — browsing `acme24` shows `workspace / notes.md` for
 *    a file in acme24. A list that also wrote `workspaces/acme24` for the tree
 *    acme24 sits in would be one word naming two different directories on two
 *    screens of the same app.
 *  - **The prefix was invented here.** `WorkspaceSummary` carries an id and no
 *    path — deliberately, so that accepting a directory from a client stays
 *    impossible rather than merely unimplemented — and the real tree is the
 *    install's workspaces folder, which an install may override. On one that
 *    does, `workspaces/acme24` names a directory that is not called that.
 *    `/acme24` is true whatever the folder is, because it is stated *relative*
 *    to it.
 *
 * Every workspace gets a segment, the default included. They are siblings on
 * disk, so writing one of them differently would describe a nesting that is
 * not there.
 */

import type { WorkspaceSummary } from '@darkwire/protocol';

/** The workspaces tree's root, as this package spells it. */
export const WORKSPACE_ROOT_PATH = '/';

/** `/<folder>`, for every workspace including the default. */
export function folderLabel(workspace: Pick<WorkspaceSummary, 'id'>): string {
  return `${WORKSPACE_ROOT_PATH}${workspace.id}`;
}
