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
    ".cache/",
    ".tmp/",
    "artifacts/",
    "build/",
    "apps/desktop/gen/",
    "apps/web/src/api.generated.ts",
  ]),
  {
    files: ["**/*.{js,mjs,cjs,ts,tsx}"],
    extends: [js.configs.recommended, tseslint.configs.recommended],
  },
  {
    files: [
      "scripts/**/*.{js,mjs,cjs,ts}",
      "tests/**/*.{js,mjs,cjs,ts}",
      "collectors/**/*.{js,cjs}",
      "apps/web/vite.config.ts",
      "eslint.config.js",
    ],
    languageOptions: { globals: globals.node },
  },
  {
    files: ["apps/web/src/**/*.{ts,tsx}"],
    extends: [reactHooks.configs.flat.recommended],
    languageOptions: { globals: globals.browser },
  },
  {
    // 运行于 QQ 主进程，同时具备 Node 与浏览器全局对象。
    files: ["collectors/qqnt-windows/automation.js"],
    languageOptions: {
      sourceType: "script",
      globals: { ...globals.node, ...globals.browser },
    },
  },
]);
