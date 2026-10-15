import { Toaster as Sonner, toast } from "sonner";

import { CheckCircleIcon, InfoCircleIcon, WarningTriangleIcon, XmarkCircleIcon } from "@/components/icons";
import { Spinner } from "@/components/ui/spinner";
import { useTheme } from "@/lib/theme";

/**
 * Transient confirmations, bottom-right, at most three at once. Call
 * `toast.success("Database created")`, `toast.error(…)`, or pass
 * `{ action: { label: "Undo", onClick } }` with a longer `duration`.
 * Never the only place something important is said: toasts disappear.
 */
export { toast };

export function Toaster() {
  const { preference } = useTheme();
  return (
    <Sonner
      theme={preference}
      position="bottom-right"
      visibleToasts={3}
      gap={8}
      offset={16}
      mobileOffset={12}
      icons={{
        success: <CheckCircleIcon className="text-success" />,
        error: <XmarkCircleIcon className="text-destructive" />,
        warning: <WarningTriangleIcon className="text-warning" />,
        info: <InfoCircleIcon className="text-info" />,
        loading: <Spinner className="text-muted-foreground" />,
      }}
      toastOptions={{
        unstyled: true,
        classNames: {
          toast:
            "z-(--z-toast) flex w-full items-start gap-2.5 rounded-lg border border-border bg-popover p-3 pr-4 text-popover-foreground shadow-popover sm:w-(--width)",
          icon: "mt-0.5 flex size-4 shrink-0 items-center [&_svg]:size-4",
          content: "grid min-w-0 flex-1 gap-0.5",
          title: "text-sm font-medium text-foreground",
          description: "text-sm text-muted-foreground",
          actionButton:
            "ml-2 h-7 shrink-0 cursor-pointer self-center rounded-md bg-primary px-2.5 text-sm font-medium text-primary-foreground hover:bg-primary-hover",
          cancelButton:
            "ml-2 h-7 shrink-0 cursor-pointer self-center rounded-md px-2.5 text-sm text-muted-foreground hover:bg-accent",
          closeButton:
            "border-border! bg-popover! text-muted-foreground! hover:text-foreground!",
        },
      }}
    />
  );
}
