# Running code

The **Run** tab runs a project's code inside its default environment, with the
same environment variables, secrets and pre-activate scripts a terminal gets.
The output appears in the tab as the program prints it.

Each project can have one program running at a time. Other projects keep
working: you can sync one project while another runs a server.

A program can only start once the project has an environment. If it doesn't,
the buttons say **Sync first to create the environment.**

## Entry points

If the project's `pyproject.toml` has a `[project.scripts]` table, each name in
it gets a button. Pressing one runs `uv run <name>`. A project made with
**New project › App** has one from the start.

## Saved commands

Save the commands you run often, such as `pytest -q` or `python main.py`. Give
each a name (optional) and press **Add**. **Edit** loads a command back into
the form, and **Remove** deletes it. Saved commands are stored per project in
[`config.toml`](configuration.md).

A command is split into words on spaces. Put a word that contains spaces in
quotes: `pytest -k "not slow"`. Backslashes are kept as they are, so a Windows
path such as `python scripts\run.py` works.

## Run a file

**Run a file…** asks for a `.py` file and runs it with `python`. The file is
added to your saved commands, so it is one click the next time.

## Jupyter

**Open in Jupyter** starts JupyterLab for the project and opens it in your
browser. JupyterLab runs from a temporary install: your project's
dependencies and `uv.lock` don't change.

## Stopping

**Stop** asks the program to finish, the same way closing a terminal window
does. If it hasn't ended after three seconds, Bombadil ends it. Anything the
program started, such as the Python process behind `uv run`, ends with it.

When a program ends, the tab says how: **exited with 0** means it finished
normally; any other number means it reported a problem, and the tab adds a
short explanation when it recognises one.

Bombadil asks before it would interrupt a program:

- **Sync** or **Recreate venv** on a running project asks **Stop the run and
  continue?** The sync starts once the program has stopped.
- **Quitting** asks first, then stops every running program.
- **Remove from list** stops the project's program.

## Programs that ask for input

A running program can't read from the keyboard here. If it waits for input,
use **Open terminal** instead.
