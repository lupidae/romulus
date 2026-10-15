import { cva, type VariantProps } from "class-variance-authority";
import type * as React from "react";

import {
  CheckCircleIcon,
  InfoCircleIcon,
  WarningTriangleIcon,
  XmarkCircleIcon,
} from "@/components/icons";
import { cn } from "@/lib/utils";

const alertVariants = cva(
  "flex items-start gap-2.5 rounded-lg border px-3 py-2.5 text-base [&>svg]:mt-0.5 [&>svg]:size-4 [&>svg]:shrink-0",
  {
    variants: {
      variant: {
        info: "border-info/25 bg-info/6 [&>svg]:text-info",
        success: "border-success/25 bg-success/6 [&>svg]:text-success",
        warning: "border-warning/30 bg-warning/8 [&>svg]:text-warning",
        destructive: "border-destructive/25 bg-destructive/6 [&>svg]:text-destructive",
      },
    },
    defaultVariants: { variant: "info" },
  },
);

const icons = {
  info: InfoCircleIcon,
  success: CheckCircleIcon,
  warning: WarningTriangleIcon,
  destructive: XmarkCircleIcon,
};

interface AlertProps extends VariantProps<typeof alertVariants> {
  title: React.ReactNode;
  children?: React.ReactNode;
  /** An action on the right, e.g. a small “Retry” Button. */
  action?: React.ReactNode;
  className?: string;
}

/** A message that stays until its cause is gone: errors you can act on, warnings. */
export function Alert({ variant = "info", title, children, action, className }: AlertProps) {
  const Icon = icons[variant ?? "info"];
  return (
    <div
      role={variant === "destructive" || variant === "warning" ? "alert" : "status"}
      className={cn(alertVariants({ variant }), className)}
    >
      <Icon />
      <div className="grid min-w-0 flex-1 gap-0.5">
        <p className="font-medium text-foreground">{title}</p>
        {children && <div className="text-muted-foreground">{children}</div>}
      </div>
      {action && <div className="-my-0.5 shrink-0 self-center">{action}</div>}
    </div>
  );
}
