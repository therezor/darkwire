/**
 * "Are you sure?", once, for every screen that needs to ask.
 *
 * This is the piece of the kit that fixes a real gap rather than a cosmetic
 * one. Files asked before deleting; Agents and Workspaces did not — both fired
 * an irreversible action on a single click of an icon button, and a workspace
 * row's delete sat one pixel from its rename. Making the question a component
 * is what makes "ask first" the cheap option rather than the diligent one.
 *
 * Two details are load-bearing and are the reason `onOpenChange` is on the
 * outside rather than being synthesised from `onConfirm`/`onCancel`:
 *
 *  - `Escape` and a click on the overlay both arrive as `onOpenChange(false)`,
 *    never as a press of Cancel. A caller that needs to intercept a close — the
 *    unsaved-edits guard in the file editor — has to be able to see it.
 *  - The dialog does not close itself on confirm. A delete that fails should
 *    leave the question on screen with its error, and a caller that closes on
 *    success is the one that knows whether it succeeded.
 */

import type { JSX, ReactNode } from 'react';

import { Button } from '@/components/ui/button.js';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogHeading,
  DialogSubheading,
} from '@/components/ui/dialog.js';

export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  cancelLabel = 'Cancel',
  tone = 'danger',
  pending = false,
  onConfirm,
  children,
}: {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
  readonly title: ReactNode;
  readonly description: ReactNode;
  readonly confirmLabel: string;
  readonly cancelLabel?: string;
  /** `danger` for anything irreversible; `primary` for a merely consequential yes. */
  readonly tone?: 'danger' | 'primary';
  readonly pending?: boolean;
  readonly onConfirm: () => void;
  /**
   * Anything that turns the question into a decision — the count of what a
   * folder holds, the number of conversations a workspace still owns.
   */
  readonly children?: ReactNode;
}): JSX.Element {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogHeading>{title}</DialogHeading>
          <DialogSubheading>{description}</DialogSubheading>
        </DialogHeader>

        {children}

        <DialogFooter>
          <Button
            variant="ghost"
            onClick={() => {
              onOpenChange(false);
            }}
          >
            {cancelLabel}
          </Button>
          <Button variant={tone} disabled={pending} onClick={onConfirm}>
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
