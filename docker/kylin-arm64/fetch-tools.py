"""Prepare a source-free Docker context. Every downloaded tool has a SHA-256 pin."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import time
import urllib.request


def fetch(entry, cache):
    path = cache / entry["name"]
    def digest(file):
        with file.open("rb") as source:
            return hashlib.file_digest(source, "sha256").hexdigest()
    def matches():
        return path.is_file() and digest(path) == entry["sha256"]
    if not matches():
        partial = path.with_suffix(path.suffix + ".partial")
        print("Downloading " + entry["name"], flush=True)
        for attempt in range(3):
            try:
                with urllib.request.urlopen(entry["url"], timeout=180) as source, partial.open("wb") as output:
                    shutil.copyfileobj(source, output)
                if digest(partial) != entry["sha256"]:
                    raise RuntimeError("SHA-256 mismatch: " + entry["name"])
                partial.replace(path)
                break
            except Exception:
                if attempt == 2:
                    raise
                print("Retrying " + entry["name"], flush=True)
                time.sleep(1)
            finally:
                partial.unlink(missing_ok=True)
    return entry["name"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--context", type=Path, required=True)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--view", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    view = (args.view or args.server.parent / "ai-ppt-view").resolve()
    projects = {}
    for name, directory, expected in [
        ("ai-ppt-server", args.server.resolve(), "codex/ppt-remote-svg"),
        ("ai-ppt-view", view, "codex/ppt-host-agent"),
    ]:
        def git(*arguments):
            return subprocess.check_output(["git", "-C", str(directory), *arguments], text=True).strip()
        branch = git("branch", "--show-current")
        if branch != expected:
            raise SystemExit("%s 分支应为 %s，实际为 %s；请显式选择正确的项目目录" % (name, expected, branch or "detached HEAD"))
        projects[name] = {"branch": branch, "commit": git("rev-parse", "HEAD"),
                          "dirty": bool(git("status", "--porcelain"))}
    projects["ai-ppt-server"]["requirements_sha256"] = hashlib.sha256((args.server / "requirements.txt").read_bytes()).hexdigest()
    args.context.mkdir(parents=True, exist_ok=True)
    artifacts = args.context / "artifacts"
    artifacts.mkdir(exist_ok=True)
    entries = json.loads((root / "toolchain-lock.json").read_text())
    with ThreadPoolExecutor(max_workers=4) as workers:
        list(workers.map(lambda item: fetch(item, artifacts), entries))
    for source in root.iterdir():
        if source.is_file():
            shutil.copy2(source, args.context / source.name)
    shutil.copy2(args.server / "requirements.txt", args.context / "ppt-requirements.txt")
    (args.context / "project-sources.json").write_text(json.dumps(projects, indent=2) + "\n")
    (args.context / "artifacts.sha256").write_text("".join(
        item["sha256"] + "  artifacts/" + item["name"] + "\n" for item in entries))
    print("Prepared tool-only context: " + str(args.context), flush=True)
