const fs = require('node:fs');
const { chromium } = require('/opt/ppt-tools/node_modules/playwright');

(async () => {
    const fixture = JSON.parse(fs.readFileSync('/tmp/customer-fixture.json','utf8'));
    const browser = await chromium.launch({executablePath:process.env.PPT_DESIGN_BROWSER,headless:true,args:['--no-sandbox']});
    let page;
    try {
        page = await browser.newPage({viewport:{width:1440,height:1000}});
        const failures = [];
        page.on('pageerror',error=>failures.push(error.message));
        await page.goto('http://app:8899/login',{waitUntil:'domcontentloaded',timeout:180000});
        await page.getByPlaceholder('请输入账户名').fill('admin');
        await page.getByPlaceholder('请输入密码',{exact:true}).fill(fixture.password);
        await page.locator('button[type=submit]').click();
        await page.waitForURL('**/create-ppt',{timeout:180000});
        await page.locator('nav.menu-list .menu-item').first().waitFor({timeout:120000});
        const menus = (await page.locator('nav.menu-list .menu-item').allTextContents()).map(x=>x.trim());
        if (menus.length !== 2 || !menus[0].includes('创建PPT') || !menus[1].includes('历史会话')) throw new Error('Unexpected customer navigation: '+JSON.stringify(menus));
        await page.getByText('DeepSeek Flash',{exact:false}).first().waitFor({timeout:120000});
        await page.waitForFunction(()=>document.querySelector('img.brand-logo-img')?.naturalWidth>0,{},{timeout:60000});
        if (await page.getByText('前往素材库',{exact:true}).count()) throw new Error('Customer page exposes the library entry');
        await page.screenshot({path:'/tmp/customer-create-ppt.png',fullPage:true,timeout:120000});
        await page.locator('nav.menu-list .menu-item').filter({hasText:'历史会话'}).click();
        await page.waitForURL('**/ppt-history',{timeout:120000});
        await page.locator('.ppt-history-summary h2').waitFor({timeout:120000});
        await page.waitForFunction(()=>document.querySelector('.ppt-history-card') || document.querySelector('.ppt-history-empty strong')?.textContent==='暂无历史会话',{},{timeout:120000});
        await page.screenshot({path:'/tmp/customer-ppt-history.png',fullPage:true,timeout:120000});
        if (failures.length) throw new Error('Browser errors: '+JSON.stringify(failures));
        console.log(JSON.stringify({browser_login:true,menus:['创建PPT','历史会话'],deepseek_visible:true,history_navigation:true,uncaught_errors:0}));
    } catch (error) {
        if (page) {
            await page.screenshot({path:'/tmp/customer-ui-failure.png',fullPage:true,timeout:60000}).catch(()=>{});
            console.error('Failed page: '+page.url());
        }
        throw error;
    } finally {await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
