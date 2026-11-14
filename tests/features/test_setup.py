"""Check the shell option boundary without downloading images or running Docker."""

import json
import os
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class SetupTests(unittest.TestCase):
    def run_setup(self, selection):
        with tempfile.TemporaryDirectory(prefix="over-setup-") as directory:
            root = Path(directory)
            shutil.copyfile(
                Path(__file__).resolve().parents[2] / "devcontainer/setup.sh",
                root / "setup.sh",
            )
            (root / "options.sh").write_text(
                f"OVERLAY={shlex.quote(selection)}\n"
                f"OVERHOME={shlex.quote(directory)}\nREPOSITORY=source\n"
            )
            # Record argument boundaries, not a reconstructed shell command: spaces,
            # globs, newlines and substitutions must remain data in each array element.
            (root / "over").write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "with open(os.environ['CALL_LOG'], 'a') as log:\n"
                "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            )
            (root / "over").chmod(0o755)
            log = root / "calls"
            result = subprocess.run(
                ["bash", str(root / "setup.sh")],
                env={
                    **os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                    "CALL_LOG": str(log),
                },
                text=True, capture_output=True, check=False,
            )
            calls = (
                [json.loads(line) for line in log.read_text().splitlines()]
                if log.exists() else []
            )
            # Remove the temporary source path while retaining every CLI argument.
            return result, [call[2:] for call in calls]

    def test_one_and_multiple_names_use_one_quoted_apply_invocation(self):
        for value, expected in [
            ("base", ["base"]),
            (" base , tools/rust ,base ", ["base", "tools/rust", "base"]),
            ("a name,glob*,$(exit 42);'", ["a name", "glob*", "$(exit 42);'"]),
            ("first\nsecond,last", ["first\nsecond", "last"]),
        ]:
            with self.subTest(value=value):
                result, calls = self.run_setup(value)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(calls, [
                    ["init", "--", "source"],
                    ["apply", "--no-prompt", "--", *expected],
                ])

    def test_blank_elements_fail_before_any_cli_side_effect(self):
        for value, position in [
            (" ", 1), (",base", 1), ("base,", 2),
            ("base,,rust", 2), ("base, \t,rust", 2),
        ]:
            with self.subTest(value=value):
                result, calls = self.run_setup(value)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(
                    f"Invalid 'overlay' option: empty name at position {position}",
                    result.stderr,
                )
                self.assertEqual(calls, [])

    def test_an_empty_option_initializes_without_applying_defaults(self):
        result, calls = self.run_setup("")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [["init", "--", "source"]])


@unittest.skipUnless(
    os.environ.get("OVER_FEATURE_TEST_BINARY"),
    "set OVER_FEATURE_TEST_BINARY for local lifecycle tests",
)
class LocalLifecycleTests(unittest.TestCase):
    def test_real_cli_composes_the_feature_fixtures_and_rejects_invalid_selections(self):
        binary = Path(os.environ["OVER_FEATURE_TEST_BINARY"]).resolve(strict=True)
        for selection, trace, error in [
            ("smoke", None, None),
            ("", None, None),
            ("left,right", "shared\nleft\nright\n", None),
            (" right , left ,right ", "shared\nright\nleft\n", None),
            ("left,missing-overlay", None, "missing-overlay"),
            ("smoke,collision", None, "Could not apply"),
            ("collision,smoke", None, "Could not apply"),
        ]:
            with (
                self.subTest(selection=selection),
                tempfile.TemporaryDirectory(prefix="over-lifecycle-") as directory,
            ):
                root = Path(directory)
                fixture = Path(__file__).resolve().parent
                shutil.copytree(fixture / "source", root / "source")
                shutil.copyfile(
                    fixture.parents[1] / "devcontainer/setup.sh", root / "setup.sh",
                )
                (root / "bin").mkdir()
                (root / "bin/over").symlink_to(binary)
                (root / "options.sh").write_text(
                    f"OVERLAY={shlex.quote(selection)}\n"
                    f"OVERHOME={shlex.quote(str(root / '.dotfiles'))}\n"
                    f"REPOSITORY={shlex.quote(str(root / 'source'))}\n"
                )
                for args in [
                    ["init", "--quiet"],
                    ["add", "."],
                    [
                        "-c", "user.name=Feature test",
                        "-c", "user.email=feature@example.invalid",
                        # Fixture commits must not depend on personal signing/hooks.
                        "-c", "core.hooksPath=/dev/null",
                        "-c", "commit.gpgsign=false",
                        "commit", "--quiet", "-m", "Fixture",
                    ],
                ]:
                    subprocess.run(
                        ["git", "-C", str(root / "source"), *args],
                        check=True, capture_output=True, timeout=20,
                    )
                # Keep fixture targets and state out of the developer's home.
                env = {
                    **os.environ, "HOME": directory,
                    "XDG_STATE_HOME": str(root / "state"),
                    "XDG_CACHE_HOME": str(root / "cache"),
                    "PATH": f"{root / 'bin'}:{os.environ['PATH']}",
                }
                result = subprocess.run(
                    ["bash", str(root / "setup.sh")], env=env, text=True,
                    capture_output=True, check=False, timeout=30,
                )
                if error:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(error, result.stderr)
                    self.assertFalse((root / "feature-target/file").exists())
                    self.assertFalse((root / "feature-left/left").exists())
                else:
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    if selection == "smoke":
                        self.assertTrue((root / "feature-target/file").is_symlink())
                    elif not selection:
                        self.assertFalse((root / "feature-target/file").exists())
                    else:
                        for name in ["shared", "left", "right"]:
                            self.assertTrue(
                                (root / f"feature-{name}/{name}").is_symlink()
                            )
                if trace:
                    self.assertEqual((root / "feature-trace").read_text(), trace)
                else:
                    self.assertFalse((root / "feature-trace").exists())


if __name__ == "__main__":
    unittest.main()
