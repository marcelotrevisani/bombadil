# Gotchas

Things that surprise people, and what to do about them.

## Changing the Python version deletes the environment

`uv sync -p 3.12` on an environment built with 3.13 doesn't upgrade it in
place. It deletes the environment and builds a new one. Anything you
installed by hand with `pip install`, rather than through `pyproject.toml`,
is lost.

Bombadil compares the pin with the version recorded in the environment's
`pyvenv.cfg` and asks before a sync that would rebuild. Versions are compared
segment by segment, so `3.12` and `3.12.13` are not a conflict and don't ask.

To try a new Python without losing the current environment,
[add a second environment](main-window.md#environments) with the new pin
instead of changing the existing one.

## Recreate can't be undone

**Recreate venv** deletes the environment before rebuilding it. If the
rebuild fails (no network, a lock file that no longer resolves, a private
index without credentials), you are left with no environment until the
problem is fixed and you sync again.

## PyCharm shows the wrong interpreter

Bombadil sets the interpreter by editing the project's `.idea` folder and
PyCharm's SDK list (`jdk.table.xml`) **before** PyCharm opens. If PyCharm is
already running:

- It holds the SDK list in memory and writes it back when it exits, which
  undoes any edit made in the meantime. So while PyCharm is running, Bombadil
  only reuses an existing SDK entry that already points at the environment.
  Otherwise it tells you which interpreter to pick by hand.
- PyCharm can re-import `pyproject.toml` and pin modules to a different
  interpreter.

To fix it, close the project in PyCharm, then use **Open in PyCharm** from
Bombadil.

Also:

- A **symlinked `.idea` folder** or `misc.xml` is refused, so Bombadil never
  writes through a link to somewhere else.
- Only `.iml` module files directly inside `.idea` are updated.
- Environment variables reach PyCharm only when **Bombadil starts it**. A
  PyCharm that was already open keeps the environment it was started with.

## A terminal opens without the environment

On macOS, Terminal.app and iTerm2 are started through `open`, which doesn't
pass environment variables on. They open in the project folder, but the
environment isn't active. The same is true of WSL on Windows. Choose a
terminal marked as fully supported in
[Settings › Terminal](settings.md#terminal), or run
`source .venv/bin/activate` yourself. On Windows, PowerShell, Command Prompt
and Git Bash receive the environment, but a startup file that changes `PATH`
can put another Python first.

## Secrets need a keyring on Linux

Secret variables and index credentials are stored through the Secret Service
API. GNOME and KDE provide one. On a minimal window manager, or over SSH,
there may be none, and saving a secret fails. Install and start
`gnome-keyring` (or KWallet, or KeePassXC with Secret Service enabled).

## The window shows a generic icon on Linux

GNOME on Wayland ignores the icon a window carries. It looks the icon up
through an installed `.desktop` file instead. Bombadil writes that file to
`~/.local/share/applications` each time it starts. If you still see a
placeholder, log out and back in once so GNOME rereads its application list.

## There's no tray icon on GNOME

Stock GNOME has no system tray. Install the *AppIndicator and
KStatusNotifierItem Support* extension. Without a tray, closing the window
quits Bombadil instead of hiding it, because there would be no way to bring
the window back.

## The uv in use isn't the one you expected

With **auto**, Bombadil uses uv from `PATH` only if it is version 0.12.1 or
newer. Otherwise it uses its bundled copy without saying so. If you need one specific
uv, pick **from PATH** or a binary in [Settings › uv](settings.md#uv).
Explicit choices fail loudly instead of falling back.

## Moving a project changes its central environment

With a **central folder** as the default location, each project's
environment folder is named after the project folder plus a hash of its full
path. Move or rename the project folder and Bombadil looks for a new
environment folder. Sync creates it, and the old one stays where it was.
Delete it yourself, or keep that project's environment **alongside** it.

## Only one index can replace PyPI

uv takes a single default index. If a project selects two indexes set to
**replaces PyPI**, the last one wins. Set the others to **supplements PyPI**.

## Variables already in your shell still apply

Bombadil starts from the environment it was launched with. A
`UV_INDEX_<NAME>_PASSWORD` or `UV_DEFAULT_INDEX` exported in the shell that
started Bombadil reaches every command it runs, unless Bombadil sets the same
variable itself.

## Pre-activate scripts run in `sh`, not your shell

On Linux and macOS, scripts are sourced by `/bin/sh`. Bash-only syntax, such
as `[[ ]]`, arrays or `source`, fails there. Write POSIX shell, or have the
script call `bash` for the parts that need it. An `exit` inside a script is
treated as a failure, not as "nothing to add".

## Members you untick are removed on sync

**Sync members** installs exactly the selected members, and removes members
you have unticked from the environment. Bombadil lists what will be removed
and asks first.

## No prebuilt wheel for Intel Macs

`pip install bombadil` has no wheel for macOS on Intel. Use
`cargo install bombadil`, which builds from source.

## Editing the config file while the app runs

Bombadil writes the whole file each time a setting changes, which would
overwrite your edits. Quit Bombadil (including from the tray) before editing
`config.toml`.

## A program on the Run tab waits forever

The Run tab can't send keystrokes to a program. One that asks a question, such
as `input()`, waits for an answer that never comes. Press **Stop**, then use
**Open terminal** to run it where you can type.

## A run doesn't install new dependencies

The Run tab never syncs (it runs `uv run --no-sync`), so a package you just
added to `pyproject.toml` isn't there until you press **Sync**. This keeps a
run from ever rebuilding the environment on another Python.

## The vulnerability check needs the internet

Bombadil asks osv.dev about installed packages. Offline, or behind a proxy
that blocks it, the check is skipped and the Dependencies tab says
"Couldn't check for vulnerabilities". Nothing else is affected.

## A project shows a warning as soon as Bombadil starts

Bombadil checks every project's folder when it starts and when its window
comes back to the front. A folder on a drive that isn't connected, or one
you moved, shows the warning straight away. Select the project to
**Locate…** its `pyproject.toml` or **Remove** it from the list.
