"""Install the staged Feature payload and exercise its lifecycle as a normal user."""

import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
FEATURE = ROOT / "devcontainer"
CLI = os.environ.get("DEVCONTAINER", "devcontainer")
SETUP = "/usr/local/share/over/setup.sh"


def command(*args, success=True):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, check=False, timeout=600)
    if (result.returncode == 0) != success:
        raise AssertionError(f"Unexpected exit {result.returncode}: {args}\n{result.stdout}")
    return result.stdout


def scenario(name, options, assertion, prepare="", mounts=None, error=None):
    print(f"Checking {name}...", flush=True)
    with tempfile.TemporaryDirectory(prefix="over-feature-") as directory:
        workspace = Path(directory)
        # The lifecycle user must be able to traverse the mounted workspace.
        workspace.chmod(0o755)
        config_dir = workspace / ".devcontainer"
        config_dir.mkdir()
        shutil.copytree(FEATURE, config_dir / "over")
        source = config_dir / "source"
        shutil.copytree(Path(__file__).parent / "source", source)
        command("git", "-C", str(source), "init", "--quiet")
        command("git", "-C", str(source), "add", ".")
        command("git", "-C", str(source), "-c", "user.name=Feature test", "-c",
                "user.email=feature@example.invalid", "-c", "core.hooksPath=/dev/null",
                "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "Fixture")
        (config_dir / "Dockerfile").write_text(
            "FROM debian:bookworm-slim\nRUN useradd -m -s /bin/bash vscode\n"
            # libgit2 must see a clone source owned by the lifecycle user.
            "COPY --chown=vscode:vscode source /opt/dotfiles\n"
        )
        # CLI 0.88 interpolates options into shell assignments before install.sh.
        # Escape that upstream boundary to test the literal received option values.
        options = {key: value.replace("\\", "\\\\").replace('"', '\\"')
                   .replace("$", "\\$").replace("`", "\\`") for key, value in options.items()}
        config = {"build": {"dockerfile": "Dockerfile"}, "remoteUser": "vscode",
                  "updateRemoteUserUID": False, "features": {"./over": options}}
        if prepare:
            config["onCreateCommand"] = ["bash", "-euc", prepare]
        if mounts:
            config["mounts"] = mounts
        (config_dir / "devcontainer.json").write_text(json.dumps(config))
        try:
            output = command(CLI, "up", "--workspace-folder", directory, success=error is None)
            if error:
                assert error in output, output
            command(CLI, "exec", "--workspace-folder", directory, "bash", "-euc", assertion)
        finally:
            containers = command("docker", "ps", "-aq", "--filter",
                                 f"label=devcontainer.local_folder={directory}").split()
            if containers:
                command("docker", "rm", "-fv", *containers)
    print(f"PASS: {name}", flush=True)


def main():
    metadata = json.loads((FEATURE / "devcontainer-feature.json").read_text())
    package = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    assert metadata["version"] == package["version"]
    assert "version" not in metadata["options"]
    assert metadata["options"]["overHome"]["default"] == ""
    for arch in ["amd64", "arm64"]:
        assert (FEATURE / "bin" / arch / "over").is_file(), f"Stage the {arch} build before testing"
    arch = {"x86_64": "amd64", "aarch64": "arm64"}[platform.machine()]
    checksum = hashlib.sha256((FEATURE / "bin" / arch / "over").read_bytes()).hexdigest()
    scenario("bundled installation and empty lifecycle options", {}, f"""
        test "$(id -un)" = vscode
        test "$(over --version)" = {shlex.quote('over ' + metadata['version'])}
        test "$(sha256sum /usr/local/bin/over | cut -d ' ' -f1)" = {checksum}
        test ! -e "$HOME/.dotfiles"
        git --version
        test -s /etc/ssl/certs/ca-certificates.crt
        ! command -v crane
    """)
    scenario("clone and composed application with repeatable setup",
             {"repository": "/opt/dotfiles", "overlay": " right , left ,right "}, f"""
        test "$(tr '\\n' ',' < "$HOME/feature-trace")" = 'shared,right,left,'
        test -L "$HOME/feature-left/left"
        test -L "$HOME/feature-right/right"
        test ! -e "$HOME/feature-target/file"
        head=$(git -C "$HOME/.dotfiles" rev-parse HEAD)
        git -C "$HOME/.dotfiles" remote set-url origin /unavailable-source
        printf 'local edit\\n' > "$HOME/.dotfiles/local-edit"
        {SETUP}
        test "$(git -C "$HOME/.dotfiles" rev-parse HEAD)" = "$head"
        test "$(cat "$HOME/.dotfiles/local-edit")" = 'local edit'
        test "$(git -C "$HOME/.dotfiles" remote get-url origin)" = /unavailable-source
    """)
    custom_home = "/home/vscode/dot files ' $(touch /tmp/over-option-executed)"
    scenario("mounted source and literal custom home",
             {"overHome": custom_home, "overlay": "smoke"}, f"""
        source /usr/local/share/over/options.sh
        test "$OVERHOME" = {shlex.quote(custom_home)}
        test "$(cat "$HOME/feature-target/file")" = 'feature content'
        test "$(readlink "$HOME/feature-target/file")" = "$OVERHOME/smoke/file"
        test ! -e /tmp/over-option-executed
        {SETUP}
    """, prepare="""
        source /usr/local/share/over/options.sh
        mkdir -p "$(dirname "$OVERHOME")"
        cp -R /opt/dotfiles "$OVERHOME"
    """, mounts=["type=volume,target=/home/vscode"])
    scenario("CLI conflicts preserve user content", {"repository": "/opt/dotfiles", "overlay": "smoke"}, """
        test "$(cat "$HOME/feature-target/file")" = 'user content'
        test ! -L "$HOME/feature-target/file"
    """, prepare="""
        mkdir -p "$HOME/feature-target"
        printf 'user content' > "$HOME/feature-target/file"
    """, error="feature-target/file")


if __name__ == "__main__":
    main()
