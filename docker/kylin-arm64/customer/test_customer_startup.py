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


if __name__ == "__main__":
    unittest.main()
