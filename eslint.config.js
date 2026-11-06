import js from "@eslint/js";
import { defineConfig, globalIgnores } from "eslint/config";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

export default defineConfig([
  globalIgnores([
    "**/node_modules/",
    "**/target/",
    "**/dist/",
    "**/test-results/",
    "**/playwright-report/",
    ".tmp/",
    "artifacts/",
    "build/",
    "apps/desktop/gen/",
    "apps/web/src/api.generated.ts",
  ]),
  {
    files: ["**/*.{js,mjs,cjs,ts,tsx}"],
    extends: [js.configs.recommended, tseslint.configs.recommended],
    languageOptions: { globals: globals.node },
  },
  {
    files: ["apps/web/src/**/*.{ts,tsx}"],
    extends: [reactHooks.configs.flat.recommended],
    languageOptions: { globals: globals.browser },
    rules: {
      // Existing effects reset local state when their inputs change; warn until each is reworked.
      "react-hooks/set-state-in-effect": "warn",
    },
  },
  {
    // Runs inside the QQ main process, where both Node and browser globals exist.
    files: ["collectors/qqnt-windows/automation.js"],
    languageOptions: {
      sourceType: "script",
      globals: { ...globals.node, ...globals.browser },
    },
  },
]);
