"""Remove China Unicom-branded bundled templates, including their index entries."""
import json
from pathlib import Path
import re
import shutil
import zipfile

BRAND = re.compile(r"联通|unicom", re.I)


def prune_unicom_templates(skill_root):
    skill_root = Path(skill_root)
    catalog = skill_root / "pptx_templates"
    layouts = skill_root / "templates/layouts"
    excluded = set()
    indexes = [catalog / "templates_index.json", skill_root / "templates/layouts/layouts_index.json"]
    for path in indexes:
        if path.is_file():
            data = json.loads(path.read_text())
            for key, row in data.get("layouts", {}).items():
                if BRAND.search(key + " " + json.dumps(row, ensure_ascii=False)):
                    excluded.add(key)
    if layouts.is_dir():
        excluded.update(path.stem if path.is_file() else path.name
                        for path in layouts.iterdir() if BRAND.search(path.name))
    if catalog.is_dir():
        for directory in catalog.iterdir():
            if not directory.is_dir():
                continue
            if BRAND.search(directory.name):
                excluded.add(directory.name)
            deck = directory / "template.pptx"
            if deck.is_file():
                with zipfile.ZipFile(deck) as archive:
                    if any(BRAND.search(archive.read(name).decode(errors="ignore")) for name in archive.namelist() if name.endswith(".xml")):
                        excluded.add(directory.name)
    # Validate every identity before deleting anything, including dot segments.
    for key in excluded:
        if key in {"", ".", ".."} or Path(key).name != key or "\\" in key:
            raise ValueError("Invalid template identity")
    for key in excluded:
        for folder in [catalog / key, layouts / key]:
            if folder.is_symlink():
                folder.unlink()
            elif folder.is_dir():
                shutil.rmtree(folder)
        for suffix in [".svg", ".md", ".png", ".pptx"]:
            (skill_root / "templates/layouts" / (key + suffix)).unlink(missing_ok=True)

    def clean(value):
        if isinstance(value, dict):
            return {key: clean(row) for key, row in value.items()
                    if key not in excluded and not (isinstance(row, dict) and row.get("template") in excluded)}
        if isinstance(value, list):
            return [clean(row) for row in value if not (isinstance(row, str) and row in excluded)
                    and not (isinstance(row, dict) and row.get("template") in excluded)]
        return value

    for path in indexes + [catalog / "conversion_manifest.json"]:
        if not path.is_file():
            continue
        data = clean(json.loads(path.read_text()))
        if isinstance(data.get("layouts"), dict) and isinstance(data.get("meta"), dict) and "total" in data["meta"]:
            data["meta"]["total"] = len(data["layouts"])
        if isinstance(data.get("templates"), list):
            data["template_count"] = len(data["templates"])
            data["page_count"] = sum(row.get("pages", 0) for row in data["templates"])
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n")
    return sorted(excluded)
