// The homepage's live hero (src/hero): one script, one stylesheet and their
// chunks and wasm, built into site/public/hero/ by site/tools/hero.mjs and
// committed, so Cloudflare Pages serves the site as files.
//
// Every CSS rule is scoped to `.romulus-hero` (the island and the element
// its menus and dialogs render into), and the kit's toaster is swapped for
// one that needs no inline <style> (the homepage's CSP allows none).

import { fileURLToPath, URL } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import type { AtRule, Plugin as PostcssPlugin, Rule } from "postcss";
import { defineConfig, type Plugin } from "vite";

const SCOPE = ".romulus-hero";
const here = (path: string) => fileURLToPath(new URL(path, import.meta.url));

/** `a, b:is(c, d)` split on its top-level commas. */
function selectors(list: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < list.length; i++) {
    const c = list[i];
    if (c === "(" || c === "[") depth++;
    else if (c === ")" || c === "]") depth--;
    else if (c === "," && depth === 0) {
      out.push(list.slice(start, i).trim());
      start = i + 1;
    }
  }
  out.push(list.slice(start).trim());
  return out.filter(Boolean);
}

function scoped(selector: string): string {
  // Variables on the document stay there: the theme's spacing, durations.
  if (/^(:root|:host)\b/.test(selector)) return selector;
  // The document's own element is the island here.
  if (/^(html|body)\b/.test(selector)) return selector.replace(/^(html|body)/, SCOPE);
  // Resets apply to the island's own elements, and the island.
  if (/^(\*|::)/.test(selector)) return `${SCOPE} ${selector}`;
  return `:where(${SCOPE}) ${selector}`;
}

const scope: PostcssPlugin = {
  postcssPlugin: "romulus-hero-scope",
  Rule(rule: Rule) {
    let parent = rule.parent;
    while (parent && parent.type !== "root") {
      if (parent.type === "atrule" && /keyframes$/.test((parent as AtRule).name)) return;
      parent = parent.parent;
    }
    if (rule.selector.includes(SCOPE)) return;
    rule.selector = selectors(rule.selector).map(scoped).join(", ");
  },
};

/**
 * CodeMirror's styles (style-mod) go in a constructable stylesheet: in a
 * document, style-mod writes a <style> element, which the homepage's CSP
 * refuses (`style-src 'self'`); the CSSOM is not subject to it. style-mod
 * already does this in shadow roots; this takes that path in the document.
 */
const adoptedStyles: Plugin = {
  name: "romulus-hero-style-mod",
  transform(code, id) {
    if (!/style-mod[\\/]src[\\/]style-mod\.js$/.test(id)) return;
    const from = "!root.head && root.adoptedStyleSheets && win.CSSStyleSheet";
    if (!code.includes(from))
      throw new Error("style-mod changed: update romulus-hero-style-mod in vite.hero.config.ts");
    return code.replace(from, "root.adoptedStyleSheets && win.CSSStyleSheet");
  },
};

/**
 * CodeMirror scrolls every scrollable ancestor to bring the caret or a
 * thread into view, the window included: on the homepage that would carry
 * the visitor off what they are reading. Its own scroller still scrolls.
 */
const editorScrollsItself: Plugin = {
  name: "romulus-hero-codemirror-scroll",
  transform(code, id) {
    if (!/@codemirror[\\/]view[\\/]dist[\\/]index\.js$/.test(id)) return;
    const from = "if (top) {\n                    win.scrollBy(moveX, moveY);\n                }";
    if (!code.includes(from))
      throw new Error(
        "@codemirror/view changed: update romulus-hero-codemirror-scroll in vite.hero.config.ts",
      );
    return code.replace(
      from,
      "if (top) {\n                    // The homepage's window: left where it is.\n                }",
    );
  },
};

export default defineConfig({
  base: "/hero/",
  // The app's public/ (favicons) belongs to the app, not to the homepage.
  publicDir: false,
  plugins: [react(), tailwindcss(), adoptedStyles, editorScrollsItself],
  resolve: {
    alias: [
      { find: "@/components/ui/toaster", replacement: here("./src/hero/toaster.tsx") },
      { find: "@", replacement: here("./src") },
      // Written by site/tools/hero.mjs: crates/romulus-wasm through wasm-bindgen.
      { find: /^romulus-wasm$/, replacement: here("./.hero/romulus-wasm/romulus_wasm.js") },
      { find: /^romulus-wasm\//, replacement: `${here("./.hero/romulus-wasm")}/` },
    ],
  },
  css: { postcss: { plugins: [scope] } },
  build: {
    outDir: here("../site/public/hero"),
    emptyOutDir: true,
    target: "es2022",
    cssCodeSplit: false,
    assetsInlineLimit: 0,
    sourcemap: false,
    rollupOptions: {
      input: here("./src/hero/main.tsx"),
      preserveEntrySignatures: "exports-only",
      output: {
        entryFileNames: "hero.js",
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: (asset) =>
          asset.names.some((n) => n.endsWith(".css"))
            ? "hero.css"
            : "assets/[name]-[hash][extname]",
      },
    },
  },
});
