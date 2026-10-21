# Terminals and PyCharm

The project header has a button for each: **>_** opens a terminal and **PC**
opens PyCharm. The **Ju** button next to them starts JupyterLab; see
[Running code](running.md#jupyter).

## Open a terminal

Click the **>_** button on a project, or press ++ctrl+t++. A terminal opens
in the project folder with the default environment activated: `python` and
`uv` use the environment, and your variables, secrets, index settings and
scripts are applied.

Choose which terminal opens in [Settings › Terminal](settings.md#terminal).
To use a different terminal just once, pick it from **Open terminal** in the
project menu (**⋯**, or right-click the project). The tray can also open a
terminal for any environment, not only the default one.

### Supported terminals

| OS | Terminals | Activation |
|---|---|---|
| Linux | GNOME Terminal, Konsole, kitty, Alacritty, WezTerm, foot, xterm, Ptyxis, Ghostty, Terminator | Full |
| Linux | Warp | Environment activated only |
| Windows | Windows Terminal | Full |
| Windows | PowerShell, Command Prompt, Git Bash | Environment on PATH only |
| Windows | WSL | None |
| macOS | Terminal.app, iTerm2 | None |

- **Full**: the environment is put at the front of `PATH` again after your
  shell's own startup files have run, so a `.bashrc` or `.zshrc` that changes
  `PATH` can't push it back.
- **Environment activated only**: the environment is activated and kept at the
  front of `PATH`, so `python` and `uv` use it — but your variables, secrets,
  index settings and scripts are not applied. Warp is a single-instance
  program: opening it again hands the request to the window that is already
  open, and variables don't survive that trip. Bombadil works around it by
  asking Warp to run the activation itself in the tab it opens. That tab is
  titled `Bombadil: <project>`, and it also shows up in Warp's own **+** menu.
  Use a **Full** terminal, or the [Run tab](running.md), when a command needs
  your variables.
- **Environment on PATH only**: the variables arrive, but a startup file that
  changes `PATH` can put another Python in front.
- **None**: the terminal starts in the project folder without the
  environment. On macOS this is because Terminal.app and iTerm2 are launched
  through `open`, which doesn't pass environment variables on. For WSL,
  nothing crosses into the Linux distribution. Settings and the menu say so
  next to each such terminal.

A terminal that isn't installed is still listed. Opening it tells you it
couldn't be found.

## Open in PyCharm

Click the **PC** button on a project, or use **Open in PyCharm** in the tray
menu. Bombadil does three things:

1. **Sets the project interpreter** to the environment, in the project's
   `.idea` folder and in PyCharm's list of Python SDKs. Modules inside the
   project that were pinned to another of the project's environments are
   switched to use the project interpreter.
2. **Passes the composed environment** to PyCharm, so its terminal and run
   configurations see your variables, secrets and the activated environment.
3. **Opens the project folder** in PyCharm.

Choose which PyCharm to use in [Settings › PyCharm](settings.md#pycharm). By
default, Bombadil finds it in the usual install locations:

- **Linux**: the JetBrains Toolbox script, the PyCharm snaps,
  `/opt/pycharm/bin/pycharm.sh`, or `pycharm` on `PATH`.
- **macOS**: the Toolbox script, or PyCharm (or PyCharm CE) in
  `/Applications` or `~/Applications`.
- **Windows**: the Toolbox script, or
  `C:\Program Files\JetBrains\PyCharm\bin\pycharm64.exe`.

!!! warning "Close the project in PyCharm first"
    PyCharm keeps its SDK list in memory and writes it back when it exits,
    which would undo the change. While PyCharm is running, Bombadil only
    reuses an SDK entry that already points at the environment. Otherwise it
    opens the project anyway and tells you which interpreter to select by
    hand. See [Gotchas](gotchas.md#pycharm-shows-the-wrong-interpreter).
