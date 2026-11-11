import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("customer_profile", Path(__file__).with_name("prepare-ppt-customer-profile.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CustomerProfileTests(unittest.TestCase):
    def fixture(self):
        return [{"id": "llm-pi-ai", "config": {"providers": {
            "deepseek": {"baseURL": "https://api.deepseek.com/v1", "apiKey": "do-not-copy-corporate-key",
                         "models": [{"id": "deepseek-flash", "name": "Flash"}, {"id": "deepseek-v4-pro"}]},
            "deepseek-self": {"baseURL": "https://private.invalid", "apiKey": "personal-key", "models": [{"id": "private-model"}]},
            "another": {"baseURL": "https://another.invalid", "models": [{"id": "another-model"}]},
        }}}, {"id": "agent-default-model", "config": {"provider": "deepseek-self", "model": "private-model"}}]

    def test_only_non_personal_models_and_references_survive(self):
        source = self.fixture()
        before = json.dumps(source)
        generation = {"visual_tool": {"provider": "another", "model": "another-model"},
                      "context_agent": {"model": "private-model"},
                      "plugin_models": {"ppt-master": [{"provider": "deepseek-self", "model": "private-model"}]},
                      "model_capabilities": {"deepseek": {"deepseek-flash": {"tool_calling": True, "image_probe": {"raw": "do-not-copy"}}}}}
        providers, settings = module.project_profile(source, generation)
        encoded = json.dumps([providers, settings])
        for value in ["deepseek-self", "private-model", "another-model", "personal-key", "do-not-copy", "private.invalid"]:
            self.assertNotIn(value, encoded)
        self.assertEqual(set(providers[0]["config"]["providers"]), {"deepseek"})
        self.assertEqual(settings["visual_tool"]["provider"], "deepseek")
        self.assertEqual(json.dumps(source), before)

    def test_no_fallback_to_personal_account(self):
        source = self.fixture()
        del source[0]["config"]["providers"]["deepseek"]
        with self.assertRaises(ValueError):
            module.project_profile(source, {})

    def test_export_does_not_read_credentials_or_overwrite_existing_profile(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            (source / "providers.yml").write_text(json.dumps(self.fixture()))
            (source / "credentials.yaml").write_text("not valid yaml: [personal-key")
            output = root / "customer"
            module.write_profile(source, output)
            self.assertEqual(json.loads((output / "credentials.yaml").read_text())["records"], {})
            with self.assertRaises(FileExistsError):
                module.write_profile(source, output)


if __name__ == "__main__":
    unittest.main()
