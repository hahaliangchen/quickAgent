#!/usr/bin/env python3
"""Exercise the release with a fresh, isolated customer database and no real API key."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import platform
import secrets
import shutil
import subprocess
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--work',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--existing',action='store_true')
    parser.add_argument('--keep-running',action='store_true')
    args = parser.parse_args()
    args.output = args.output.resolve()
    work = args.work.resolve()
    if not args.existing:
        work.mkdir(parents=True,exist_ok=False)
        values = {name:secrets.token_hex(16) for name in ['MYSQL_ROOT_PASSWORD','MYSQL_PASSWORD','REDIS_PASSWORD','JWT_SIGNING_KEY','CUSTOMER_ADMIN_PASSWORD']}
        values.update(COMPOSE_PROJECT_NAME='ppt-smoke-'+secrets.token_hex(4), DEEPSEEK_API_KEY='smoke-no-external-api', PPT_PORT='18899', PPT_BIND='127.0.0.1', QUICKAGENT_ARM64_EMULATED='1' if platform.machine()=='x86_64' else '0')
        (work/'.env').write_text(''.join(f'{k}={v}\n' for k,v in values.items()))
        (work/'.env').chmod(0o600)
        shutil.copy2(HERE/'compose.yaml',work/'compose.yaml')
    values = dict(line.split('=',1) for line in (work/'.env').read_text().splitlines() if '=' in line)
    compose = ['docker','compose','-f','compose.yaml']
    if values.get('QUICKAGENT_ARM64_EMULATED')=='1':
        # QEMU exposes host /proc mappings to Redis's guest-address COW probe.
        # Only this disposable fixture suppresses that probe and disables fork
        # persistence. The shipped native-ARM64 Redis keeps its safety check.
        (work/'compose.smoke.yaml').write_text('services:\n  redis:\n    command: ["redis-server", "--save", "", "--appendonly", "no", "--ignore-warnings", "ARM64-COW-BUG", "--requirepass", "${REDIS_PASSWORD}"]\n')
        compose += ['-f','compose.smoke.yaml']
    log = (work/'verification.log').open('a')
    def run(command, *, capture=False, data=None, timeout=600):
        return subprocess.run(command,cwd=work,input=data,text=True,stdout=subprocess.PIPE if capture else log,stderr=log,check=True,timeout=timeout).stdout
    def dc(*command,**kwargs): return run(compose+list(command),**kwargs)
    base='http://127.0.0.1:'+values['PPT_PORT']
    def post(path,body,token=''):
        request=urllib.request.Request(base+path,data=json.dumps(body).encode(),headers={'Content-Type':'application/json','x-token':token,'Authorization':'Bearer '+token})
        with urllib.request.urlopen(request,timeout=180) as response: return json.load(response)
    try:
        if not args.existing: dc('run','--rm','init')
        dc('up','-d','--wait','--wait-timeout','480','app')
        login=post('/prod-api/base/login',{'username':'admin','password':hashlib.md5((values['CUSTOMER_ADMIN_PASSWORD']+'ai-ppt-login-salt-v1').encode()).hexdigest(),'captcha':'','captchaId':''})
        assert login['code']==0, login.get('msg')
        token=login['data']['token']
        menus=post('/prod-api/menu/getMenu',{},token)['data']['menus']
        assert [m['path'] for m in menus]==['create-ppt','ppt-history']
        connection=post('/ppt-agent/agent/plugins/ppt-master/connect',{},token)
        handle=connection['connection_handle']
        models=post('/ppt-agent/agent/plugins/ppt-master/session',{'method':'models','connection_handle':handle},token)
        expected={('deepseek','deepseek-flash'),('deepseek','deepseek-v4-pro')}
        assert {(m['provider'],m['id']) for m in models['models']}==expected, models
        assert all(m['enabled'] and m['available_for_agent'] for m in models['models'])
        created=post('/ppt-agent/agent/plugins/ppt-master/session',{'method':'create','connection_handle':handle,'name':'客户部署验证'},token)
        assert created.get('task_id'), created
        try:
            post('/ppt-agent/agent/plugins/ppt-master/connect',{})
            raise AssertionError('Anonymous connection unexpectedly succeeded')
        except urllib.error.HTTPError as error:
            assert error.code==400 and 'Plugin login required' in error.read().decode()
        sql="SELECT CONCAT('provider:',name) FROM sys_ai_providers WHERE deleted_at IS NULL; SELECT CONCAT('model:',name) FROM sys_ai_models WHERE deleted_at IS NULL; SELECT CONCAT('user:',username) FROM sys_users WHERE deleted_at IS NULL;"
        rows=dc('exec','-T','db','sh','-c','MYSQL_PWD="$MYSQL_ROOT_PASSWORD" exec mysql -N -uroot ai_ppt',capture=True,data=sql).splitlines()
        assert set(rows)=={'provider:deepseek','model:deepseek-flash','model:deepseek-v4-pro','user:admin'}, rows
        agent=json.loads(dc('exec','-T','app','python','-c',"import json,urllib.request;print(json.dumps(json.load(urllib.request.urlopen('http://127.0.0.1:3001/agent/models'))))",capture=True))
        assert {(m['provider'],m['id']) for m in agent['models']}==expected
        app_id=dc('ps','-q','app',capture=True).strip()
        renderer_id=dc('ps','-q','renderer',capture=True).strip()
        run(['docker','cp',str(HERE/'renderer-smoke.mts'),renderer_id+':/opt/ppt-engine/customer-renderer-smoke.mts'])
        render=json.loads(dc('exec','-T','renderer','node','/opt/ppt-engine/node_modules/vite-node/vite-node.mjs','/opt/ppt-engine/customer-renderer-smoke.mts',capture=True,timeout=600).splitlines()[-1])
        (work/'render-result.json').write_text(json.dumps(render)+'\n')
        fixture=work/'browser-fixture.json'
        fixture.write_text(json.dumps({'password':values['CUSTOMER_ADMIN_PASSWORD']}));fixture.chmod(0o600)
        run(['docker','cp',str(fixture),renderer_id+':/tmp/customer-fixture.json'])
        run(['docker','cp',str(HERE/'ui-smoke.cjs'),renderer_id+':/tmp/customer-ui-smoke.cjs'])
        ui=json.loads(dc('exec','-T','renderer','node','/tmp/customer-ui-smoke.cjs',capture=True,timeout=600).splitlines()[-1])
        (work/'ui-result.json').write_text(json.dumps(ui,ensure_ascii=False)+'\n')
        args.output.mkdir(parents=True,exist_ok=True)
        evidence=args.output/'verification'
        evidence.mkdir(exist_ok=True)
        for filename in ['customer-render.png','customer-render.pptx','customer-create-ppt.png','customer-ppt-history.png']:
            run(['docker','cp',renderer_id+':/tmp/'+filename,str(evidence/filename)])
        images={}
        for image in ['quickagent-customer-ppt-arm64:20261111','quickagent-customer-ppt-renderer-arm64:20261111','quickagent-customer-mysql-arm64:8.0','quickagent-customer-redis-arm64:7']:
            item=json.loads(run(['docker','image','inspect','--platform','linux/arm64',image],capture=True))[0]
            assert item['Architecture']=='arm64'
            images[image]=item['Id']
        report={'passed':True,'checked_at':datetime.now(timezone.utc).isoformat(),'host':platform.machine(),'execution':'ARM64 QEMU' if platform.machine()=='x86_64' else 'native ARM64','customer_login':True,'menus':[m['path'] for m in menus],'models':sorted([list(m) for m in expected]),'fresh_database_only_admin':True,'authenticated_plugin_connection':True,'session_creation':True,'anonymous_plugin_blocked':True,'render':render,'ui':ui,'images':images,'deepseek_api_generation_tested':False,'physical_halberd_server_tested':False,'redis_qemu_fixture_override':values.get('QUICKAGENT_ARM64_EMULATED')=='1'}
        (args.output/'verification.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
        print('Customer integration verification passed: '+str(args.output/'verification.json'),flush=True)
    finally:
        if not args.keep_running: dc('down','-v',timeout=120)
        log.close()


if __name__=='__main__': main()
