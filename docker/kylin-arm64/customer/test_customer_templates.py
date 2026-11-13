import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("customer_templates", Path(__file__).with_name("customer_templates.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class TemplatePolicyTest(unittest.TestCase):
    def test_removes_branded_files_and_all_indexes_preserving_other_templates(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            rows = {"china_unicom": {"label": "中国联通"}, "opaque": {"label": "通用"}, "business": {"label": "商务"}}
            for key in rows:
                folder = root / "pptx_templates" / key
                folder.mkdir(parents=True)
                with zipfile.ZipFile(folder / "template.pptx", "w") as archive:
                    archive.writestr("ppt/slides/slide1.xml", "联通品牌" if key == "opaque" else "通用正文")
            for relative in ["pptx_templates/templates_index.json", "templates/layouts/layouts_index.json"]:
                p = root / relative
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(json.dumps({"meta": {"total": 3}, "layouts": rows, "categories": {"all": {"layouts": list(rows)}}}))
            (root / "templates/layouts/china_unicom.svg").write_text("brand")
            p = root / "pptx_templates/conversion_manifest.json"
            p.write_text(json.dumps({"templates": [{"template": key, "pages": 2} for key in rows]}))
            self.assertEqual(module.prune_unicom_templates(root), ["china_unicom", "opaque"])
            self.assertEqual([p.name for p in (root / "pptx_templates").iterdir() if p.is_dir()], ["business"])
            self.assertFalse((root / "templates/layouts/china_unicom.svg").exists())
            for relative in ["pptx_templates/templates_index.json", "templates/layouts/layouts_index.json"]:
                d = json.loads((root / relative).read_text())
                self.assertEqual(list(d["layouts"]), ["business"])
                self.assertEqual(d["meta"]["total"], 1)
                self.assertEqual(d["categories"]["all"]["layouts"], ["business"])
            self.assertEqual(json.loads(p.read_text())["template_count"], 1)
            self.assertEqual(module.prune_unicom_templates(root), [])

    def test_cleans_unindexed_legacy_layouts_without_touching_other_resources(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            layouts = root / "templates/layouts"
            for name in ["中国联通_旧版", "business"]:
                (layouts / name).mkdir(parents=True)
                (layouts / name / "01_cover.svg").write_text("layout")
            (layouts / "unicom_old.svg").write_text("brand")
            (root / "customer-project.pptx").write_text("user work")
            self.assertEqual(module.prune_unicom_templates(root), ["unicom_old", "中国联通_旧版"])
            self.assertTrue((layouts / "business/01_cover.svg").exists())
            self.assertTrue((root / "customer-project.pptx").exists())
            self.assertFalse((layouts / "unicom_old.svg").exists())

    def test_rejects_traversal_before_deleting_templates(self):
        for key in ["..", "../outside", "nested/template", "nested\\template"]:
            with self.subTest(key=key), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                catalog = root / "pptx_templates"
                (catalog / "unicom").mkdir(parents=True)
                (catalog / "templates_index.json").write_text(json.dumps({"layouts": {key: {"label": "联通"}}}))
                with self.assertRaises(ValueError):
                    module.prune_unicom_templates(root)
                self.assertTrue((catalog / "unicom").is_dir())


if __name__ == "__main__":
    unittest.main()
