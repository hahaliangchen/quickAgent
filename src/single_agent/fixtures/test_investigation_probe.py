"""Small real test runner; records invocations so replay is observable."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import unittest

root = Path(__file__).resolve().parent
version = sys.argv[1]
if version not in ("baseline", "current"):
    raise SystemExit("usage: python3 probe.py baseline|current [trim empty]")
selected = sys.argv[2:] or ["trim", "empty"]
if not selected or any(name not in ("trim", "empty") for name in selected):
    raise SystemExit("unknown test selection")
output = root / "target"
output.mkdir(exist_ok=True)
ledger = Path("__TEST_RUN_LEDGER__")  # Bound by the fixture, including copied runners.
with ledger.open("a") as stream:
    stream.write(json.dumps({"version": version, "tests": selected, "workspace": str(root)}) + "\n")
spec = importlib.util.spec_from_file_location("codec", root / version / "codec.py")
codec = importlib.util.module_from_spec(spec)
spec.loader.exec_module(codec)


class Checks(unittest.TestCase):
    def test_trim(self):
        self.assertEqual(codec.normalize("  MiXeD  "), "mixed")

    def test_empty(self):
        self.assertEqual(codec.normalize("   "), "")


capture = io.StringIO()
suite = unittest.TestSuite(Checks("test_" + name) for name in selected)
with contextlib.redirect_stdout(capture), contextlib.redirect_stderr(capture):
    result = unittest.TextTestRunner(stream=capture, verbosity=2).run(suite)
report = capture.getvalue()
(output / (version + ".log")).write_text(report)
print(report, end="")
raise SystemExit(0 if result.wasSuccessful() else 1)
