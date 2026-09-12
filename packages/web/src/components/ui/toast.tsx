/**
 * Toasts — one helper, one viewport.
 *
 * `toast()` is a plain function backed by a Zustand store rather than a hook,
 * and that is the whole design: the things that need to report a failure are
 * not components. The WebSocket transport raises "reconnecting" from a
 * socket handler, `api.ts` raises "your session expired" from a fetch, and a
 * mutation raises "saved" from a callback. A `useToast()` hook would be
 * unreachable from all three.
 *
 * Radix supplies the part that is genuinely hard: an `aria-live` region that
 * announces without stealing focus, swipe-to-dismiss, and `F6` to jump to the
 * viewport — a toast carrying an action a keyboard user cannot reach is a toast
 * that lied.
 */

import * as ToastPrimitive from '@radix-ui/react-toast';
import { AlertTriangle, CheckCircle2, Info, X } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { create } from 'zustand';

import { cn } from '@/lib/cn.js';

type ToastRole = 'info' | 'success' | 'warning' | 'danger';

interface ToastInput {
  readonly title: string;
  readonly description?: string;
  readonly role?: ToastRole;
  /** Omitted means the role's default: errors stay until dismissed. */
  readonly durationMs?: number;
  readonly action?: { readonly label: string; readonly onSelect: () => void };
}

interface ToastRecord extends ToastInput {
  readonly id: number;
}

interface ToastStore {
  readonly toasts: readonly ToastRecord[];
  readonly push: (input: ToastInput) => number;
  readonly dismiss: (id: number) => void;
}

/**
 * Ids come from a counter, not `Date.now()` or a random: two toasts raised in
 * the same millisecond would collide, and React would reuse one's DOM node for
 * the other.
 */
let nextId = 0;

export const useToastStore = create<ToastStore>((set) => ({
  toasts: [],
  push: (input) => {
    const id = (nextId += 1);
    set((state) => ({ toasts: [...state.toasts, { ...input, id }] }));
    return id;
  },
  dismiss: (id) => {
    set((state) => ({
      toasts: state.toasts.filter((toast) => toast.id !== id),
    }));
  },
}));

/** Raise a toast from anywhere — a component, a socket handler, a catch block. */
export function toast(input: ToastInput): number {
  return useToastStore.getState().push(input);
}

toast.success = (title: string, description?: string): number =>
  toast({
    title,
    role: 'success',
    ...(description === undefined ? {} : { description }),
  });

toast.error = (title: string, description?: string): number =>
  toast({
    title,
    role: 'danger',
    ...(description === undefined ? {} : { description }),
  });

/**
 * The third of the three, and the one that was missing until something needed
 * it: a request that *succeeded* and produced a state the operator has to look
 * at. Approving an extension whose `activate` throws is exactly that — the
 * approval worked, and the extension did not — and reporting it as `success`
 * would put a green toast over a red row.
 */
toast.warning = (title: string, description?: string): number =>
  toast({
    title,
    role: 'warning',
    ...(description === undefined ? {} : { description }),
  });

const ICONS: Record<ToastRole, typeof Info> = {
  info: Info,
  success: CheckCircle2,
  warning: AlertTriangle,
  danger: AlertTriangle,
};

/** Info is the base rule, so it needs no modifier of its own. */
const ROLE_CLASSES: Record<ToastRole, string> = {
  info: '',
  success: 'toast--success',
  warning: 'toast--warning',
  danger: 'toast--danger',
};

/**
 * A failure that vanishes in four seconds is a failure the user has to
 * reproduce to read. Errors and warnings wait to be dismissed.
 */
function durationOf({ role = 'info', durationMs }: ToastInput): number {
  if (durationMs !== undefined) return durationMs;
  return role === 'danger' || role === 'warning' ? Infinity : 4500;
}

/** Mounted once, in `providers.tsx`. */
export function Toaster(): JSX.Element {
  const { t } = useTranslation();
  const toasts = useToastStore((state) => state.toasts);
  const dismiss = useToastStore((state) => state.dismiss);

  return (
    <ToastPrimitive.Provider swipeDirection="right">
      {toasts.map((record) => {
        const role = record.role ?? 'info';
        const Icon = ICONS[role];

        return (
          <ToastPrimitive.Root
            key={record.id}
            duration={durationOf(record)}
            onOpenChange={(open) => {
              if (!open) dismiss(record.id);
            }}
            className={cn('toast', ROLE_CLASSES[role])}
          >
            <Icon className="toast__icon" />

            <div className="stack toast__body">
              <ToastPrimitive.Title className="toast__title">
                {record.title}
              </ToastPrimitive.Title>
              {record.description !== undefined && (
                <ToastPrimitive.Description className="toast__description">
                  {record.description}
                </ToastPrimitive.Description>
              )}
            </div>

            {record.action !== undefined && (
              <ToastPrimitive.Action
                altText={record.action.label}
                onClick={record.action.onSelect}
                className="toast__action"
              >
                {record.action.label}
              </ToastPrimitive.Action>
            )}

            <ToastPrimitive.Close
              aria-label={t('common.dismiss')}
              className="toast__close"
            >
              <X />
            </ToastPrimitive.Close>
          </ToastPrimitive.Root>
        );
      })}

      <ToastPrimitive.Viewport className="toast-viewport" />
    </ToastPrimitive.Provider>
  );
}

/** Test seam: the store is module state, and a suite needs it empty per case. */
export function resetToasts(): void {
  useToastStore.setState({ toasts: [] });
}
