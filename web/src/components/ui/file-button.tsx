import * as React from "react";

import { Button, type ButtonProps } from "./button";

type FileButtonProps = Omit<ButtonProps, "onClick" | "asChild"> & {
  /** What the picker offers, as for `<input type="file">`: ".sql,text/plain". */
  accept?: string;
} & (
    | { onFile: (file: File) => void; multiple?: never; directory?: never; onFiles?: never }
    | {
        /** Several files at once. */
        multiple?: boolean;
        /** A folder: every file in it, each with its `webkitRelativePath`. */
        directory?: boolean;
        onFiles: (files: File[]) => void;
        onFile?: never;
      }
  );

/**
 * A button that opens the file picker and hands over the chosen file (or
 * files, or a folder's files). The file input itself stays hidden; the
 * button is what people see and focus.
 */
export function FileButton({
  accept,
  onFile,
  onFiles,
  multiple,
  directory,
  children,
  ...props
}: FileButtonProps) {
  const inputRef = React.useRef<HTMLInputElement>(null);
  // Not in React's types; every browser supports it.
  const folder = directory ? { webkitdirectory: "" } : {};
  return (
    <>
      <Button {...props} onClick={() => inputRef.current?.click()}>
        {children}
      </Button>
      <input
        ref={inputRef}
        type="file"
        accept={accept}
        multiple={multiple ?? directory}
        tabIndex={-1}
        aria-hidden
        className="hidden"
        {...folder}
        onChange={(event) => {
          const files = Array.from(event.target.files ?? []);
          // Cleared, so choosing the same file again still fires.
          event.target.value = "";
          if (files.length === 0) return;
          if (onFiles) onFiles(files);
          else onFile?.(files[0]!);
        }}
      />
    </>
  );
}
