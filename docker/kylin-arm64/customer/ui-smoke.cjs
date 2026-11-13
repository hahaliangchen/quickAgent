const fs = require('node:fs');
const { chromium } = require('/opt/ppt-tools/node_modules/playwright');

(async () => {
    const fixture = JSON.parse(fs.readFileSync('/tmp/customer-fixture.json','utf8'));
    const browser = await chromium.launch({executablePath:process.env.PPT_DESIGN_BROWSER,headless:true,args:['--no-sandbox']});
    let page;
    const failures = [];
    try {
        const context=await browser.newContext({viewport:{width:1440,height:1000},httpCredentials:{username:'admin',password:fixture.password,origin:'http://app:3002'}});
        page=await context.newPage();
        page.on('response', async response => { if(response.url().includes('/ppt-agent/')) console.error('Agent HTTP:',response.status(),new URL(response.url()).pathname); });
        page.on('requestfailed', request=>console.error('Request failure:',new URL(request.url()).pathname,request.failure()?.errorText));
        page.on('pageerror',error=>failures.push(error.message));
        if (fixture.cleanup) {
            await page.goto('http://app:3002/agent/settings',{waitUntil:'domcontentloaded',timeout:180000});
            const dialog=page.getByRole('dialog',{name:'设置',exact:true});
            await dialog.getByRole('button',{name:'模型与提供商',exact:true}).click();
            await dialog.getByRole('button',{name:'删除',exact:true}).waitFor({timeout:120000});
            while(await dialog.getByRole('button',{name:'删除',exact:true}).count())await dialog.getByRole('button',{name:'删除',exact:true}).first().click();
            await dialog.getByRole('button',{name:'保存更改',exact:true}).click();
            await dialog.getByText('设置已保存并立即对新任务生效。',{exact:true}).waitFor({timeout:120000});
            const empty=await (await page.request.get('http://app:3002/agent/settings/data')).json();
            if(empty.providers.length||empty.orchestrator_provider||empty.orchestrator_model)throw Error('Model cleanup was incomplete');
            await page.screenshot({path:'/tmp/customer-quickagent-empty.png',fullPage:true,timeout:120000});
            if(failures.length)throw Error(JSON.stringify(failures));
            console.log(JSON.stringify({all_models_deleted:true,empty_settings_page:true}));
            return;
        }
        await page.goto('http://app:8899/login',{waitUntil:'domcontentloaded',timeout:180000});
        await page.getByPlaceholder('请输入账户名').fill('admin');
        await page.getByPlaceholder('请输入密码',{exact:true}).fill(fixture.password);
        await page.locator('button[type=submit]').click();
        await page.waitForURL('**/create-ppt',{timeout:180000});
        await page.locator('nav.menu-list .menu-item').first().waitFor({timeout:120000});
        const menus = (await page.locator('nav.menu-list .menu-item').allTextContents()).map(x=>x.trim());
        if (menus.length !== 2 || !menus[0].includes('创建PPT') || !menus[1].includes('历史会话')) throw new Error('Unexpected customer navigation: '+JSON.stringify(menus));
        await page.waitForFunction(()=>document.querySelector('img.brand-logo-img')?.naturalWidth>0,{},{timeout:60000});
        if (await page.getByText('前往素材库',{exact:true}).count()) throw new Error('Customer page exposes the library entry');
        await page.screenshot({path:'/tmp/customer-create-ppt.png',fullPage:true,timeout:120000});
        await page.locator('nav.menu-list .menu-item').filter({hasText:'历史会话'}).click();
        await page.waitForURL('**/ppt-history',{timeout:120000});
        await page.locator('.ppt-history-summary h2').waitFor({timeout:120000});
        await page.waitForFunction(()=>document.querySelector('.ppt-history-card') || document.querySelector('.ppt-history-empty strong')?.textContent==='暂无历史会话',{},{timeout:120000});
        await page.screenshot({path:'/tmp/customer-ppt-history.png',fullPage:true,timeout:120000});
        await page.goto('http://app:3002/agent/settings',{waitUntil:'domcontentloaded',timeout:180000});
        const dialog=page.getByRole('dialog',{name:'设置',exact:true});
        await dialog.waitFor({timeout:120000});
        await dialog.getByRole('button',{name:'模型与提供商',exact:true}).click();
        await dialog.getByRole('button',{name:'添加模型提供商',exact:true}).click();
        await dialog.getByPlaceholder('例如 deepseek',{exact:true}).fill('qwen-test');
        await dialog.getByPlaceholder('例如 DeepSeek Official').fill('Qwen 客户配置验证');
        await dialog.getByPlaceholder('https://api.deepseek.com/v1',{exact:true}).fill(fixture.model_url);
        await dialog.locator('textarea').fill(fixture.model_id);
        await dialog.getByRole('switch',{name:'系统消息集中到开头',exact:true}).click();
        if(await dialog.getByPlaceholder('免鉴权服务可留空').inputValue())throw Error('Expected a blank API key');
        await dialog.getByRole('combobox',{name:'主代理提供商',exact:true}).selectOption('qwen-test');
        await dialog.getByRole('button',{name:'收起',exact:true}).click();
        await dialog.getByRole('button',{name:'编辑',exact:true}).click();
        await dialog.getByRole('button',{name:'专用模型',exact:true}).click();
        await dialog.getByRole('button',{name:'模型与提供商',exact:true}).click();
        if(await dialog.getByPlaceholder('例如 DeepSeek Official').inputValue()!=='Qwen 客户配置验证')throw Error('QuickAgent unsaved provider draft was overwritten');
        await dialog.getByRole('button',{name:'保存更改',exact:true}).click();
        await dialog.getByText('设置已保存并立即对新任务生效。',{exact:true}).waitFor({timeout:120000});
        const saved=await (await page.request.get('http://app:3002/agent/settings/data')).json();
        if(saved.providers.length!==1||saved.providers[0].id!=='qwen-test'||saved.providers[0].has_api_key||saved.orchestrator_model!==fixture.model_id)throw Error('Keyless Qwen setting was not persisted');
        await page.screenshot({path:'/tmp/customer-quickagent-settings.png',fullPage:true,timeout:120000});
        if (failures.length) throw new Error('Browser errors: '+JSON.stringify(failures));
        console.log(JSON.stringify({browser_login:true,menus:['创建PPT','历史会话'],history_navigation:true,quickagent_page:true,quickagent_draft_preserved:true,quickagent_settings_saved:true,keyless_qwen_configured:true,uncaught_errors:0}));
    } catch (error) {
        console.error("Original failure:",error.stack||error);
        if (page && !page.isClosed()) {
            await page.screenshot({path:'/tmp/customer-ui-failure.png',fullPage:true,timeout:60000}).catch(()=>{});
            console.error('Failed page: '+page.url());
            console.error('Page text: '+(await page.locator('body').innerText().catch(()=>'' )).slice(-1600));
            console.error('Runtime errors: '+JSON.stringify(failures));
        }
        throw error;
    } finally {await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
