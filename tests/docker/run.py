"""Exercise the packaged CLI with real immutable and persistent writable mounts."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile


def run(*args, success=True):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, check=False, timeout=120)
    assert (result.returncode == 0) == success, f"{args}:\n{result.stdout}"
    return result.stdout


def inventory(root):
    return {str(path.relative_to(root)): (path.stat().st_mode,
            path.read_bytes() if path.is_file() else None)
            for path in [root, *root.rglob("*")]}


def main():
    image = sys.argv[1] if len(sys.argv) > 1 else "over:local"
    print("Checking Docker default command...", flush=True)
    assert "Usage:" in run("docker", "run", "--rm", image)
    with tempfile.TemporaryDirectory(prefix="over-image-") as directory:
        root = Path(directory)
        root.chmod(0o755)
        source, home, state = [root / name for name in ["source", "home", "state"]]
        for path in [source / "app", home, state]:
            path.mkdir(parents=True)
        (source / "over.yaml").write_text("host_overlay: app\n")
        (source / "app/over.yaml").write_text(
            "mode: checkout\ndata: {name: container}\nrules:\n"
            "  - {path: '*.jinja', template: true, strip_extension: true}\n")
        (source / "app/config.jinja").write_text("hello={{ data.name }}")
        (source / "app/plain").write_text("protected content")
        run("git", "-C", str(source), "init", "--quiet")
        # Snapshots must observe Over's writes, not asynchronous Git housekeeping.
        run("git", "-C", str(source), "config", "maintenance.auto", "false")
        run("git", "-C", str(source), "config", "gc.auto", "0")

        def container(*args, readonly=False, entrypoint=None, success=True):
            source_mount = f"type=bind,source={source},target=/dotfiles"
            if readonly:
                source_mount += ",readonly"
            options = ["docker", "run", "--rm", "--user", f"{os.getuid()}:{os.getgid()}",
                       "--workdir", "/dotfiles",
                       "--env", "HOME=/home/over", "--env", "XDG_STATE_HOME=/state",
                       "--mount", source_mount,
                       "--mount", f"type=bind,source={home},target=/home/over",
                       "--mount", f"type=bind,source={state},target=/state"]
            if entrypoint:
                options += ["--entrypoint", entrypoint]
            return run(*options, image, *args, success=success)

        def git(*args):
            return container("-C", "/dotfiles", "-c", "user.name=Image tests", "-c",
                             "user.email=image@example.invalid", "-c", "commit.gpgsign=false",
                             *args, entrypoint="git")

        print("Preparing an unlocked source checkout...", flush=True)
        git("add", ".")
        git("commit", "-qm", "fixture")
        git("config", "user.name", "Image tests")
        git("config", "user.email", "image@example.invalid")
        container("--home", "/dotfiles", "user", "add", "owner", "--identity-output", "/home/over/identity.age")
        container("--home", "/dotfiles", "crypt", "add", "app/plain")
        git("add", ".")
        git("commit", "-qm", "protected source")
        git("remote", "add", "origin", "/unavailable-remote")
        (source / ".git/over-crypt/operation.lock").unlink()
        before = inventory(source)
        print("Checking genuine read-only source consumption...", flush=True)
        for _ in range(2):
            container("--home", "/dotfiles", "apply", "--read-only", "--no-prompt", "app", readonly=True)
            assert (home / "config").read_text() == "hello=container"
            assert (home / "plain").read_text() == "protected content"
            assert not (home / "config").is_symlink()
            assert inventory(source) == before
            assert not list(state.iterdir()), "Ephemeral apply persisted operational state"
        print("Checking locked read-only diagnostics...", flush=True)
        container("--home", "/dotfiles", "lock")
        locked = inventory(source)
        error = container("--home", "/dotfiles", "apply", "--read-only", "--no-prompt", "app",
                          readonly=True, success=False)
        assert "unlock/repair it on the host" in error
        assert inventory(source) == locked
        container("--home", "/dotfiles", "unlock", "--identity", "/home/over/identity.age")
        print("Checking persistent writable development...", flush=True)
        # Fresh targets distinguish normal initialization from unmanaged ephemeral outputs.
        container("--home", "/dotfiles", "apply", "--no-prompt", "--target", "/home/over/managed", "app")
        association = state / "over/virtual_checkout.toml"
        before_state = association.read_bytes()
        # Every helper call recreates the container, preserving all three storage mounts.
        container("--home", "/dotfiles", "apply", "--no-prompt", "--target", "/home/over/managed", "app")
        assert association.read_bytes() == before_state
        container("-c", "printf 'intentional edit' > /dotfiles/app/plain", entrypoint="/bin/sh")
        git("add", "app/plain")
        git("commit", "-qm", "intentional development commit")
        assert "intentional development commit" in git("log", "-1", "--format=%s")
    print("PASS: Docker read-only consumption, lock/unlock, and persistent writable development")


if __name__ == "__main__":
    main()
