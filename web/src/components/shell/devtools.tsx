import { TanStackDevtools } from "@tanstack/react-devtools";
import { ReactQueryDevtoolsPanel } from "@tanstack/react-query-devtools";
import { TanStackRouterDevtoolsPanel } from "@tanstack/react-router-devtools";

/**
 * Router and Query devtools in one panel behind one button (middle of the
 * right edge: clear of the toasts, the sidebar's toggle and the graph's
 * controls). Loaded in dev only (see routes/__root.tsx), never
 * part of a production bundle.
 */
export default function Devtools() {
  return (
    <TanStackDevtools
      config={{ position: "middle-right" }}
      plugins={[
        { name: "Router", render: <TanStackRouterDevtoolsPanel /> },
        { name: "Query", render: <ReactQueryDevtoolsPanel /> },
      ]}
    />
  );
}
