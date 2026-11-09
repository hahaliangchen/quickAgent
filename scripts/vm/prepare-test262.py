"""Fetch a hash-checked test262 archive into a cache outside the checkout."""
import argparse,hashlib,json,os,pathlib,tarfile,urllib.request

repo=pathlib.Path(__file__).resolve().parents[2]
def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache-dir',type=pathlib.Path,default=pathlib.Path(os.environ.get('LOCALAPPDATA',pathlib.Path.home()/'.cache'))/'GrowVmTests/test262')
    args=parser.parse_args();cache=args.cache_dir.resolve()
    if cache==repo or repo in cache.parents:raise SystemExit('test262 must stay outside the repository')
    pin=json.loads((repo/'tests/vm/test262-selection.json').read_text(encoding='utf-8'))
    revision=pin['revision'];cache.mkdir(parents=True,exist_ok=True)
    archive=cache/(revision+'.tar.gz');target=cache/revision
    if not archive.is_file():urllib.request.urlretrieve('https://codeload.github.com/tc39/test262/tar.gz/'+revision,archive)
    digest=hashlib.sha256(archive.read_bytes()).hexdigest()
    if digest!=pin['archive_sha256']:raise SystemExit('Archive SHA256 does not match the pinned selection; nothing extracted')
    if not target.exists():
        with tarfile.open(archive) as source:source.extractall(cache,filter='data')
        (cache/('test262-'+revision)).rename(target)
    marker=target/'.vm-test262-pin.json'
    expected={k:pin[k] for k in ['revision','archive_sha256']}
    if marker.exists() and json.loads(marker.read_text(encoding='utf-8'))!=expected:raise SystemExit('Existing cache pin differs')
    marker.write_text(json.dumps(expected,indent=2)+'\n',encoding='utf-8')
    print(target)

if __name__=='__main__':main()
