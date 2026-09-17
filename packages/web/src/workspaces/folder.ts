/**
 * How a workspace's folder is written on screen.
 *
 * Three places render it — the list's second line, the editor's box, and the
 * create dialog's "Creates …" hint — and they have to agree, because the whole
 * point of showing it is that a reader can match what the UI says against what
 * the app does.
 *
 * **Rooted at `/`, never prefixed with `workspaces/`.** The prefix was invented
 * here. `WorkspaceSummary` carries an id and no path, deliberately, so that
 * accepting a directory from a client stays impossible rather than merely
 * unimplemented. The real tree is the install's workspaces folder, which an
 * install may override, and on one that does `workspaces/acme24` names a
 * directory that is not called that. `/acme24` is true whatever the folder is,
 * because it is stated *relative* to it.
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
