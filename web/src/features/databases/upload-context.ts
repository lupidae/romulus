import * as React from "react";

/**
 * Opens the database page's upload dialog, from anywhere under it (the
 * header button, the empty state, `u`). `null` for reviewers.
 */
export const UploadContext = React.createContext<(() => void) | null>(null);

export function useOpenUpload() {
  return React.useContext(UploadContext);
}
