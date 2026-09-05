import { defineConfig } from "eslint/config";
import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import tseslint from "typescript-eslint";
import convexPlugin from "@convex-dev/eslint-plugin";

export default defineConfig([
  {
    ignores: [
      "dist",
      "eslint.config.js",
      "convex/_generated",
      "postcss.config.js",
      "tailwind.config.js",
      "vite.config.ts",
    ],
  },
  {
    extends: [
      js.configs.recommended,
      ...tseslint.configs.recommendedTypeChecked,
    ],
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2020,
      globals: {
        ...globals.browser,
        ...globals.node,
      },
      parserOptions: {
        project: [
          "./tsconfig.node.json",
          "./tsconfig.app.json",
          "./convex/tsconfig.json",
          "./tsconfig.e2e.json",
          "./tsconfig.functions.json",
        ],
      },
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      // `no-explicit-any` and the five `no-unsafe-*` rules used to be off here,
      // under a starter-template comment about stricter linting "down the
      // line". That line arrived: turning them back on is what keeps an id
      // crossing the Convex boundary an id, rather than something laundered
      // through `as never` on its way into a mutation that wanted a different
      // table's. Reach for a type, not for the switch.

      // Only warn on unused variables, and ignore variables starting with `_`
      "@typescript-eslint/no-unused-vars": [
        "warn",
        { varsIgnorePattern: "^_", argsIgnorePattern: "^_" },
      ],

      // Allow escaping the compiler
      "@typescript-eslint/ban-ts-comment": "error",

      // Allow async functions without await
      // for consistency (esp. Convex `handler`s)
      "@typescript-eslint/require-await": "off",
    },
  },
  {
    // Playwright's fixture API takes a callback named `use`, which the React
    // Hooks rule reads as a misplaced `use()` hook. There is no React here.
    files: ["e2e/**/*.ts"],
    rules: {
      "react-hooks/rules-of-hooks": "off",
    },
  },
  ...convexPlugin.configs.recommended,
]);
