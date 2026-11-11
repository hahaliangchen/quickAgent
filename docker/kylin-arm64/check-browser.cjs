const assert = require('node:assert/strict');
assert.equal(process.arch, 'arm64');
const { chromium } = require(process.env.PPT_PLAYWRIGHT_MODULE);
(async () => {
  const args = ['--no-sandbox', '--disable-dev-shm-usage'];
  if (process.env.QUICKAGENT_ARM64_EMULATED === '1') {
    args.push('--disable-gpu', '--disable-software-rasterizer', '--no-zygote', '--single-process');
  }
  const browser = await chromium.launch({ headless: true, timeout: 60000, args,
    ...(process.env.PPT_DESIGN_BROWSER ? { executablePath: process.env.PPT_DESIGN_BROWSER } : {}) });
  try {
    const page = await browser.newPage();
    await page.setContent('<meta charset="utf-8"><h1 style="font-family:Noto Sans CJK SC">麒麟 ARM64 PPT</h1>');
    assert.equal(await page.locator('h1').textContent(), '麒麟 ARM64 PPT');
    assert((await page.screenshot()).length > 1000);
    console.log('ARM64 Chromium/Playwright rendering: OK (' + browser.version() + ')');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
