# Bombadil

![Bombadil](images/banner.png)

Bombadil is a desktop app for the Python virtual environments you already
have. It shows which environments exist, which have drifted from what their
`pyproject.toml` declares, and what is installed in each. It also gives you one
place to sync them, look inside them, and open them in a terminal or PyCharm.

It never owns an environment. [uv](https://docs.astral.sh/uv/) creates them and
resolves them. Bombadil tells you the truth about them and gets out of the way.
If you uninstall Bombadil, every environment it showed you is still there,
untouched.

![The main window](images/main.png)

## What you can do with it

- **Keep every project in one list.** Each project shows its environments as a
  tree, with the default one marked. A project can have several environments,
  for example one on Python 3.12 and one on 3.13.
- **See drift at a glance.** Each package, environment and workspace member is
  marked present, missing or drifted.
- **Sync with the output in view.** `uv sync` streams into a drawer at the
  bottom of the window. If it fails, you see the full output.
- **Manage dependencies.** Browse the resolved tree, change a version, upgrade
  one package or all of them, or add a local package.
- **Handle monorepos.** Choose which workspace members are installed as
  editable packages.
- **Set environment variables and secrets.** Variables can be global or per
  project. Secrets live in your OS keychain, never in the config file.
- **Use private package indexes,** with their credentials kept in the keychain.
- **Open a terminal or PyCharm** with the environment already activated.
- **Use the tray.** Closing the window keeps Bombadil in the system tray, with
  a menu for every project.

## Where to start

1. [Install Bombadil](install.md).
2. Follow [Getting started](getting-started.md) to add your first project.
3. Read [Gotchas](gotchas.md) before you change a Python version or point
   PyCharm at an environment.
