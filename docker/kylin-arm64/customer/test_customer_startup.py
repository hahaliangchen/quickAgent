import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("customer_entrypoint", HERE / "entrypoint.py")
entrypoint = importlib.util.module_from_spec(spec)
spec.loader.exec_module(entrypoint)


class CustomerStartupTests(unittest.TestCase):
    def test_missing_ppt_registration_is_repaired_without_replacing_preferences(self):
        with tempfile.TemporaryDirectory() as temporary:
            profile = Path(temporary)
            entrypoint.ensure_ppt_registration(profile)
            path = profile / "skill-packages.json"
            self.assertEqual(json.loads(path.read_text())["remotes"]["ppt-master"]["token"], "")
            for config in [
                {"remotes": {"ppt-master": {"url": "http://custom/plugin", "token": "customer"}}, "enabled": {"ppt-master": False}},
                {"removed": ["ppt-master"], "remotes": {}},
            ]:
                path.write_text(json.dumps(config))
                before = path.read_bytes()
                entrypoint.ensure_ppt_registration(profile)
                self.assertEqual(path.read_bytes(), before)
            path.write_text('{"remotes":{"other":{"url":"http://other","token":"keep"}},"enabled":{"ppt-master":false}}')
            entrypoint.ensure_ppt_registration(profile)
            saved = json.loads(path.read_text())
            self.assertEqual(saved["remotes"]["other"]["token"], "keep")
            self.assertFalse(saved["enabled"]["ppt-master"])

    def test_legacy_factory_restrictions_upgrade_once_and_custom_settings_survive(self):
        for customized in [False, True]:
            with self.subTest(customized=customized), tempfile.TemporaryDirectory() as temporary:
                profile = Path(temporary)
                (profile / "customer-policy.json").write_text('{"provider":"deepseek"}')
                allowed = ([{"provider": "qwen-local", "model": "customer-qwen3-vl-32b"}] if customized else
                           [{"provider": "deepseek", "model": model} for model in ["deepseek-flash", "deepseek-v4-pro"]])
                visual = ({"provider": "qwen-local", "model": "customer-qwen3-vl-32b"} if customized else
                          {"provider": "deepseek", "model": "deepseek-flash", "reasoning_effort": "high"})
                path = profile / "agent-generation.yml"
                initial = {"plugin_models": {"ppt-master": allowed}, "visual_tool": visual, "unrelated": "keep"}
                path.write_text(json.dumps(initial))
                entrypoint.upgrade_customer_defaults(profile)
                saved = json.loads(path.read_text())
                self.assertEqual(saved, initial if customized else {"plugin_models": {}, "visual_tool": {}, "unrelated": "keep"})
                # After migration, an intentionally chosen restriction is respected.
                path.write_text(json.dumps(initial))
                before = path.read_bytes()
                entrypoint.upgrade_customer_defaults(profile)
                self.assertEqual(path.read_bytes(), before)

    def test_keyless_customer_profile_survives_startup_and_restart(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app, state = root / "app", root / "state"
            (app / "server/resource").mkdir(parents=True)
            profile = app / "config/agent"
            profile.mkdir(parents=True)
            providers = [{"id": "llm-pi-ai", "config": {"providers": {
                "qwen-local": {"baseURL": "http://model:8000/v1", "api": "openai-completions",
                               "models": [{"id": "customer-qwen3-vl-32b"}]}}}}]
            (profile / "providers.yml").write_text(json.dumps(providers))
            (profile / "agent-generation.yml").write_text('{"web_search_provider": null}')
            with patch.object(entrypoint, "APP", app), patch.object(entrypoint, "STATE", state), patch.dict(
                os.environ, {"JWT_SIGNING_KEY": "test-signing-key", "MYSQL_PASSWORD": "test-password", "DEEPSEEK_API_KEY": ""}
            ):
                entrypoint.write_config()
                saved = state / "agent-config/providers.yml"
                saved.write_text(saved.read_text() + "\n# Customer editing must survive restart\n")
                before = {p.name: p.read_bytes() for p in saved.parent.iterdir()}
                entrypoint.write_config()
                self.assertEqual({p.name: p.read_bytes() for p in saved.parent.iterdir()}, before)
                self.assertFalse((saved.parent / "credentials.yaml").exists())
                self.assertEqual(os.environ["AGENT_BIND"], "127.0.0.1:3001")

    def test_deploy_up_does_not_require_deepseek_key(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shutil.copy2(HERE / "deploy.sh", root / "deploy.sh")
            (root / ".env").write_text("DEEPSEEK_API_KEY=\n")
            fake_bin = root / "bin"
            fake_bin.mkdir()
            docker = fake_bin / "docker"
            docker.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$DOCKER_TEST_ARGS"\n')
            docker.chmod(0o755)
            arguments = root / "docker-args"
            subprocess.run(["bash", str(root / "deploy.sh"), "up"], check=True,
                           env={**os.environ, "PATH": str(fake_bin) + os.pathsep + os.environ["PATH"],
                                "DOCKER_TEST_ARGS": str(arguments)})
            self.assertEqual(arguments.read_text().splitlines(),
                             ["compose", "up", "-d", "--wait", "--wait-timeout", "480", "app"])
            self.assertTrue((root / "models.yaml").is_file())

    def test_sync_models_file_applies_once_per_edit_and_skips_empty_default(self):
        import io
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            profile = root / "agent-config"
            profile.mkdir()
            models_file = root / "models.yaml"
            models_file.write_text("providers: []\n")
            calls = []

            def fake_open(req, timeout=30):
                calls.append((req.get_method(), json.loads(req.data.decode()) if req.data else None))
                if req.get_method() == "GET":
                    body = {"revision": "rev-1", "providers": [], "orchestrator_provider": None, "orchestrator_model": None}
                else:
                    body = {"revision": "rev-2", "providers": [{"id": "qwen-local"}],
                            "orchestrator_provider": "qwen-local", "orchestrator_model": "qwen3-vl-32b"}
                return io.BytesIO(json.dumps(body).encode())

            with patch.object(entrypoint.HTTP, "open", side_effect=fake_open):
                self.assertFalse(entrypoint.sync_models_file(profile, models_file))
                self.assertEqual(calls, [])

                models_file.write_text(
                    "providers:\n"
                    "  - id: qwen-local\n"
                    "    url: http://10.10.10.201:12891/v1\n"
                    "    api_key: ''\n"
                    "    models:\n"
                    "      - qwen3-vl-32b\n"
                    "ppt_models:\n"
                    "  - qwen-local/qwen3-vl-32b\n"
                )
                self.assertTrue(entrypoint.sync_models_file(profile, models_file))
                self.assertEqual([c[0] for c in calls], ["GET", "PUT"])
                put_payload = calls[1][1]
                self.assertEqual(put_payload["orchestrator_provider"], "qwen-local")
                self.assertEqual(put_payload["orchestrator_model"], "qwen3-vl-32b")
                self.assertTrue(put_payload["providers"][0]["clear_api_key"])
                self.assertEqual(put_payload["plugin_models"], {"ppt-master": [{"provider": "qwen-local", "model": "qwen3-vl-32b"}]})

                # Unchanged file is not re-applied on restart.
                self.assertFalse(entrypoint.sync_models_file(profile, models_file))
                self.assertEqual(len(calls), 2)

                # Editing models.yaml re-syncs automatically.
                models_file.write_text(
                    "providers:\n"
                    "  - id: subapi\n"
                    "    url: http://104.238.154.29:58081/v1\n"
                    "    api_key: sk-test-key\n"
                    "    models:\n"
                    "      - gpt-6.1-terra\n"
                )
                self.assertTrue(entrypoint.sync_models_file(profile, models_file))
                self.assertEqual(len(calls), 4)
                self.assertEqual(calls[3][1]["providers"][0]["api_key"], "sk-test-key")
                self.assertEqual(calls[3][1]["orchestrator_model"], "gpt-6.1-terra")


if __name__ == "__main__":
    unittest.main()
