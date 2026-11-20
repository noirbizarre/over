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


def inventory(root):
    # Include administrative files/objects: git status alone misses persistent traces.
    return {str(path.relative_to(root)): (path.stat().st_mode,
            path.read_bytes() if path.is_file() else None)
            for path in [root, *root.rglob("*")]}


def scenario(name, options, assertion, prepare="", mounts=None, error=None,
             bind_source=False, recreated_assertion=None):
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
        # Background Git housekeeping would race administrative byte-for-byte snapshots.
        command("git", "-C", str(source), "config", "maintenance.auto", "false")
        command("git", "-C", str(source), "config", "gc.auto", "0")
        command("git", "-C", str(source), "add", ".")
        command("git", "-C", str(source), "-c", "user.name=Feature test", "-c",
                "user.email=feature@example.invalid", "-c", "core.hooksPath=/dev/null",
                "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "Fixture")
        (config_dir / "Dockerfile").write_text(
            "FROM debian:bookworm-slim\nRUN useradd -m -s /bin/bash vscode "
            "&& install -d -o vscode -g vscode /home/vscode/.local/state\n"
            # libgit2 must see a clone source owned by the lifecycle user.
            "COPY --chown=vscode:vscode source /opt/dotfiles\n"
        )
        # CLI 0.88 interpolates options into shell assignments before install.sh.
        # Escape that upstream boundary to test the literal received option values.
        options = {key: (value.replace("\\", "\\\\").replace('"', '\\"')
                   .replace("$", "\\$").replace("`", "\\`") if isinstance(value, str) else value)
                   for key, value in options.items()}
        config = {"build": {"dockerfile": "Dockerfile"}, "remoteUser": "vscode",
                  "updateRemoteUserUID": bind_source, "features": {"./over": options},
                  "containerEnv": {"XDG_STATE_HOME": "/home/vscode/.local/state"}}
        if prepare:
            config["onCreateCommand"] = ["bash", "-euc", prepare]
        if mounts:
            config["mounts"] = [mount.replace("SOURCE_PATH", str(source)).replace("VOLUME_ID", workspace.name)
                                for mount in mounts]
        (config_dir / "devcontainer.json").write_text(json.dumps(config))
        before = inventory(source)
        try:
            output = command(CLI, "up", "--workspace-folder", directory, success=error is None)
            if error:
                assert error in output, output
            command(CLI, "exec", "--workspace-folder", directory, "bash", "-euc", assertion)
            if bind_source and options.get("readOnly"):
                assert inventory(source) == before, "Read-only setup changed the host source"
            if recreated_assertion:
                containers = command("docker", "ps", "-aq", "--filter",
                                     f"label=devcontainer.local_folder={directory}").split()
                command("docker", "rm", "-fv", *containers)
                command(CLI, "up", "--workspace-folder", directory)
                command(CLI, "exec", "--workspace-folder", directory, "bash", "-euc", recreated_assertion)
        finally:
            containers = command("docker", "ps", "-aq", "--filter",
                                 f"label=devcontainer.local_folder={directory}").split()
            if containers:
                command("docker", "rm", "-fv", *containers)
            volumes = command("docker", "volume", "ls", "-q", "--filter", f"name={workspace.name}").split()
            if volumes:
                command("docker", "volume", "rm", *volumes)
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
    scenario("read-only host bind with ephemeral targets",
             {"overHome": "/mounted/dotfiles", "readOnly": True, "overlay": "smoke"}, f"""
        test "$(id -un)" = vscode
        test "$(cat "$HOME/feature-target/file")" = 'feature content'
        test ! -e "$XDG_STATE_HOME/over"
        ! touch /mounted/dotfiles/forbidden
        {SETUP}
        test ! -e "$XDG_STATE_HOME/over"
    """, bind_source=True,
             mounts=["type=bind,source=SOURCE_PATH,target=/mounted/dotfiles,readonly"])
    scenario("writable source with persistent home and state",
             {"overHome": "/home/vscode/.dotfiles", "overlay": "smoke"}, """
        over --home "$HOME/.dotfiles" apply persistent --no-prompt
        test -f "$XDG_STATE_HOME/over/virtual_checkout.toml"
        sha256sum "$XDG_STATE_HOME/over/virtual_checkout.toml" > "$HOME/state-before"
        printf 'container edit\\n' > "$HOME/.dotfiles/from-container"
        git -C "$HOME/.dotfiles" add from-container
        git -C "$HOME/.dotfiles" -c user.name=Container -c user.email=container@example.invalid commit -qm 'container edit'
        git -C "$HOME/.dotfiles" rev-parse HEAD > "$HOME/commit-before"
    """, bind_source=True, prepare="""
        mkdir -p "$XDG_STATE_HOME"
        test -w "$XDG_STATE_HOME"
    """, mounts=[
        "type=volume,source=over-home-VOLUME_ID,target=/home/vscode",
        "type=bind,source=SOURCE_PATH,target=/home/vscode/.dotfiles",
        "type=volume,source=over-state-VOLUME_ID,target=/home/vscode/.local/state",
    ], recreated_assertion="""
        test "$(cat "$HOME/.dotfiles/from-container")" = 'container edit'
        test "$(git -C "$HOME/.dotfiles" rev-parse HEAD)" = "$(cat "$HOME/commit-before")"
        sha256sum -c "$HOME/state-before"
        test "$(cat "$HOME/persistent-target/file")" = 'persistent content'
        over --home "$HOME/.dotfiles" apply persistent --no-prompt
    """)


if __name__ == "__main__":
    main()
