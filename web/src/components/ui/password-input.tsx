import * as React from "react";

import { EyeClosedIcon, EyeIcon } from "@/components/icons";
import { cn } from "@/lib/utils";

import { Button } from "./button";
import { controlClasses } from "./input";

type PasswordInputProps = Omit<React.ComponentProps<"input">, "type" | "prefix">;

/**
 * A password field with a show/hide toggle at its end. id and aria-* go to
 * the input (Field wires them), so it sits in a Field like any Input. Give
 * it `autoComplete="current-password"` or `"new-password"`: password
 * managers fill and save by it.
 */
export function PasswordInput({ className, disabled, ...props }: PasswordInputProps) {
  const [shown, setShown] = React.useState(false);
  return (
    <div
      data-slot="input-group"
      className={cn(
        controlClasses,
        "flex h-9 items-center overflow-hidden sm:h-8",
        "has-[input:focus-visible]:border-ring has-[input:focus-visible]:ring-3 has-[input:focus-visible]:ring-ring/20",
        "has-[input[aria-invalid=true]]:border-destructive has-[input[aria-invalid=true]:focus-visible]:ring-destructive/20",
        "has-[input:disabled]:cursor-not-allowed has-[input:disabled]:bg-muted",
        className,
      )}
    >
      <input
        data-slot="input"
        type={shown ? "text" : "password"}
        disabled={disabled}
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
        className="h-full min-w-0 flex-1 bg-transparent pl-2.5 text-foreground outline-none placeholder:text-faint-foreground disabled:cursor-not-allowed disabled:text-faint-foreground"
        {...props}
      />
      <Button
        variant="ghost"
        size="icon-sm"
        className="mr-0.5 shrink-0 text-faint-foreground hover:text-foreground"
        aria-label={shown ? "Hide password" : "Show password"}
        aria-pressed={shown}
        disabled={disabled}
        onClick={() => setShown((value) => !value)}
      >
        {shown ? <EyeClosedIcon /> : <EyeIcon />}
      </Button>
    </div>
  );
}
