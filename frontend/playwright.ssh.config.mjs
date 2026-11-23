import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './tests',
  testMatch: 'ssh-workbench.spec.mjs',
  outputDir: '../target/playwright-ssh',
  workers: 1,
  timeout: 45_000,
  use: { browserName: 'chromium', viewport: { width: 1380, height: 870 }, trace: 'retain-on-failure' },
})
