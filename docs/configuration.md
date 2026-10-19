# Configuration file

Bombadil keeps everything in one TOML file. You don't need to edit it by
hand, but it is plain text, so you can read it, back it up or copy it to
another machine.

| OS | Location |
|---|---|
| Linux | `~/.config/bombadil/config.toml` (or `$XDG_CONFIG_HOME/bombadil/`) |
| macOS | `~/Library/Application Support/bombadil/config.toml` |
| Windows | `%APPDATA%\bombadil\config\config.toml` |

Secrets are **not** in this file. Secret variables and index credentials are
kept in the OS keychain. A copy of the file on another machine lists the
secret variables without their values, so set the values again there.

## Example

```toml
schema_version = 2

[settings]
theme = "system"            # "system" | "light" | "dark"
close_to_tray = true
watch_github = true         # look up each project's pull request
check_vulnerabilities = true  # ask osv.dev about installed packages
just_source = "auto"        # "auto" | "bundled" | "from_path"
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

- Bombadil rewrites the file whenever you change something in the app, and
  drops comments and fields it doesn't recognise. Edit the file while
  Bombadil is closed.
- If the file's `schema_version` is newer than your Bombadil supports, the
  app opens it read-only, so an older build can't damage a newer config.
- An older config is upgraded on load. Fields added in later versions take
  their defaults.
- A project is identified by its `id`. If you move a project folder and update
  `pyproject_path`, its variables and scripts stay with it.
