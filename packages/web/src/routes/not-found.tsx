/**
 * A route that does not exist.
 *
 * Reachable in one realistic way: a bookmark to a panel that moved. So it
 * offers the way back rather than an apology.
 */

import { Link } from '@tanstack/react-router';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { buttonVariants } from '@/components/ui/button.js';

export function NotFoundRoute(): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="stack page page--reading">
      <h1 className="page__title">{t('common.notFound')}</h1>
      <p className="page__note">{t('common.notFoundBody')}</p>
      <Link to="/" className={buttonVariants({ variant: 'secondary' })}>
        {t('common.backToSession')}
      </Link>
    </div>
  );
}
