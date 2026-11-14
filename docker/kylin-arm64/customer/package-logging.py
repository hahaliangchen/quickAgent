#!/usr/bin/env python3
"""Add customer logging to the existing release; never run business workflows."""
import argparse
import hashlib
import json
import re
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
from datetime import datetime, timezone

HERE = Path(__file__).resolve().parent
BASE = '20261113-latest-empty-ppt'
VERSION = '20261113-logs'


def run(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def sha(path):
    digest=hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda:stream.read(8*1024*1024),b''):digest.update(block)
    return digest.hexdigest()


def inspect(tag):
    return json.loads(subprocess.check_output(['docker','image','inspect','--platform','linux/arm64',tag]))[0]


def package(base_archive, output):
    output.mkdir(parents=True, exist_ok=False)
    manifest=None
    with tarfile.open(base_archive, 'r|gz') as archive:
        for entry in archive:
            if entry.name.endswith('/manifest.json') and entry.size<2*1024*1024:
                manifest=json.load(archive.extractfile(entry));break
    if manifest is None:raise RuntimeError('Base release manifest not found')
    for name in ['compose.yaml','deploy.sh','collect-diagnostics.sh','diagnostics.py','customer_logging.py']:
        shutil.copy2(HERE/name,output/name)
        if name in ['compose.yaml','deploy.sh']:
            p=output/name
            p.write_text(re.sub(r'quickagent-customer-ppt-(?:renderer-)?arm64:[\w.-]+',lambda m:m.group().split(':')[0]+':'+VERSION,p.read_text()))
    for name in ['deploy.sh','collect-diagnostics.sh']:(output/name).chmod(0o755)
    base_app='quickagent-customer-ppt-arm64:'+BASE
    base_renderer='quickagent-customer-ppt-renderer-arm64:'+BASE
    assert inspect(base_app)['Id']==manifest['release_images'][base_app]['id'], 'Base application image differs from prior release'
    assert inspect(base_renderer)['Id']==manifest['release_images'][base_renderer]['id'], 'Base renderer image differs from prior release'
    app='quickagent-customer-ppt-arm64:'+VERSION
    renderer='quickagent-customer-ppt-renderer-arm64:'+VERSION
    with tempfile.TemporaryDirectory(prefix='customer-logging-') as temp:
        context=Path(temp)
        for name in ['entrypoint.py','customer_logging.py']:shutil.copy2(HERE/name,context/name)
        (context/'Dockerfile').write_text('FROM '+base_app+'\nCOPY entrypoint.py customer_logging.py /opt/customer/bin/\nENV CUSTOMER_LOG_DIR=/logs\n')
        run(['docker','build','--platform','linux/arm64','-t',app,str(context)])
    run(['docker','tag',base_renderer,renderer])
    # This container runs only isolated logging tests with synthetic SQLite data.
    # It starts no application, model, database service, browser or renderer.
    test_command=['docker','run','--rm','--platform','linux/arm64','--network','none','--entrypoint','python','-e','PYTHONPATH=/opt/customer/bin',
                  '-v',str(HERE/'test_customer_logging.py')+':/logging-checks/test_customer_logging.py:ro',
                  '-v',str(HERE/'diagnostics.py')+':/logging-checks/diagnostics.py:ro',
                  app,'-m','unittest','discover','-s','/logging-checks','-p','test_customer_logging.py','-v']
    result=run(test_command,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
    (output/'logging-checks.txt').write_text(result.stdout)
    print(result.stdout,flush=True)
    with tempfile.TemporaryDirectory(prefix='customer-log-mount-') as mounted:
        run(['docker','run','--rm','--platform','linux/arm64','--network','none','--user',str(os.getuid())+':'+str(os.getgid()),'--entrypoint','python',
             '-v',mounted+':/logs',app,'-c',
             "import sys;sys.path.insert(0,'/opt/customer/bin');from customer_logging import CustomerLog;l=CustomerLog('/logs');l.write('diagnostics','mount_check',task_id='synthetic-logging-check',message='PROGRAM_SOURCE_INVALID: synthetic binding failure');l.close()"])
        record=json.loads((Path(mounted)/'customer.log').read_text())
        assert record['event']=='mount_check' and 'synthetic binding failure' in record['message']
    with (output/'logging-checks.txt').open('a') as log:log.write('Host bind-mounted customer.log: OK\n')
    tags=[app,renderer,'quickagent-customer-mysql-arm64:8.0','quickagent-customer-redis-arm64:7']
    manifest['base_release']=base_archive.name
    manifest['release_variant']='external-customer-logs'
    manifest['logging_update']={'built_at':datetime.now(timezone.utc).isoformat(),'scope':'logging only; business binaries, frontend, renderer, database images unchanged','host_log':'./logs/customer.log','rotation':{'max_bytes':5*1024*1024,'backups':4},'diagnostics_command':'./deploy.sh diagnostics','validation':'6 logging-only tests in ARM64 image and a real host bind-mount write check; no model/browser/PPT integration workflows executed','source_sha256':{name:sha(HERE/name) for name in ['entrypoint.py','customer_logging.py','diagnostics.py','collect-diagnostics.sh','compose.yaml','deploy.sh','test_customer_logging.py','package-logging.py']}}
    manifest['release_images']={tag:{'id':inspect(tag)['Id'],'architecture':inspect(tag)['Architecture']} for tag in tags}
    assert all(item['architecture']=='arm64' for item in manifest['release_images'].values())
    (output/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n')
    (output/'README-部署.txt').write_text('''麒麟 ARM64 客户包：外部日志版（2026-11-13）

本次仅增加日志及诊断导出。业务二进制、前端、渲染器、数据库镜像沿用上一版。
没有运行模型调用、浏览器业务操作或 PPT 生成测试；仅运行日志写入、提取、轮转和脱敏检查。

首次部署：
1. ./deploy.sh load
2. ./deploy.sh init
3. ./deploy.sh up
4. 打开 http://服务器地址:8899；admin 初始密码见本机 .env 的 CUSTOMER_ADMIN_PASSWORD。
本包不预置模型或密钥，启动后在设置页添加；免鉴权服务的 key 留空。
QuickAgent 设置页：http://服务器地址:3001/agent/settings，入口密码同 .env。

升级：
保留原 .env、state/ 及 mysql-data、redis-data 数据卷；沿用同一 COMPOSE_PROJECT_NAME。
先停止旧版 app（不删除数据卷），加载新镜像，在原部署目录更新 compose.yaml、deploy.sh 和本包日志工具后执行 ./deploy.sh up。
不重新执行 init，不删除现有数据。也可以用新目录部署，但必须先带入原 .env 和 state/。

日志：
宿主机部署目录下 logs/customer.log。容器 /logs 显式挂载到这个目录。
包含 Agent、后端、网关和启动器输出；自动提取 program/source_invalid、程序运行错误、失败工具结果、模型连接错误及会话 ID。
每个日志文件上限 5 MiB，保留 4 个轮转备份（customer.log.1 至 .4），重启继续保留。
日志对常见 API key、Authorization、密码和 URL 鉴权参数脱敏。

出现问题后：
./deploy.sh diagnostics
将生成的 logs/customer-diagnostics-时间.tar.gz 发给维护人员即可。
也可直接提供 logs/customer.log 及其轮转备份，无需打开 F12。
诊断包提取已有会话的具体错误、最近任务状态及不含密钥的模型路由信息；不导出 .env、凭据文件、整库、完整对话、文档或生成稿。
只读采集，不修改模型和会话，不发起模型请求，不重启业务服务。

旧包现在就需要排障：
把 collect-diagnostics.sh、diagnostics.py、customer_logging.py 放到旧包 compose.yaml 同目录，执行：
bash collect-diagnostics.sh
脚本兼容 docker compose 和 docker-compose，无需先升级或重启。
''')
    run(['docker','save','--platform','linux/arm64','-o',str(output/'images.tar'),*tags])
    # Inspect the archive metadata rather than starting any business service.
    with tarfile.open(output/'images.tar') as archive:
        saved=json.load(archive.extractfile('manifest.json'))
        exported={tag for item in saved for tag in item.get('RepoTags',[])}
        assert set(tags)<=exported, 'Offline archive is missing release image tags'
    (output/'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(output).as_posix()}\n' for p in sorted(output.rglob('*')) if p.is_file() and p.name!='SHA256SUMS'))
    run(['sha256sum','-c','SHA256SUMS'],cwd=output)
    target=output.with_suffix('.tar.gz')
    run(['tar','-I','gzip -1','-cf',str(target),'-C',str(output.parent),output.name])
    target.with_name(target.name+'.sha256').write_text(sha(target)+'  '+target.name+'\n')
    # Small support tool archive for customers already using the old package.
    support=output.parent/'quickagent-customer-diagnostics-tools-20261113.tar.gz'
    with tarfile.open(support,'w:gz') as archive:
        for name in ['collect-diagnostics.sh','diagnostics.py','customer_logging.py']:
            archive.add(output/name,arcname=name)
    print(json.dumps({'package':str(target),'bytes':target.stat().st_size,'support_tools':str(support),'business_workflows_run':False},ensure_ascii=False),flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--base-archive',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    package(args.base_archive.resolve(),args.output.resolve())
