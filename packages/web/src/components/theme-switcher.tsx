/**
 * The colour-scheme control: one button in the header, three choices in a menu.
 *
 * Three choices, not two, because `system` is a real preference: a user who
 * never touched it follows their OS at sunset, and a two-state toggle silently
 * makes that choice for them the first time they click. `useTheme` owns the
 * rule and the persistence; this is only its control surface.
 *
 * A menu rather than three segments on the header bar. Both were built; the
 * segmented version put every state on screen at the cost of three permanent
 * targets in a bar that already carries a wordmark, the resolved model, a
 * connection badge and the context inspector — a lot of chrome to spend on a
 * setting most users touch once. The menu spends one.
 *
 * A radio group rather than a cycling button — a button that cycles through
 * three states does not say what the next press will do, and a screen reader
 * announces it as a button whose label just changed for no stated reason.
 *
 * The trigger wears the *preference's* icon, not the resolved theme's, so it
 * reports what the setting is rather than what it currently evaluates to. The
 * sun-moon glyph is what makes that legible: `system` under a plain moon is
 * indistinguishable from `dark`.
 *
 * Ordered light, dark, then system — see `OPTIONS` below.
 */

import { Moon, Sun, SunMoon } from 'lucide-react';
import type { JSX } from 'react';

import { useAppTheme } from '@/theme/theme-context.js';
import { isThemePreference, type ThemePreference } from '@/theme/theme.js';
import { Button } from './ui/button.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from './ui/dropdown-menu.js';

interface Option {
  readonly value: ThemePreference;
  readonly label: string;
  readonly Icon: typeof Sun;
}

/**
 * The two explicit choices first, then the one that defers.
 *
 * `system` is last because it is a different kind of answer from the two above
 * it: light and dark are the themes, and system is an instruction to stop
 * choosing. Grouping the like things together and putting the deferral at the
 * bottom is also where every OS settings panel puts it, which matters more than
 * the visual argument for sitting the sun-moon glyph between the sun and the
 * moon it is drawn from.
 */
const OPTIONS: readonly Option[] = [
  { value: 'light', label: 'Light', Icon: Sun },
  { value: 'dark', label: 'Dark', Icon: Moon },
  { value: 'system', label: 'System', Icon: SunMoon },
];

const ICONS = { light: Sun, system: SunMoon, dark: Moon } as const;

export function ThemeSwitcher({
  className,
}: {
  readonly className?: string;
}): JSX.Element {
  const { preference, resolved, setPreference } = useAppTheme();
  const Icon = ICONS[preference];

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className={className}
          // The resolution is in the label because it is the thing the icon
          // cannot show: `system` resolved to dark is still `system`.
          aria-label={`Colour scheme: ${preference} (currently ${resolved})`}
        >
          <Icon />
        </Button>
      </DropdownMenuTrigger>

      <DropdownMenuContent align="end" className="floating--menu">
        <DropdownMenuRadioGroup
          value={preference}
          onValueChange={(value) => {
            if (isThemePreference(value)) setPreference(value);
          }}
        >
          {OPTIONS.map(({ value, label, Icon: OptionIcon }) => (
            <DropdownMenuRadioItem key={value} value={value}>
              <OptionIcon />
              {label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
