/**
 * How a notification's level is drawn, in the one place that decides.
 *
 * Two surfaces render notifications — the header popover and the full page —
 * and they were going to be written twice. A level added to the protocol and
 * wired into one of them would leave the other rendering `undefined` as an
 * icon, which is exactly the kind of divergence a shared table prevents.
 */

import { AlertTriangle, CheckCheck, CircleAlert, Info } from 'lucide-react';

export const LEVEL_ICONS = {
  info: Info,
  success: CheckCheck,
  warning: AlertTriangle,
  error: CircleAlert,
} as const;

export const LEVEL_CLASSES = {
  info: 'notification__icon--info',
  success: 'notification__icon--success',
  warning: 'notification__icon--warning',
  error: 'notification__icon--error',
} as const;
