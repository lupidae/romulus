// The kit's `toast`, for the homepage's hero: the hero build resolves
// `@/components/ui/toaster` here (vite.hero.config.ts). Sonner, the app's
// toaster, injects its stylesheet as an inline <style>, which the homepage's
// CSP refuses; these toasts use the kit's own classes, in the island.

import * as React from "react";

import {
  CheckCircleIcon,
  InfoCircleIcon,
  WarningTriangleIcon,
  XmarkCircleIcon,
  XmarkIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";

type Kind = "default" | "success" | "error" | "warning" | "info";

interface Options {
  description?: React.ReactNode;
  duration?: number;
  action?: { label: string; onClick: () => void };
}

interface Item extends Options {
  id: number;
  kind: Kind;
  title: React.ReactNode;
}

let items: Item[] = [];
let next = 0;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

function dismiss(id: number) {
  items = items.filter((t) => t.id !== id);
  emit();
}

function show(kind: Kind, title: React.ReactNode, options: Options = {}) {
  const id = ++next;
  // At most three at once, as in the app.
  items = [...items, { id, kind, title, ...options }].slice(-3);
  emit();
  window.setTimeout(() => dismiss(id), options.duration ?? 4000);
  return id;
}

// The kit's API: `toast()` beside `<Toaster>` (no fast refresh in the hero build).
// eslint-disable-next-line react-refresh/only-export-components
export const toast = Object.assign(
  (title: React.ReactNode, options?: Options) => show("default", title, options),
  {
    success: (title: React.ReactNode, options?: Options) => show("success", title, options),
    error: (title: React.ReactNode, options?: Options) => show("error", title, options),
    warning: (title: React.ReactNode, options?: Options) => show("warning", title, options),
    info: (title: React.ReactNode, options?: Options) => show("info", title, options),
    dismiss,
  },
);

const ICONS: Record<Kind, React.ReactNode> = {
  default: null,
  success: <CheckCircleIcon className="text-success" />,
  error: <XmarkCircleIcon className="text-destructive" />,
  warning: <WarningTriangleIcon className="text-warning" />,
  info: <InfoCircleIcon className="text-info" />,
};

/** The toasts, bottom-right of the element it is rendered in; announced politely. */
export function Toaster() {
  const shown = React.useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => items,
  );
  return (
    <ol
      aria-live="polite"
      aria-label="Notifications"
      className="pointer-events-none absolute right-3 bottom-12 z-(--z-toast) flex w-80 max-w-[calc(100%-1.5rem)] flex-col gap-2"
    >
      {shown.map((t) => (
        <li
          key={t.id}
          className="pointer-events-auto flex animate-in items-start gap-2.5 rounded-lg border border-border bg-popover p-3 pr-2 text-popover-foreground shadow-popover fade-in-0 slide-in-from-bottom-2"
        >
          {ICONS[t.kind] && (
            <span className="mt-0.5 flex size-4 shrink-0 items-center [&_svg]:size-4">
              {ICONS[t.kind]}
            </span>
          )}
          <div className="grid min-w-0 flex-1 gap-0.5">
            <p className="text-sm font-medium text-foreground">{t.title}</p>
            {t.description && <p className="text-sm text-muted-foreground">{t.description}</p>}
          </div>
          {t.action && (
            <Button
              size="sm"
              variant="primary"
              className="self-center"
              onClick={() => {
                t.action?.onClick();
                dismiss(t.id);
              }}
            >
              {t.action.label}
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Dismiss"
            className="-my-1"
            onClick={() => dismiss(t.id)}
          >
            <XmarkIcon />
          </Button>
        </li>
      ))}
    </ol>
  );
}
