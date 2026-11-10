"""Exercise the packaged Feature and its real lifecycle with Docker and the CLI."""

import json
import os
import shlex
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CLI = os.environ.get("DEVCONTAINER", "devcontainer")
SETUP = "/usr/local/share/over-dotfiles/setup.sh"
# Shell metacharacters must survive both build-time serialization and runtime use.
CUSTOM_HOME = "/home/vscode/dot files ' $(touch /tmp/over-option-executed)"


def command(*args, expected_success=True):
    result = subprocess.run(
        args,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
        timeout=300,
    )
    if (result.returncode == 0) != expected_success:
        print(result.stdout, flush=True)
        raise AssertionError(f"Unexpected exit {result.returncode}: {args}")
    return result.stdout


def scenario(name, options, assertion=None, error=None, prepare="", mounts=None):
    print(f"\n=== {name} ===", flush=True)
    with tempfile.TemporaryDirectory(prefix="over-feature-") as directory:
        workspace = Path(directory)
        # tempfile defaults to 0700, which blocks the non-root workspace user.
        workspace.chmod(0o755)
        config_dir = workspace / ".devcontainer"
        config_dir.mkdir()
        shutil.copytree(ROOT / "features/dotfiles", config_dir / "dotfiles")
        shutil.copytree(Path(__file__).parent / "source", config_dir / "source")
        source = config_dir / "source"
        command("git", "-C", str(source), "init", "--quiet")
        command("git", "-C", str(source), "add", ".")
        command(
            "git", "-C", str(source), "-c", "user.name=Feature test", "-c",
            "user.email=feature@example.invalid", "commit", "--quiet", "-m", "Fixture",
        )
        (config_dir / "Dockerfile").write_text(
            "FROM debian:bookworm-slim\n"
            "RUN useradd -m -s /bin/bash vscode\n"
            # libgit2 rejects local clone sources owned by another user.
            "COPY --chown=vscode:vscode source /opt/dotfiles\n"
        )
        # CLI 0.88.0 interpolates options into double-quoted shell assignments.
        # Escape that upstream boundary so the Feature receives the literal test
        # values; otherwise command substitutions execute before install.sh runs.
        injected_options = {
            key: value.replace("\\", "\\\\").replace('"', '\\"')
            .replace("$", "\\$").replace("`", "\\`")
            for key, value in options.items()
        }
        config = {
            "build": {"dockerfile": "Dockerfile"},
            "remoteUser": "vscode",
            "updateRemoteUserUID": False,
            "features": {"./dotfiles": injected_options},
        }
        # Feature hooks precede user hooks at the same stage; onCreate prepares
        # conflicts before the Feature's postCreateCommand can apply anything.
        if prepare:
            config["onCreateCommand"] = ["bash", "-euc", prepare]
        if mounts:
            config["mounts"] = mounts
        (config_dir / "devcontainer.json").write_text(json.dumps(config))
        try:
            output = command(
                CLI, "up", "--workspace-folder", directory,
                expected_success=error is None,
            )
            if error:
                assert error in output, f"Missing diagnostic: {error}\n{output}"
            if assertion:
                # exec also works after an expected lifecycle failure, allowing
                # direct checks that conflicting user data survived unchanged.
                command(
                    CLI, "exec", "--workspace-folder", directory,
                    "bash", "-euxc", assertion,
                )
        finally:
            containers = command(
                "docker", "ps", "-aq", "--filter",
                f"label=devcontainer.local_folder={directory}",
            ).split()
            if containers:
                command("docker", "rm", "-fv", *containers)
    print(f"PASS: {name}", flush=True)


def main():
    metadata = json.loads(
        (ROOT / "features/dotfiles/devcontainer-feature.json").read_text()
    )
    assert metadata["id"] == "dotfiles"
    assert metadata["options"]["overHome"]["default"] == "~/.dotfiles"
    assert os.access(ROOT / "features/dotfiles/install.sh", os.X_OK)
    scenario(
        "default installation is a non-root no-op", {}, """
        test "$(id -un)" = vscode
        over --version
        bash -n /usr/local/share/over-dotfiles/options.sh
        source /usr/local/share/over-dotfiles/options.sh
        test "$OVERHOME" = '~/.dotfiles'
        test ! -e "$HOME/.dotfiles"
        ! command -v crane
    """,
    )
    scenario(
        "cloning and repeated setup preserve deployed files and local edits",
        {"repository": "/opt/dotfiles", "overlay": "smoke"}, f"""
        source="$HOME/.dotfiles"
        test "$(cat "$HOME/feature-target/file")" = 'feature content'
        test "$(readlink "$HOME/feature-target/file")" = "$source/smoke/file"
        head=$(git -C "$source" rev-parse HEAD)
        # A new source commit makes an accidental implicit pull observable.
        git -C /opt/dotfiles -c user.name=Fixture -c user.email=fixture@example.invalid \\
            commit --allow-empty -m 'Upstream changed'
        {SETUP}
        test "$(git -C "$source" rev-parse HEAD)" = "$head"
        git -C "$source" remote set-url origin /unavailable-source
        printf 'local edit\\n' > "$source/local-edit"
        {SETUP}
        test "$(git -C "$source" rev-parse HEAD)" = "$head"
        test "$(git -C "$source" remote get-url origin)" = /unavailable-source
        test "$(cat "$source/local-edit")" = 'local edit'
        test "$(cat "$HOME/feature-target/file")" = 'feature content'
        test "$(readlink "$HOME/feature-target/file")" = "$source/smoke/file"
    """,
    )
    # An anonymous volume is writable by the lifecycle user and does not depend on
    # the host's UID matching the container's UID (including on ARM runners).
    scenario(
        "a mounted repository and shell-sensitive home remain literal",
        {"overHome": CUSTOM_HOME, "overlay": "smoke"}, f"""
        source /usr/local/share/over-dotfiles/options.sh
        test "$OVERHOME" = {shlex.quote(CUSTOM_HOME)}
        test "$(cat "$HOME/feature-target/file")" = 'feature content'
        test "$(readlink "$HOME/feature-target/file")" = "$OVERHOME/smoke/file"
        test ! -e /tmp/over-option-executed
        {SETUP}
    """,
        prepare="""
        source /usr/local/share/over-dotfiles/options.sh
        mkdir -p "$(dirname "$OVERHOME")"
        cp -R /opt/dotfiles "$OVERHOME"
    """,
        mounts=["type=volume,target=/home/vscode"],
    )
    for kind, prepare, assertion in [
        (
            "file", "printf 'user content' > \"$HOME/feature-target/file\"",
            ('test "$(cat "$HOME/feature-target/file")" = "user content"; '
             'test ! -L "$HOME/feature-target/file"'),
        ),
        (
            "directory",
            ('mkdir "$HOME/feature-target/file"; '
             'printf precious > "$HOME/feature-target/file/child"'),
            'test "$(cat "$HOME/feature-target/file/child")" = precious',
        ),
        (
            "foreign symlink",
            ('printf precious > "$HOME/foreign"; '
             'ln -s "$HOME/foreign" "$HOME/feature-target/file"'),
            ('test "$(readlink "$HOME/feature-target/file")" = "$HOME/foreign"; '
             'test "$(cat "$HOME/foreign")" = precious'),
        ),
    ]:
        scenario(
            f"a conflicting {kind} is preserved without prompting",
            {"repository": "/opt/dotfiles", "overlay": "smoke"}, assertion,
            error="Could not apply 'smoke'",
            prepare='mkdir -p "$HOME/feature-target"; ' + prepare,
        )
    scenario(
        "an invalid repository produces an initialization error",
        {"repository": "https://example.invalid/dotfiles.git", "overlay": "smoke"},
        error="Could not initialize",
    )
    scenario(
        "a missing mounted repository explains how to provide it",
        {"overlay": "smoke"}, error="supply 'repository' or mount",
    )
    scenario(
        "a missing overlay fails with its name",
        {"repository": "/opt/dotfiles", "overlay": "missing-overlay"},
        error="Could not apply 'missing-overlay'",
    )
    scenario(
        "shell syntax in overlay names is never executed",
        {
            "repository": "/opt/dotfiles",
            "overlay": 'bad"; touch /tmp/over-option-executed; #',
        },
        assertion="test ! -e /tmp/over-option-executed", error="Could not apply",
    )
    scenario(
        "an unavailable image version fails installation with context",
        {"version": "nonexistent-feature-smoke-test-version"},
        error="Failed to extract Over from ghcr.io/noirbizarre/over:nonexistent-feature-smoke-test-version",
    )


if __name__ == "__main__":
    main()
