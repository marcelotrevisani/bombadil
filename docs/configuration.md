# Configuration file

Bombadil keeps everything in one TOML file. You never need to edit it by
hand, but it is plain text, and you can back it up, read it, or copy it to
another machine.

| OS | Location |
|---|---|
| Linux | `~/.config/bombadil/config.toml` (or `$XDG_CONFIG_HOME/bombadil/`) |
| macOS | `~/Library/Application Support/bombadil/config.toml` |
| Windows | `%APPDATA%\bombadil\config\config.toml` |

Secrets are **not** in this file. Secret variables and index credentials are
kept in the OS keychain. Copying the file to another machine copies the
fact that a secret exists, not its value. Set the value again there.

## Example

```toml
schema_version = 2

[settings]
theme = "system"            # "system" | "light" | "dark"
close_to_tray = true
# pycharm = "/opt/pycharm/bin/pycharm.sh"   # omit to detect automatically

[settings.uv_source]
kind = "auto"               # "auto" | "bundled" | "from_path"
# kind = "custom"
# path = "/usr/local/bin/uv"

[settings.default_venv_location]
kind = "alongside"          # or: kind = "central", path = "/home/alex/.venvs"

[settings.python]
kind = "unpinned"           # or: kind = "version", value = "3.12"

[[indexes]]
name = "acme"
url = "https://pypi.acme.example/simple"
kind = "extra"              # "default" replaces PyPI, "extra" supplements it
default_for_new_projects = false
[indexes.auth]
type = "token"              # "none" | "basic" (add username = "...") | "token"

[[global_env]]
key = "UV_HTTP_TIMEOUT"
value = "60"
secret = false

[[global_scripts]]
path = "/home/alex/scripts/aws-dev.sh"
enabled = true

[[projects]]
id = "6f1d2c3a-1b2c-4d5e-8f90-111111111111"
label = "weather-api"
pyproject_path = "/home/alex/code/weather-api/pyproject.toml"
index_names = ["acme"]

[[projects.environments]]
location = { kind = "alongside" }      # "default" | "alongside" | "custom" (+ path)
python = { kind = "version", value = "3.13" }

[[projects.environments]]
location = { kind = "custom", path = "/home/alex/.venvs/weather-api-py312" }
python = { kind = "version", value = "3.12" }

[projects.active]           # which environment is the default
kind = "alongside"

[[projects.env]]
key = "WEATHER_API_KEY"
value = ""                  # always empty for a secret; the value is in the keychain
secret = true

[projects.uv_source]
kind = "inherit"            # or: kind = "set", value = { kind = "bundled" }

[terminal_defaults.linux]
kind = "detected"
value = "gnome-terminal"
```

## A terminal Bombadil doesn't know

To use a terminal that isn't in the list, set a command template in the file.
`{cwd}` is replaced with the project folder and `{shell}` with your shell:

```toml
[terminal_defaults.linux]
kind = "custom"
value = { template = "myterm --cd {cwd} -e {shell}" }
```

## Rules worth knowing

- **Bombadil rewrites the file** whenever you change something in the app.
  Comments and fields it doesn't recognise are not kept. Edit the file while
  Bombadil is closed.
- **A config from a newer version opens read-only.** If the file's
  `schema_version` is newer than your Bombadil supports, the app shows it but
  never writes to it, so an older build can't damage a newer config.
- **Older configs are upgraded on load.** Fields added in later versions take
  their defaults.
- **Projects are identified by `id`, not by path.** Moving a project folder
  and updating `pyproject_path` keeps its variables and scripts.
