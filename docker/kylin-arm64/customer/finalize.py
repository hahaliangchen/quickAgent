#!/usr/bin/env python3
"""Export only the tested ARM64 images and seal the offline deployment archive."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def sha(path):
    digest=hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda:stream.read(8*1024*1024),b''): digest.update(block)
    return digest.hexdigest()


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    output=args.output.resolve()
    report=json.loads((output/'verification.json').read_text())
    manifest=json.loads((output/'manifest.json').read_text())
    assert report['passed'] and report['ui']['browser_login'] and report['render']['chinese_png']
    for tag,expected in report['images'].items():
        current=json.loads(subprocess.check_output(['docker','image','inspect','--platform','linux/arm64',tag]))[0]
        assert current['Id']==expected and current['Architecture']=='arm64', 'Images changed after verification: '+tag
        assert manifest['release_images'][tag]['id']==expected
    recipe=Path(__file__).resolve().parent
    manifest['verification_recipe_sha256']={name:sha(recipe/name) for name in ['smoke.py','ui-smoke.cjs','renderer-smoke.mts']}
    manifest['finalization_recipe_sha256']=sha(Path(__file__).resolve())
    (output/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n')
    forbidden=[p for p in output.rglob('*') if p.name=='.env' or p.name=='state' or p.name=='credentials.yaml']
    assert not forbidden, 'Private runtime data must not be included'
    subprocess.run(['docker','save','--platform','linux/arm64','-o',str(output/'images.tar'),*report['images']],check=True)
    # Verify the actual saved archive can be loaded by Docker, rather than only
    # trusting image metadata from before serialization.
    subprocess.run(['docker','load','-i',str(output/'images.tar')],check=True)
    (output/'SHA256SUMS').write_text(''.join(f'{sha(path)}  {path.relative_to(output).as_posix()}\n' for path in sorted(output.rglob('*')) if path.is_file() and path.name!='SHA256SUMS'))
    subprocess.run(['sha256sum','-c','SHA256SUMS'],cwd=output,check=True)
    archive=output.with_suffix('.tar.gz')
    subprocess.run(['tar','-I','gzip -1','-cf',str(archive),'-C',str(output.parent),output.name],check=True)
    checksum=sha(archive)
    archive.with_name(archive.name+'.sha256').write_text(f'{checksum}  {archive.name}\n')
    print(json.dumps({'archive':str(archive),'bytes':archive.stat().st_size,'sha256':checksum},ensure_ascii=False),flush=True)


if __name__=='__main__': main()
