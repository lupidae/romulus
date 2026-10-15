/**
 * Dev-only screens (/app/dev/*: the kit, the shell, the schema viewer on
 * fixtures) exist in dev builds, and in a production build made with
 * VITE_DEV_ROUTES=true: the bundle the perf spec measures (`npm run perf`).
 * Never set it for a deployed build.
 */
export const devRoutes = import.meta.env.DEV || import.meta.env.VITE_DEV_ROUTES === "true";
