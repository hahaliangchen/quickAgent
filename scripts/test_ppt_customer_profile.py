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

    def test_all_models_providers_and_model_references_are_removed(self):
        source = self.fixture()
        before = json.dumps(source)
        generation = {"visual_tool": {"provider": "another", "model": "another-model"},
                      "context_agent": {"model": "private-model"},
                      "plugin_models": {"ppt-master": [{"provider": "deepseek-self", "model": "private-model"}]},
                      "model_capabilities": {"deepseek": {"deepseek-flash": {"tool_calling": True, "image_probe": {"raw": "do-not-copy"}}}}}
        providers, settings = module.project_profile(source, generation)
        encoded = json.dumps([providers, settings])
        for value in ["deepseek", "private-model", "another-model", "personal-key", "do-not-copy", "private.invalid"]:
            self.assertNotIn(value, encoded)
        self.assertEqual(providers, [{"id":"llm-pi-ai","config":{"providers":{}}}])
        self.assertEqual(settings["visual_tool"], {})
        self.assertIsNone(settings["web_search_provider"])
        self.assertEqual(settings["plugin_models"], {})
        self.assertEqual(settings["model_capabilities"], {})
        self.assertEqual(settings["plugin_generation"], {})
        self.assertIsNone(settings["reasoning_effort"])
        self.assertEqual(json.dumps(source), before)

    def test_an_empty_source_is_valid_without_any_fallback(self):
        source = self.fixture()
        del source[0]["config"]["providers"]["deepseek"]
        self.assertEqual(module.project_profile(source, {})[0][0]["config"]["providers"], {})
        self.assertEqual(module.project_profile([], {})[0][0]["config"]["providers"], {})

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
            self.assertTrue(json.loads((output / "customer-policy.json").read_text())["customer_custom_models"])
            self.assertEqual(json.loads((output / "customer-policy.json").read_text())["models"], [])
            with self.assertRaises(FileExistsError):
                module.write_profile(source, output)


if __name__ == "__main__":
    unittest.main()
