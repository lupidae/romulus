// The Pages Function of romulus.lupidae.com: /app and /api go to the API
// (edge/proxy.js), anything else is the static homepage. public/_routes.json
// keeps the Function off every other path.
import { proxied, proxy } from "../edge/proxy.js";

export async function onRequest({ request, env, next }) {
  if (!proxied(new URL(request.url).pathname)) return next();
  return proxy(request, env);
}
