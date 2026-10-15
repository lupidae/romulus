/**
 * `value` if it is a path on this origin, else undefined: for redirect
 * targets that come from the URL (`?next=`), which an attacker controls.
 *
 * Browsers treat `\` like `/`, so `/\evil.com` would leave the site; control
 * characters are stripped by URL parsers in ways that differ between them.
 * The API must check the target again before redirecting.
 */
export function safePath(value: unknown): string | undefined {
  if (typeof value !== "string" || !value.startsWith("/") || value.startsWith("//")) {
    return undefined;
  }
  // eslint-disable-next-line no-control-regex -- control characters are exactly what is refused
  if (/[\\\u0000-\u001f\u007f]/.test(value)) return undefined;
  return value;
}
