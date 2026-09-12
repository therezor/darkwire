/**
 * The actions on one row of a list.
 *
 * A kebab rather than a cluster of icon buttons, and that is a decision about
 * how many actions a row is allowed to grow. An agent has four — open, rename,
 * duplicate, delete — and four ghost icon buttons in a table cell is a row that
 * reads as a toolbar with a name attached to it. It also puts Delete
 * permanently one pixel from Rename, which is how the workspace list managed to
 * offer an unconfirmed destructive action as the neighbour of a harmless one.
 *
 * Items are `children`, not an array of descriptors. The conditional row — no
 * Delete on the default agent, none on the default workspace — is the normal
 * case here, and `{!isDefault && <DropdownMenuItem…>}` says that in the
 * language the rest of the file is already written in. What this component owns
 * is the part that actually drifted between the four hand-written copies: the
 * accessible name, the alignment, and the menu surface class.
 */

import { MoreHorizontal } from 'lucide-react';
import type { JSX, ReactNode } from 'react';

import { Button } from '@/components/ui/button.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';

export function RowActions({
  label,
  children,
}: {
  /** What the row is, so the trigger announces as more than "button". */
  readonly label: string;
  readonly children: ReactNode;
}): JSX.Element {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={`Actions for ${label}`}>
          <MoreHorizontal />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="floating--menu">
        {children}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
