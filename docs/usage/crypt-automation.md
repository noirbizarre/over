# Lock/unlock automation recipes

Over provides `over lock`, `over unlock`, and the Git filter. You choose when to
invoke them. These opt-in, user-managed examples add no session policy or daemon
to Over; adapt and test them for your desktop and repository.

Read the [explicit lock lifecycle](crypt.md#transparent-git-and-explicit-lock)
first. The local key persists across terminal closure and reboot until explicit
lock. Lock affects linked worktrees sharing that key and the native secret store;
it cannot erase deployed destination files, application copies, or backups.

## Prepare the repository

- Configure user access and test manual lock/unlock before installing hooks.
  Use a disposable repository with non-secret content for initial testing.
- Replace all example executable, repository, and identity paths. Each example
  selects one repository explicitly with `--home`; repeat/adapt for independent
  repositories, but do not invoke transitions twice for linked worktrees sharing
  a common key.
- Fresh unattended unlock needs an available noninteractive identity. The examples
  use an unencrypted software identity **outside** the repository. Keep its
  directory private and file permissions restrictive. Encrypted identities,
  passphrase keys, and hardware plugins may need terminal/device interaction;
  use manual terminal unlock if credentials cannot be acquired unattended.
- An identity readable by the same OS user also permits that user to unlock
  again. Automatic re-unlock does not isolate secrets from same-user processes.
  Choose whether that trade-off fits your session policy.
- Never place private key bytes, passphrases, tokens, or secret values in command
  arguments, units, configuration, notifications, or logs. Only identity **paths**
  appear here. `OVER_SECRET` is a legacy migration credential, not an unlock method.
  Do not enable shell tracing or log decrypted files/secret-command output.

Lock can refuse dirty or untracked protected plaintext, policy conflicts, or
inaccessible linked worktrees. Failure does **not** mean the repository is locked.
Inspect and resolve the reported state; never automatically discard edits or
disable required filters. Event callbacks are best effort, not atomic enforcement
of OS screen locking. If unlock fails, inspect the error and retry in a terminal:

```sh
over --home /home/alice/.dotfiles unlock --identity /home/alice/.config/age/over-alice.txt
```

## Linux: systemd user services with Sway

This example needs a running systemd user manager, Sway, swaylock, swayidle,
util-linux `flock`, `logger`, and `notify-send` with a notification daemon.
It uses a wrapper around the locker as the concrete lock/unlock integration.
**systemd alone has no universal desktop screen lock/unlock event API.**

### User services

Create `~/.config/systemd/user/over-lock.service`:

```ini
[Unit]
Description=Lock the Over repository

[Service]
Type=oneshot
WorkingDirectory=/home/alice/.dotfiles
ExecStart=/home/alice/.cargo/bin/over --home /home/alice/.dotfiles lock
StandardInput=null
StandardOutput=null
StandardError=journal
TimeoutStartSec=30s
```

Create `~/.config/systemd/user/over-unlock.service`:

```ini
[Unit]
Description=Unlock the Over repository

[Service]
Type=oneshot
WorkingDirectory=/home/alice/.dotfiles
ExecStart=/home/alice/.cargo/bin/over --home /home/alice/.dotfiles unlock --identity /home/alice/.config/age/over-alice.txt
StandardInput=null
StandardOutput=null
StandardError=journal
TimeoutStartSec=30s
```

These [oneshot services](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html)
finish after each command; leave `RemainAfterExit` unset so subsequent starts run
again. `systemctl --user start` waits for completion and returns failure if the
command fails. Adjust the timeout to your repository's size; a timeout is a
failure requiring inspection, not evidence that lock completed. No `enable` or
`[Install]` section is needed: the wrapper starts these services on demand.

User services do not inherit your interactive shell setup. Both executable and
repository selection are explicit. Ensure Git and any required identity helper
are available in the user manager's `PATH`; do not import credential environment
variables wholesale. The software identity here needs no GUI prompt. The wrapper
runs inside Sway, where `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and the session bus
are available to swaylock, systemctl, and notifications.

Load and test the services manually, in that order:

```sh
systemctl --user daemon-reload
systemctl --user start over-lock.service
systemctl --user start over-unlock.service
systemctl --user status over-lock.service over-unlock.service
journalctl --user -u over-lock.service -u over-unlock.service
```

A successful oneshot normally shows `inactive (dead)`. Check its exit result,
not whether it remains active. Errors go to the journal; the commands do not
print secret values, but diagnostics can include repository paths. Inspect logs
locally before sharing them.

### Sway event integration

Save this as `~/.local/bin/over-screen-lock` and make it executable:

```sh
#!/bin/sh
set -u

report() {
    logger -t over-screen-lock -- "$1"
    notify-send 'Over session integration' "$1" ||
        logger -t over-screen-lock 'Desktop notification failed; inspect the journal'
}

# Keep the guard across the whole screen-lock cycle to prevent overlapping wrappers.
exec 9>"${XDG_RUNTIME_DIR:?}/over-screen-lock.guard"
if ! flock -n 9; then
    exit 1
fi

result=0
if ! systemctl --user start over-lock.service; then
    report 'Repository lock failed; inspect over-lock.service after screen unlock'
    result=1
fi

# A repository refusal must not prevent the OS screen from locking.
# Use foreground swaylock: --daemonize (including in its config) returns too early.
if swaylock; then
    if ! systemctl --user start over-unlock.service; then
        report 'Repository unlock failed; retry over unlock in a terminal'
        result=1
    fi
else
    report 'Screen locker failed; repository unlock was not requested'
    result=1
fi
exit "$result"
```

The guard's contention exit means another wrapper is already responsible for
this cycle; other guard/setup failures need investigation. Ensure the tools above
are on Sway's `PATH`. Do not put `daemonize` in swaylock's config. A normal
foreground [swaylock](https://github.com/swaywm/swaylock/blob/master/swaylock.1.scd)
exit is used as the unlock trigger; its explicit `SIGUSR1` unlock also exits, so
this is not an independent proof of authentication. Do not signal it to automate
authentication bypass. If you use a different locker, use its documented
successful-unlock callback instead.

Add these lines to your Sway config, adapting existing lock/idle bindings rather
than starting a second idle manager:

```text
bindsym $mod+Shift+l exec /home/alice/.local/bin/over-screen-lock
exec swayidle -w timeout 300 '/home/alice/.local/bin/over-screen-lock'
```

The keybinding and five-minute idle timeout both lock the repository before
starting the screen locker, then request repository unlock after the locker
exits successfully. `swayidle -w` waits for the wrapper; it does not process other
idle events while this foreground lock cycle is running. This is deliberately
not an idle `resume` hook: activity alone does not establish screen unlock.

Only entry points routed through this wrapper are covered. Calling swaylock
directly, suspend, logout, compositor crashes, and desktop-specific lock actions
need separate handling. This wrapper waits for authentication and is unsuitable
as a blocking `before-sleep` hook. It also delays starting the screen locker
while the repository lock service runs; select a suitable timeout and test it.

For other desktops, attach the services to their documented lock/unlock hooks or
a user-managed session-event listener. In particular, swayidle's logind `lock`
and `unlock` events require corresponding session signals and logind support;
a lock **request** is not proof that a screen locker finished locking, and not
every locker emits an unlock signal. Screen-off, wake, idle resume, and generic
session targets are not substitutes for authenticated unlock. See the
[swayidle event reference](https://github.com/swaywm/swayidle/blob/master/swayidle.1.scd).

To remove this integration, remove the Sway bindings/idle command, restore your
usual screen-lock command, remove the wrapper and units, and run
`systemctl --user daemon-reload`. Removing automation does not change the current
repository key state; use explicit `over lock` if desired.

## macOS: Hammerspoon screen events

[Hammerspoon](https://www.hammerspoon.org/) provides a user-managed event source;
a LaunchAgent by itself does not provide a universal screen-lock event trigger.
Install Hammerspoon, complete its documented setup/permission prompts, and enable
its launch-at-login preference if desired. It must be running in your GUI session
to receive events. Test event delivery on your macOS/Hammerspoon versions.

Add this to `~/.hammerspoon/init.lua`, replacing all three paths. Use a software
identity that can unlock noninteractively, or omit automatic unlock and use a
terminal instead. GUI applications have a different environment from your shell;
ensure any required helpers are available in Hammerspoon's task environment.

```lua
local executable = "/Users/alice/.cargo/bin/over"
local repository = "/Users/alice/.dotfiles"
local identity = "/Users/alice/.config/age/over-alice.txt"

-- Retain the watcher and active task so garbage collection cannot stop callbacks.
overSession = { pending = nil, task = nil }
local state = overSession

local function report(action)
    hs.notify.new({title = "Over", informativeText =
        action .. " failed; inspect the repository in a terminal"}):send()
end

local runPending
runPending = function()
    if state.task or not state.pending then return end
    local action = state.pending
    state.pending = nil
    local arguments = {"--home", repository, action}
    if action == "unlock" then
        table.insert(arguments, "--identity")
        table.insert(arguments, identity)
    end
    state.task = hs.task.new(executable, function(code)
        -- Report status only; never forward subprocess payloads into GUI logs.
        state.task = nil
        if code ~= 0 then report(action) end
        runPending()
    end, arguments)
    if not state.task then
        report(action)
    elseif not state.task:start() then
        state.task = nil
        report(action)
    end
end

state.watcher = hs.caffeinate.watcher.new(function(event)
    if event == hs.caffeinate.watcher.screensDidLock then
        state.pending = "lock"
    elseif event == hs.caffeinate.watcher.screensDidUnlock then
        state.pending = "unlock"
    else
        return
    end
    -- Serialize transitions and coalesce queued events to the latest screen state.
    runPending()
end):start()
```

Reload Hammerspoon's configuration while no transition is running, then test a
screen lock/unlock cycle. The
[screen watcher](https://www.hammerspoon.org/docs/hs.caffeinate.watcher.html)
uses screen lock/unlock events rather than display sleep/wake; the
[asynchronous task](https://www.hammerspoon.org/docs/hs.task.html) avoids blocking
the GUI and reports launch/nonzero-exit failures. Transitions take time; wait for
command completion before assuming success. A new lock event during an in-flight
unlock can only run afterward, so this remains best-effort automation.

On a failure notification, use manual commands in a terminal to inspect and
resolve the error. Notifications do not claim that a failed lock removed the key.
To disable, remove the snippet and reload Hammerspoon once no task is running;
then explicitly lock or unlock the repository as needed. Neither installation
nor reload applies an initial repository transition: events drive this example.
