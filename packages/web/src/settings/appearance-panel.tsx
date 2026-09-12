/**
 * Language, timezone and theme — three preferences, and not all the same kind.
 *
 * **The language and the timezone are the install's**, stored in `config.ui` and
 * read by `ghostai chat` and the scheduler on the same server. **The theme is this
 * browser's**, stored in `localStorage` and never written to the config. Putting
 * them on one screen is right — they are all "what this looks and reads like" —
 * but the difference is why only two thirds of this panel has a `SaveBar`: the
 * theme applies on the click, because there is nothing to persist anywhere else,
 * and a save button over it would imply a round trip that never happens.
 *
 * The language select writes through immediately as a *preference* too, so the
 * UI switches while the patch is in flight. `useAppLocale().adopt` reconciles it
 * when the settings query comes back, so a save the server refuses does not
 * leave the page in a language the config does not have.
 *
 * **The timezone is more than display, and the copy says so.** It is the zone
 * every timestamp is rendered in *and* the zone a cron expression is read in, so
 * changing it moves when existing jobs fire. That is the deliberate shape of one
 * install-wide zone rather than three — but it is a consequence an operator must
 * be told about at the control, not discover from a job that ran at the wrong
 * hour, which is what `timezoneDesc` is for.
 */

import { useMemo, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { SUPPORTED_LOCALES } from '@ghostwire/i18n';

import { useAppLocale } from '@/i18n/i18n-context.js';
import { SYSTEM } from '@/i18n/locale-preference.js';
import { SYSTEM_TZ, timezoneOptions } from '@/lib/timezones.js';
import { isThemePreference } from '@/theme/theme.js';
import { useAppTheme } from '@/theme/theme-context.js';
import {
  browserTimezone,
  useAppTimezone,
} from '@/timezone/timezone-context.js';

import {
  FieldGrid,
  SaveBar,
  Section,
  SelectField,
  type SelectFieldOption,
} from '@/components/form/controls.js';
import { useSaveSettings } from './use-settings.js';

/**
 * The languages this build ships, named in their own language.
 *
 * `Intl.DisplayNames` with the tag as its own locale, so German reads `Deutsch`
 * in an English UI rather than `German` — a language picker is one of the few
 * places where *not* translating the label is the accessible choice, because the
 * person who needs it cannot read the surrounding page.
 */
function localeOptions(systemLabel: string): readonly SelectFieldOption[] {
  return [
    { value: SYSTEM, label: systemLabel },
    ...SUPPORTED_LOCALES.map((locale) => ({
      value: locale,
      label: nameOf(locale),
    })),
  ];
}

function nameOf(locale: string): string {
  try {
    return (
      new Intl.DisplayNames([locale], { type: 'language' }).of(locale) ?? locale
    );
  } catch {
    // A runtime without the data for this tag. The tag itself is a worse label
    // than its own name and a better one than nothing.
    return locale;
  }
}

export function AppearancePanel(): JSX.Element {
  const { t } = useTranslation();
  const locale = useAppLocale();
  const theme = useAppTheme();
  const timezone = useAppTimezone();
  const { save, saving } = useSaveSettings();

  // The preference as the select shows it, which is not the same as what is
  // saved: `system` is a real choice here and resolves to a concrete tag before
  // it reaches the config, because `ui.locale` names a language and not a rule.
  const [pending, setPending] = useState<string>(locale.preference);
  const dirty = pending !== locale.preference;

  const onSave = (): void => {
    save({ ui: { locale: pending === SYSTEM ? locale.resolved : pending } });
  };

  // Built once: the IANA list is several hundred entries and does not change
  // while the panel is open.
  const tzOptions = useMemo(
    () => [
      // Labelled with what it will become, because that is what gets saved. An
      // option reading only "System" would hide the fact that the click writes
      // a concrete zone — and that the answer therefore stops following this
      // browser the moment it is saved.
      {
        value: SYSTEM_TZ,
        label: `${t('settings.appearance.system')} (${browserTimezone()})`,
      },
      ...timezoneOptions().map((zone) => ({ value: zone, label: zone })),
    ],
    [t],
  );

  /**
   * `undefined` until the operator touches it, rather than seeded from the
   * current zone.
   *
   * Seeding it would capture whatever `useAppTimezone()` answered on the first
   * render — and that is the *browser's* zone, because the settings query has
   * not come back yet. The select would then show a zone the install does not
   * have and read as unsaved before anyone touched anything.
   *
   * It also means no reset after a save: once the config carries the new zone,
   * `pendingTz` and `timezone` agree and `tzDirty` goes false on its own,
   * without a round trip where the select flicks back to the old value.
   */
  const [pendingTz, setPendingTz] = useState<string | undefined>(undefined);
  const shownTz = pendingTz ?? timezone;
  const tzDirty = pendingTz !== undefined && pendingTz !== timezone;

  return (
    <div className="stack">
      <Section
        title={t('settings.appearance.languageTitle')}
        description={t('settings.appearance.languageDesc')}
      >
        <FieldGrid>
          <SelectField
            label={t('settings.appearance.language')}
            hint={t('settings.appearance.languageHint')}
            value={pending}
            options={localeOptions(t('settings.appearance.system'))}
            onValueChange={(value) => {
              setPending(value);
              // Applied on the click rather than on the save, so the screen the
              // choice is made on is the first thing to answer in the new
              // language. The config still decides — `adopt` reconciles.
              locale.setPreference(value);
            }}
          />
        </FieldGrid>

        <SaveBar
          dirty={dirty}
          saving={saving}
          onSave={onSave}
          onRevert={() => {
            setPending(locale.preference);
            locale.setPreference(locale.preference);
          }}
        />
      </Section>

      <Section
        title={t('settings.appearance.timezoneTitle')}
        description={t('settings.appearance.timezoneDesc')}
      >
        <FieldGrid>
          <SelectField
            label={t('settings.appearance.timezone')}
            hint={t('settings.appearance.timezoneHint')}
            value={shownTz}
            options={tzOptions}
            onValueChange={setPendingTz}
          />
        </FieldGrid>

        {/* A `SaveBar` rather than applying on the click, unlike the theme:
            this one reschedules cron jobs on the server, and a change with
            that reach should be something the operator confirms. */}
        <SaveBar
          dirty={tzDirty}
          saving={saving}
          onSave={() => {
            // Resolved here, never stored as the sentinel — see `SYSTEM_TZ`.
            const chosen = shownTz === SYSTEM_TZ ? browserTimezone() : shownTz;
            setPendingTz(chosen);
            save({ ui: { timezone: chosen } });
          }}
          onRevert={() => {
            setPendingTz(undefined);
          }}
        />
      </Section>

      <Section
        title={t('settings.appearance.themeTitle')}
        description={t('settings.appearance.themeDesc')}
      >
        <FieldGrid>
          <SelectField
            label={t('settings.appearance.theme')}
            value={theme.preference}
            options={[
              { value: 'system', label: t('settings.appearance.system') },
              { value: 'light', label: t('settings.appearance.light') },
              { value: 'dark', label: t('settings.appearance.dark') },
            ]}
            onValueChange={(value) => {
              // No save bar: this is `localStorage` and the DOM, both of which
              // are already written by the time the menu closes.
              if (isThemePreference(value)) theme.setPreference(value);
            }}
          />
        </FieldGrid>
      </Section>
    </div>
  );
}
