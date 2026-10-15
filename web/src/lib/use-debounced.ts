import * as React from "react";

/**
 * `fn`, run `ms` after the last call with that call's arguments: a burst of
 * calls (j held down) runs it once. A call still waiting is dropped when
 * the component unmounts.
 */
export function useDebounced<A extends unknown[]>(fn: (...args: A) => void, ms: number) {
  const latest = React.useRef(fn);
  React.useEffect(() => {
    latest.current = fn;
  });
  const timer = React.useRef<number | undefined>(undefined);
  React.useEffect(() => () => window.clearTimeout(timer.current), []);
  return React.useCallback(
    (...args: A) => {
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => latest.current(...args), ms);
    },
    [ms],
  );
}
