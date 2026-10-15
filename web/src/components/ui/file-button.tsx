import * as React from "react";

import { Button, type ButtonProps } from "./button";

interface FileButtonProps extends Omit<ButtonProps, "onClick" | "asChild"> {
  /** What the picker offers, as for `<input type="file">`: ".sql,text/plain". */
  accept?: string;
  onFile: (file: File) => void;
}

/**
 * A button that opens the file picker and hands over the chosen file. The
 * file input itself stays hidden; the button is what people see and focus.
 */
export function FileButton({ accept, onFile, children, ...props }: FileButtonProps) {
  const inputRef = React.useRef<HTMLInputElement>(null);
  return (
    <>
      <Button {...props} onClick={() => inputRef.current?.click()}>
        {children}
      </Button>
      <input
        ref={inputRef}
        type="file"
        accept={accept}
        tabIndex={-1}
        aria-hidden
        className="hidden"
        onChange={(event) => {
          const file = event.target.files?.[0];
          // Cleared, so choosing the same file again still fires.
          event.target.value = "";
          if (file) onFile(file);
        }}
      />
    </>
  );
}
