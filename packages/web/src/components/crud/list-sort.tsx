/**
 * How a list is ordered, once the column headings are gone.
 *
 * `SortHeader` put the control in the `<th>`, which is the right place when
 * there is a `<th>`: the thing you press is the thing you are ordering by, and
 * `aria-sort` announces the state without any prose. `DataList` has no headings
 * to hang that on, so the control moves to the toolbar beside the filter — the
 * other thing on the screen that changes which rows you see.
 *
 * **A ghost trigger and a menu, not a select and a button.** The first version
 * of this was exactly that pair, and it was wrong twice over. A bordered select
 * is the treatment this system gives a *form field*, so putting one in a
 * toolbar made the quietest control on the page look like the loudest — heavier
 * than the search box it sits next to, for something read far less often. And
 * the direction lived in a bare icon button beside it, which is two controls
 * and two visual weights for one decision. What is here instead is the shape
 * `.composer__picker` already uses for "a quiet control naming its current
 * value": muted, small, no border until you point at it.
 *
 * Inside, two radio groups rather than one clever list. Picking the column and
 * reversing it are different questions, and a combined menu — Name ↑, Name ↓,
 * Size ↑, Size ↓ — doubles in length with every column while making the common
 * act, reversing what is already chosen, a hunt.
 *
 * Choosing a column goes through `sortBy`, so a new one opens in the order that
 * column is usually read: names from A, sizes and times largest and newest
 * first. That is the one piece of `SortHeader`'s behaviour worth carrying over.
 */

import { ArrowDown, ArrowUp, ArrowUpDown } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '@/components/ui/button.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';
import { sortBy, type SortOrder } from './sort.js';

export function ListSort<K extends string>({
  options,
  sort,
  ascendingFirst,
  onChange,
}: {
  readonly options: ReadonlyArray<{ readonly key: K; readonly label: string }>;
  readonly sort: SortOrder<K>;
  /** The columns that open ascending — the text ones. See `sortBy`. */
  readonly ascendingFirst: readonly K[];
  readonly onChange: (next: SortOrder<K>) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const active = options.find((option) => option.key === sort.key);
  const direction = sort.descending
    ? t('common.descending')
    : t('common.ascending');

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        {/* One interpolated label rather than the visible text plus a pair of
            `sr-only` spans. The spans were the first attempt and they read as
            "Sort byNameAscending": a text alternative is the concatenation of
            its parts, and adjacent elements with no whitespace between them in
            the markup get none in the name either.

            The arrow carries the direction for everyone not hearing the label,
            which is why it is the only part duplicated. */}
        <Button
          variant="ghost"
          className="list-sort"
          aria-label={t('common.sortedBy', {
            column: active?.label ?? sort.key,
            direction,
          })}
        >
          <ArrowUpDown aria-hidden="true" />
          <span className="truncate">{active?.label ?? sort.key}</span>
          {sort.descending ? (
            <ArrowDown aria-hidden="true" />
          ) : (
            <ArrowUp aria-hidden="true" />
          )}
        </Button>
      </DropdownMenuTrigger>

      <DropdownMenuContent align="end" className="floating--menu">
        <DropdownMenuLabel>{t('common.sortBy')}</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={sort.key}
          onValueChange={(next) => {
            // The cast is safe by construction: the only values these items can
            // carry are the option keys.
            onChange(sortBy(next as K, ascendingFirst));
          }}
        >
          {options.map((option) => (
            <DropdownMenuRadioItem key={option.key} value={option.key}>
              {option.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>

        <DropdownMenuSeparator />

        <DropdownMenuLabel>{t('common.order')}</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={sort.descending ? 'descending' : 'ascending'}
          onValueChange={(next) => {
            onChange({ key: sort.key, descending: next === 'descending' });
          }}
        >
          <DropdownMenuRadioItem value="ascending">
            {t('common.ascending')}
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="descending">
            {t('common.descending')}
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
