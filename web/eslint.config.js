import js from "@eslint/js";
import pluginQuery from "@tanstack/eslint-plugin-query";
import pluginRouter from "@tanstack/eslint-plugin-router";
import jsxA11y from "eslint-plugin-jsx-a11y";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";
import tseslint from "typescript-eslint";

// The design system is enforced here, not by review. Feature code composes the
// kit in src/components/ui and the tokens in src/styles/globals.css; these
// selectors catch the usual ways around them. The kit itself is exempt.
const designSystem = [
  {
    selector: "JSXOpeningElement[name.name=/^(button|input|select|textarea)$/]",
    message:
      "Use the kit (Button, Input, Select, Textarea from @/components/ui) instead of a raw form control.",
  },
  {
    selector: "JSXAttribute[name.name='className'] Literal[value=/\\[(#|rgb|hsl|oklch|color:)/]",
    message: "No ad-hoc colors: use a token class (bg-card, text-muted-foreground, …).",
  },
  {
    selector:
      "JSXAttribute[name.name='className'] TemplateElement[value.raw=/\\[(#|rgb|hsl|oklch|color:)/]",
    message: "No ad-hoc colors: use a token class (bg-card, text-muted-foreground, …).",
  },
  {
    selector: "JSXAttribute[name.name='className'] Literal[value=/(^|\\s)dark:/]",
    message: "Tokens already switch with the theme; feature code never needs dark: variants.",
  },
  {
    selector:
      "JSXAttribute[name.name='className'] Literal[value=/(^|\\s)uppercase(\\s|$)/][value=/tracking-/]",
    message: "Labels are sentence case, never letter-spaced capitals.",
  },
  {
    selector:
      "JSXAttribute[name.name='style'] Property[key.name=/^(color|background|backgroundColor|borderColor)$/]",
    message: "No inline colors: use token classes.",
  },
];

export default tseslint.config(
  {
    ignores: [
      "dist",
      "test-results",
      "playwright-report",
      "src/routeTree.gen.ts",
      "src/api/schema.gen.ts",
      "src/components/icons",
    ],
  },
  {
    extends: [
      js.configs.recommended,
      ...tseslint.configs.recommendedTypeChecked,
      jsxA11y.flatConfigs.recommended,
      ...pluginQuery.configs["flat/recommended"],
      ...pluginRouter.configs["flat/recommended"],
    ],
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2023,
      globals: globals.browser,
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "error",
        { allowConstantExport: true, allowExportNames: ["Route"] },
      ],
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      "@typescript-eslint/consistent-type-imports": "error",
      "@typescript-eslint/only-throw-error": "off",
      "no-restricted-syntax": ["error", ...designSystem],
    },
  },
  {
    // Route files keep their page components next to `Route`; the router's
    // code splitting, not react-refresh's file rule, makes them hot-reload.
    files: ["src/routes/**"],
    rules: { "react-refresh/only-export-components": "off" },
  },
  {
    // The kit is where raw primitives and token plumbing legitimately live.
    files: ["src/components/ui/**"],
    rules: {
      "no-restricted-syntax": "off",
      "react-refresh/only-export-components": "off",
    },
  },
  {
    files: ["*.config.{js,ts}", "scripts/**", "e2e/**"],
    languageOptions: { globals: globals.node },
  },
  {
    files: ["**/*.test.{ts,tsx}", "src/test/**"],
    rules: { "no-restricted-syntax": "off" },
  },
);
