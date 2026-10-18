import * as React from "react";

import {
  type ConsoleSession,
  type ConsoleState,
  ConsoleSession as Session,
  consoleUrl,
} from "./console";

/**
 * The console session of a playground, created in an effect (StrictMode
 * mounts twice: a socket made during render would leak) and closed when the
 * page leaves. `null` until it exists, or while `enabled` is false (the
 * playground is not ready).
 */
export function useConsoleSession(
  org: string,
  db: string,
  id: string,
  enabled: boolean,
): ConsoleSession | null {
  const [session, setSession] = React.useState<ConsoleSession | null>(null);
  React.useEffect(() => {
    if (!enabled) return;
    const created = new Session(consoleUrl(org, db, id));
    // The session owns a socket: it can only be made here, then handed to render.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSession(created);
    return () => {
      created.close();
      setSession(null);
    };
  }, [org, db, id, enabled]);
  return session;
}

const noop = () => () => {};

/** A slice of the session's state, re-rendering when it changes. */
export function useConsoleState<T>(
  session: ConsoleSession | null,
  select: (state: ConsoleState) => T,
  fallback: T,
): T {
  const subscribe = session ? session.subscribe : noop;
  const get = () => (session ? select(session.getState()) : fallback);
  return React.useSyncExternalStore(subscribe, get, get);
}
