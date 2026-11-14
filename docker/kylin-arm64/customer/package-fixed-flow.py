#!/usr/bin/env python3
"""Package the contract/fixed-workflow repair without model or PPT generation."""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

HERE=Path(__file__).resolve().parent
VERSION='20261113-fixed-flow'
BASE='20261113-logs'


def run(args,**kwargs):return subprocess.run(args,check=True,**kwargs)
def sha(path):
    h=hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda:stream.read(8*1024*1024),b''):h.update(block)
    return h.hexdigest()
def inspect(tag):return json.loads(subprocess.check_output(['docker','image','inspect','--platform','linux/arm64',tag]))[0]


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--work',type=Path,required=True)
    parser.add_argument('--base-archive',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args();work=args.work.resolve();output=args.output.resolve()
    manifest=None
    with tarfile.open(args.base_archive,'r|gz') as archive:
        for member in archive:
            if member.name.endswith('/manifest.json') and member.size<2*1024*1024:
                manifest=json.load(archive.extractfile(member));break
    if manifest is None:raise RuntimeError('Base manifest missing')
    base_app='quickagent-customer-ppt-arm64:'+BASE
    assert inspect(base_app)['Id']==manifest['release_images'][base_app]['id']
    skill=work/'source/server/resource/ai_skills/ppt-master'
    for name in ['layout-fill.bundle.json','svg-generate.bundle.json']:
        bundle=json.loads((skill/'programs'/name).read_text())
        for item in [bundle['source'],*bundle['modules'].values()]:
            path=skill/item['path'].removeprefix('skill:')
            assert sha(path)==item['sha256'],str(path)
        main_source=(skill/bundle['source']['path'].removeprefix('skill:')).read_text()
        assert 'require("ppt")' in main_source and 'require("ppt_master")' not in main_source
    spec=importlib.util.spec_from_file_location('assemble',HERE/'assemble.py');module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    compiled=[module.verify_elf(work/'bin'/name) for name in ['quickagent','ai-ppt-server']]
    output.mkdir(parents=True,exist_ok=False)
    for name in ['compose.yaml','deploy.sh','collect-diagnostics.sh','diagnostics.py','customer_logging.py']:
        shutil.copy2(HERE/name,output/name)
    for name in ['deploy.sh','collect-diagnostics.sh']:(output/name).chmod(0o755)
    app='quickagent-customer-ppt-arm64:'+VERSION
    renderer='quickagent-customer-ppt-renderer-arm64:'+VERSION
    with tempfile.TemporaryDirectory(prefix='customer-fixed-flow-') as temporary:
        context=Path(temporary)
        shutil.copytree(work/'bin',context/'bin')
        for name in ['programs','references','workflows']:shutil.copytree(skill/name,context/'skill'/name)
        for name in ['SKILL.md','skill_events.json']:shutil.copy2(skill/name,context/'skill'/name)
        for name in ['entrypoint.py','customer_logging.py']:shutil.copy2(HERE/name,context/name)
        (context/'Dockerfile').write_text('FROM '+base_app+'\nCOPY bin/ /opt/customer/bin/\nCOPY skill/ /opt/customer/server/resource/ai_skills/ppt-master/\nCOPY entrypoint.py customer_logging.py /opt/customer/bin/\nENV PPT_BUNDLED_SKILL_DIR=/opt/customer/server/resource/ai_skills/ppt-master\n')
        run(['docker','build','--platform','linux/arm64','-t',app,str(context)])
    run(['docker','tag','quickagent-customer-ppt-renderer-arm64:'+BASE,renderer])
    # Inspect only the installed files and their digests, without starting services.
    check="import json,hashlib,os;from pathlib import Path;p=Path(os.environ['PPT_BUNDLED_SKILL_DIR']);[(None if hashlib.sha256((p/a['path'].removeprefix('skill:')).read_bytes()).hexdigest()==a['sha256'] else (_ for _ in ()).throw(AssertionError(a['path']))) for n in ['layout-fill.bundle.json','svg-generate.bundle.json'] for m in [json.loads((p/'programs'/n).read_text())] for a in [m['source'],*m['modules'].values()]];print('Installed fixed program resources and hashes: OK')"
    checked=subprocess.check_output(['docker','run','--rm','--platform','linux/arm64','--network','none','--entrypoint','python',app,'-c',check],text=True)
    evidence=output/'verification';evidence.mkdir()
    reports={'go-contracts-resources-policy.txt':'/tmp/ppt-fixed-program-final-go-checks-4.log','rust-fixed-programs.txt':'/tmp/ppt-fixed-program-rust-checks.log','rust-bundle-linking.txt':'/tmp/ppt-actual-bundle-check.log','rust-contracts-policy-transport.txt':'/tmp/ppt-program-transport-contract-checks.log','rust-policy-recovery.txt':'/tmp/ppt-fixed-program-final-rust-checks.log'}
    for name,path in reports.items():shutil.copy2(path,evidence/name)
    (evidence/'installed-resource-check.txt').write_text(checked)
    tags=[app,renderer,'quickagent-customer-mysql-arm64:8.0','quickagent-customer-redis-arm64:7']
    manifest['base_release']=args.base_archive.name
    manifest['release_variant']='fixed-ppt-programs-and-contracts'
    manifest['release_images']={tag:{'id':inspect(tag)['Id'],'architecture':inspect(tag)['Architecture']} for tag in tags}
    assert all(v['architecture']=='arm64' for v in manifest['release_images'].values())
    replacements={record['file']:record for record in compiled}
    manifest['binaries']=[replacements.get(record['file'],record) for record in manifest['binaries']]
    manifest['fixed_program_update']={'built_at':datetime.now(timezone.utc).isoformat(),'sources':json.loads((work/'sources.json').read_text()),'immutable_skill_root':'/opt/customer/server/resource/ai_skills/ppt-master','customer_templates_and_projects_preserved':True,'model_or_ppt_generation_tested':False,'checks':'contracts; stale mounted resources; bundle preflight; authorization; zero model calls on failed installation; actual bundle offline compile/link; installed asset hashes','recipes':{name:sha(HERE/name) for name in ['entrypoint.py','package-fixed-flow.py']}}
    (output/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n')
    (output/'README-部署.txt').write_text('''麒麟 ARM64 客户包：固定编排修复版（2026-11-13）

修复：
1. list_templates 明确声明返回 data.templates 数组。
2. 内置程序、模块、说明及执行元数据从本次镜像读取，不被旧 state 目录遮住。
3. 固定编排统一使用 ppt 库，预检资源及摘要；资源/编译/绑定失败即停止，不回退到模型自写主编排。
4. 已确认但尚未完成的整套制作阶段拒绝临时替代主程序；页面内容作者及已交付稿的局部编辑能力保留。
5. 保留宿主机 logs/customer.log 和 ./deploy.sh diagnostics。

验证范围：接口契约、旧数据目录模拟、编排授权、安装失败时零模型请求、真实固定脚本离线编译/绑定、ARM64 安装资源摘要。
没有调用真实模型、没有执行整套 PPT 生成或之前的浏览器业务测试流程。

首次安装：./deploy.sh load → ./deploy.sh init → ./deploy.sh up。
访问 http://服务器地址:8899；用户名 admin，初始密码见 .env 的 CUSTOMER_ADMIN_PASSWORD。
Agent 设置页默认 http://服务器地址:3001/agent/settings，密码同 .env。
出厂模型/密钥为空；客户已有模型设置保存在 state 中，升级不替换。

升级已有客户部署：
1. 保留并备份原 .env、state/ 以及数据库卷。不要重新执行 init，不删除任何数据卷。
2. 在原部署目录用旧 compose 停止 app：docker compose stop app（或 docker-compose stop app）。
3. 加载本包镜像：docker load -i 新包目录/images.tar。
4. 将新包的 compose.yaml、deploy.sh、collect-diagnostics.sh、diagnostics.py、customer_logging.py 复制到原部署目录。
5. 保持原 .env、COMPOSE_PROJECT_NAME、state/ 和数据库卷，执行 ./deploy.sh up。
   若使用 docker-compose 独立命令，则执行 docker-compose up -d app。
6. 刷新页面，重新从大纲确认入口开始固定制作。历史失败会话和已有稿保留。

本次不重建前端和渲染器；它们沿用上一版，业务修复位于 Agent、PPT 后端及内置流程资源。
日志在宿主机 logs/customer.log；故障执行 bash collect-diagnostics.sh，将生成的诊断包发回。
''')
    run(['docker','save','--platform','linux/arm64','-o',str(output/'images.tar'),*tags])
    (output/'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(output).as_posix()}\n' for p in sorted(output.rglob('*')) if p.is_file() and p.name!='SHA256SUMS'))
    run(['sha256sum','-c','SHA256SUMS'],cwd=output)
    archive=output.with_suffix('.tar.gz');run(['tar','-I','gzip -1','-cf',str(archive),'-C',str(output.parent),output.name])
    archive.with_name(archive.name+'.sha256').write_text(sha(archive)+'  '+archive.name+'\n')
    print(json.dumps({'archive':str(archive),'bytes':archive.stat().st_size,'business_generation_run':False},ensure_ascii=False),flush=True)


if __name__=='__main__':main()
