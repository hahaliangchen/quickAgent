import { defineConfig } from '@playwright/test'
export default defineConfig({
  testDir: './tests', testMatch: 'blueprint-workbench.spec.mjs', outputDir: '../target/playwright-blueprint',
  workers: 1, timeout: 90_000,
  use: { browserName: 'chromium', viewport: { width: 1440, height: 900 }, trace: 'retain-on-failure' },
})

