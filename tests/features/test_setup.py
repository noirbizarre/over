"""Verify the Feature's option adapter, not the CLI's repository policies."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest


class SetupTests(unittest.TestCase):
    def run_setup(self, selection="", repository="source", home="", failure=""):
        with tempfile.TemporaryDirectory(prefix="over-setup-") as directory:
            root = Path(directory)
            shutil.copyfile(Path(__file__).resolve().parents[2] / "devcontainer/setup.sh", root / "setup.sh")
            (root / "options.sh").write_text(
                f"OVERLAY={shlex.quote(selection)}\nREPOSITORY={shlex.quote(repository)}\n"
                f"OVERHOME={shlex.quote(home)}\n"
            )
            # JSON records boundaries and effective home without reinterpreting shell data.
            (root / "over").write_text(
                f"#!{sys.executable}\nimport json, os, sys\n"
                "with open(os.environ['CALL_LOG'], 'a') as log:\n"
                "    log.write(json.dumps([sys.argv[1:], os.environ.get('OVER_HOME')]) + '\\n')\n"
                "if sys.argv[1] == os.environ['FAIL_COMMAND']: sys.exit(37)\n"
            )
            (root / "over").chmod(0o755)
            log = root / "calls"
            result = subprocess.run(
                ["bash", str(root / "setup.sh")], text=True, capture_output=True,
                env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                     # Personal shell startup must not replace the controlled home.
                     "BASH_ENV": "/dev/null", "CALL_LOG": str(log),
                     "FAIL_COMMAND": failure, "OVER_HOME": "ambient-home"},
                check=False,
            )
            calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            return result, calls

    def test_names_are_passed_in_one_literal_apply_invocation(self):
        for value, expected in [
            ("base", ["base"]),
            (" base , tools/rust ,base ", ["base", "tools/rust", "base"]),
            ("a name,glob*,$(exit 42);'", ["a name", "glob*", "$(exit 42);'"]),
            ("first\nsecond,last", ["first\nsecond", "last"]),
        ]:
            with self.subTest(value=value):
                result, calls = self.run_setup(value)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([call[0] for call in calls], [
                    ["init", "--", "source"], ["apply", "--no-prompt", "--", *expected],
                ])

    def test_blank_elements_fail_before_initialization(self):
        for value, position in [(" ", 1), (",base", 1), ("base,", 2), ("base,,rust", 2)]:
            with self.subTest(value=value):
                result, calls = self.run_setup(value)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"empty name at position {position}", result.stderr)
                self.assertEqual(calls, [])

    def test_empty_options_do_not_invoke_the_cli(self):
        result, calls = self.run_setup(repository="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [])

    def test_an_empty_overlay_initializes_without_inferring_an_apply_selection(self):
        result, calls = self.run_setup()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [[["init", "--", "source"], "ambient-home"]])

    def test_explicit_homes_override_normal_cli_resolution(self):
        for home, expected in [("~/custom", f"{os.environ['HOME']}/custom"), ("/custom ' $home", "/custom ' $home")]:
            with self.subTest(home=home):
                result, calls = self.run_setup(home=home)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls[0][1], expected)

    def test_cli_failure_status_is_propagated_without_running_later_commands(self):
        result, calls = self.run_setup("base", failure="init")
        self.assertEqual(result.returncode, 37)
        self.assertEqual(len(calls), 1)
        result, _ = self.run_setup("base", repository="", failure="apply")
        self.assertEqual(result.returncode, 37)


if __name__ == "__main__":
    unittest.main()
