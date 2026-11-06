import { defineConfig } from "@playwright/test";
import { resolve } from "node:path";
import { randomUUID } from "node:crypto";

process.env.QSCOPE_E2E_DIR ||= resolve(
  import.meta.dirname,
  "../../.tmp/webui-e2e",
  randomUUID(),
);

export default defineConfig({
  testDir: "./tests",
  testMatch: "**/*.spec.ts",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  use: {
    baseURL: "http://127.0.0.1:19841",
    browserName: "chromium",
    channel: process.env.QSCOPE_BROWSER_CHANNEL || "msedge",
    viewport: { width: 1600, height: 1000 },
    trace: "retain-on-failure",
  },
  webServer: {
    command: "node tests/start-server.mjs",
    url: "http://127.0.0.1:19841",
    reuseExistingServer: process.env.QSCOPE_REUSE_SERVER === "1",
    timeout: 120_000,
  },
});
