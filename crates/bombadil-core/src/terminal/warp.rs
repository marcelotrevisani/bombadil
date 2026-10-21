//! Launching Warp through a Tab Config, because Warp's argv cannot carry an
//! environment.
//!
//! Warp is single-instance. A second `warp-terminal` is a short-lived forwarder
//! that hands the request to the already-running instance and exits, so the
//! composed environment dies with it, and `warp-terminal --help` (verified
//! against 0.2026.09.16) exposes no `--working-directory` and no `-e`/`--`
//! passthrough to build an argv from. `argv.rs` therefore still spawns Warp
//! bare, and Warp sits outside the `Full` bucket.
//!
//! What *does* cross is an argument. Warp registers itself as the handler for
//! `warp://` (`MimeType=x-scheme-handler/warp;`, `Exec=warp-terminal %U`), and a
//! Tab Config is a TOML file Warp reads from its own data directory which can
//! specify both a working directory and commands to run in the pane:
//!
//! ```toml
//! name = "Bombadil: my-api"
//!
//! [[panes]]
//! id = "main"
//! type = "terminal"
//! directory = "/proj/my-api"
//! commands = ["source '/proj/my-api/.venv/bin/activate'"]
//! ```
//!
//! `commands` runs inside the pane's own interactive shell, *after* that shell
//! has read its rc -- which is the same ordering property `activation.rs`
//! exists to obtain, reached by executing activation rather than by inheriting
//! an environment. Measured on Linux against warp-terminal 0.2026.09.16 with
//! Warp warm (the pane's `ppid` was a `warp terminal-server` that had been up
//! for hours): `VIRTUAL_ENV` was set, `command -v python` resolved into the
//! venv, and `PATH` led with `.venv/bin`. A control pane launched with a
//! sentinel variable and `VIRTUAL_ENV` set on the *forwarder's* environment
//! showed both empty, which is what proves the environment still does not cross
//! and that this mechanism -- not inheritance -- is doing the work.
//!
//! ## What this does not deliver
//!
//! Only activation crosses. The composed environment also carries project
//! variables and keychain secrets, and those are deliberately *not* written
//! here: this file lives in `~/.local/share`, persists after the terminal
//! closes, and is readable by anything running as the user. `open.rs` keeps
//! secrets in process environment only, and Warp's
//! [`ActivationSupport::VenvOnly`](super::ActivationSupport::VenvOnly) bucket is
//! what tells the user their project variables did not arrive.
//!
//! ## Known limitations
//!
//! - Warp interpolates `{{param}}` inside `directory` and `commands`. A project
//!   path containing a literal `{{` would be mangled rather than rejected. No
//!   escaping syntax is documented, so this is noted rather than handled.
//! - The file is left behind on purpose: Warp reads it asynchronously after we
//!   spawn, so deleting it would race the launch. Relaunching the same project
//!   overwrites it. Warp's documented behaviour is that saved Tab Configs also
//!   appear in its `+` menu under `name`, so the user accumulates one
//!   `Bombadil: <project>` entry per project they have opened.
//! - Linux only, matching `TerminalKind::os`. The macOS and Windows Tab Config
//!   locations are unverified, so neither is guessed at here.

use crate::process::CommandSpec;
use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A Tab Config, in the field order TOML requires: scalars before tables.
#[derive(Serialize)]
struct TabConfig {
    name: String,
    panes: Vec<Pane>,
}

#[derive(Serialize)]
struct Pane {
    id: String,
    #[serde(rename = "type")]
    pane_type: String,
    directory: String,
    is_focused: bool,
    /// Omitted entirely when empty rather than written as `commands = []`: a
    /// shell we do not know still gets the right working directory, which is
    /// more than the bare spawn manages.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commands: Vec<String>,
}

/// Where Warp looks for Tab Configs on this machine, or `None` if there is no
/// home directory to put one in.
///
/// `BaseDirs::data_dir` is `$XDG_DATA_HOME` or `~/.local/share`, which is the
/// location Warp documents.
pub fn default_base() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.data_dir().join("warp-terminal").join("tab_configs"))
}

/// The stem both the filename and the `warp://` URI are built from.
///
/// Namespaced by project id for the reason `open.rs` namespaces its init files:
/// two projects open at once must not overwrite each other's config, and the
/// same project relaunched must overwrite its own rather than accumulate.
fn stem(project_id: Uuid) -> String {
    format!("bombadil-{project_id}")
}

/// The URI that tells Warp to open `project_id`'s Tab Config.
///
/// `<stem>` matches the file stem, not the `name` field inside it.
pub fn deep_link(project_id: Uuid) -> String {
    format!("warp://tab_config/{}", stem(project_id))
}

/// Where `project_id`'s Tab Config is written under `base`.
pub fn config_path(base: &Path, project_id: Uuid) -> PathBuf {
    base.join(format!("{}.toml", stem(project_id)))
}

/// Render the Tab Config for a pane opening in `directory`, running `command`
/// first if there is one.
pub fn render(label: &str, directory: &Path, command: Option<String>) -> String {
    let config = TabConfig {
        name: format!("Bombadil: {label}"),
        panes: vec![Pane {
            id: "main".into(),
            pane_type: "terminal".into(),
            directory: directory.to_string_lossy().into_owned(),
            is_focused: true,
            commands: command.into_iter().collect(),
        }],
    };
    // `toml` only fails on a map with non-string keys or a value after a table,
    // neither of which this struct can express.
    toml::to_string_pretty(&config).expect("TabConfig is serializable")
}

/// The argv that asks a running Warp to open `uri`.
///
/// `warp-terminal <uri>` rather than `xdg-open <uri>`: Warp is already known to
/// be installed (that is how it was detected), and going through `warp-terminal`
/// does not add a dependency on `xdg-open` being present. Both forms were
/// measured working; this one has fewer moving parts.
///
/// No `?new_window=true`, so the documented default applies: a new tab in the
/// focused window, or a new window when none is open. That differs from the
/// other Linux kinds, which each open a window, but the tab is titled
/// `Bombadil: <project>` -- which window a terminal opened in says nothing
/// about which project it belongs to. The parameter exists if a separate window
/// turns out to be wanted; it is documented but was not part of the
/// measurement.
pub fn launch_spec(uri: &str, cwd: &Path) -> CommandSpec {
    CommandSpec::new("warp-terminal")
        .arg(uri)
        .cwd(cwd.to_path_buf())
}

/// Write `project_id`'s Tab Config under `base` and return the URI to open it
/// with, or `None` if it could not be written.
///
/// Falling back rather than erroring is deliberate and matches `open.rs`'s rule
/// that losing activation is acceptable while refusing to open a terminal is
/// not: an unwritable data directory should leave the user with today's
/// unactivated Warp, not with no terminal at all.
pub fn prepare(
    base: Option<&Path>,
    project_id: Uuid,
    label: &str,
    cwd: &Path,
    command: Option<String>,
) -> Option<String> {
    let base = base?;
    std::fs::create_dir_all(base).ok()?;
    let path = config_path(base, project_id);
    std::fs::write(&path, render(label, cwd, command)).ok()?;
    Some(deep_link(project_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Uuid {
        Uuid::from_u128(7)
    }

    #[test]
    fn a_command_with_quotes_and_backslashes_survives_rendering_unchanged() {
        // `toml` switches to a multi-line basic string (`"""..."""`) once a
        // value contains a double quote, and that form *interprets* backslash
        // escapes. So a path holding a quote or a backslash could come back as
        // a different string than the one built here, and Warp would then run
        // something else -- silently, since the launch still succeeds. Warp was
        // measured accepting the multi-line form; this pins that the escaping
        // inside it is right, for both interpolated fields.
        let command = r#"echo "a\b" && source '/home/o'brien/.venv/bin/activate'"#;
        let directory = Path::new(r#"/proj/my"api\sub"#);
        let text = render("my-api", directory, Some(command.into()));

        let parsed: toml::Value = toml::from_str(&text).expect("must be valid TOML");
        let pane = &parsed["panes"][0];
        assert_eq!(pane["commands"][0].as_str().unwrap(), command);
        assert_eq!(
            pane["directory"].as_str().unwrap(),
            directory.to_string_lossy()
        );
    }

    #[test]
    fn the_config_carries_the_directory_and_the_activation_command() {
        let text = render(
            "my-api",
            Path::new("/proj/my-api"),
            Some("source '/proj/my-api/.venv/bin/activate'".into()),
        );
        assert!(text.contains("name = \"Bombadil: my-api\""), "got {text}");
        assert!(text.contains("[[panes]]"), "got {text}");
        assert!(text.contains("directory = \"/proj/my-api\""), "got {text}");
        assert!(
            text.contains("source '/proj/my-api/.venv/bin/activate'"),
            "got {text}"
        );
    }

    #[test]
    fn the_rendered_config_is_valid_toml_that_warp_can_parse() {
        // A hand-formatted string could produce something Warp silently
        // ignores, which would look exactly like a successful launch with no
        // activation. Round-tripping through the parser pins the syntax.
        let text = render("my-api", Path::new("/proj/my-api"), Some("true".into()));
        let parsed: toml::Value = toml::from_str(&text).expect("must be valid TOML");
        let panes = parsed.get("panes").and_then(|p| p.as_array()).unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(
            panes[0].get("type").and_then(|v| v.as_str()),
            Some("terminal")
        );
        assert_eq!(
            panes[0]
                .get("commands")
                .and_then(|v| v.as_array())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn a_label_containing_a_quote_cannot_break_out_of_the_toml_string() {
        // The label is user-controlled and lands inside a double-quoted TOML
        // value; an unescaped quote would corrupt the file and Warp would
        // refuse to open the tab at all.
        let text = render("my\"api", Path::new("/proj"), None);
        let parsed: toml::Value = toml::from_str(&text).expect("must stay valid TOML");
        assert_eq!(
            parsed.get("name").and_then(|v| v.as_str()),
            Some("Bombadil: my\"api")
        );
    }

    #[test]
    fn an_unknown_shell_still_gets_a_directory_and_no_commands_key() {
        // Degrading to cwd-only is still better than the bare spawn, which
        // loses the directory too on a warm start.
        let text = render("my-api", Path::new("/proj/my-api"), None);
        assert!(!text.contains("commands"), "got {text}");
        assert!(text.contains("directory"), "got {text}");
    }

    #[test]
    fn the_deep_link_names_the_file_stem_not_the_display_name() {
        // Warp matches `warp://tab_config/<stem>` against the file stem. Getting
        // this wrong opens nothing, and nothing about the spawn fails -- the
        // forwarder exits 0 either way.
        assert_eq!(
            deep_link(id()),
            "warp://tab_config/bombadil-00000000-0000-0000-0000-000000000007"
        );
        let base = Path::new("/data/tab_configs");
        assert_eq!(
            config_path(base, id()),
            base.join("bombadil-00000000-0000-0000-0000-000000000007.toml")
        );
    }

    #[test]
    fn prepare_writes_the_config_and_returns_the_link_that_opens_it() {
        let tmp = tempfile::tempdir().unwrap();
        let uri = prepare(
            Some(tmp.path()),
            id(),
            "my-api",
            Path::new("/proj/my-api"),
            Some("source '/proj/my-api/.venv/bin/activate'".into()),
        )
        .expect("must succeed with a writable base");

        assert_eq!(uri, deep_link(id()));
        let written = std::fs::read_to_string(config_path(tmp.path(), id())).unwrap();
        assert!(written.contains("/proj/my-api"), "got {written}");
        assert!(written.contains(".venv/bin/activate"), "got {written}");
    }

    #[test]
    fn two_projects_do_not_share_a_tab_config() {
        // Same collision `open.rs` guards for init files: a shared filename
        // would let the second project's launch overwrite the first one's
        // config before Warp had read it, opening the wrong environment.
        let tmp = tempfile::tempdir().unwrap();
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        prepare(Some(tmp.path()), a, "alpha", Path::new("/proj/alpha"), None).unwrap();
        prepare(Some(tmp.path()), b, "beta", Path::new("/proj/beta"), None).unwrap();

        let text_a = std::fs::read_to_string(config_path(tmp.path(), a)).unwrap();
        let text_b = std::fs::read_to_string(config_path(tmp.path(), b)).unwrap();
        assert!(text_a.contains("/proj/alpha") && !text_a.contains("/proj/beta"));
        assert!(text_b.contains("/proj/beta") && !text_b.contains("/proj/alpha"));
    }

    #[test]
    fn relaunching_the_same_project_overwrites_rather_than_accumulating() {
        let tmp = tempfile::tempdir().unwrap();
        prepare(Some(tmp.path()), id(), "old", Path::new("/proj/old"), None).unwrap();
        prepare(Some(tmp.path()), id(), "new", Path::new("/proj/new"), None).unwrap();

        let count = std::fs::read_dir(tmp.path()).unwrap().count();
        assert_eq!(count, 1, "one project must own one file");
        let text = std::fs::read_to_string(config_path(tmp.path(), id())).unwrap();
        assert!(
            text.contains("/proj/new") && !text.contains("/proj/old"),
            "got {text}"
        );
    }

    #[test]
    fn a_missing_base_directory_degrades_to_no_link() {
        // `open.rs` falls back to the bare spawn on None, so the user still
        // gets a terminal -- unactivated, as before, rather than none at all.
        assert!(prepare(None, id(), "my-api", Path::new("/proj"), None).is_none());
    }

    #[test]
    fn the_launch_argv_hands_the_uri_to_warp_itself() {
        let spec = launch_spec(&deep_link(id()), Path::new("/proj/my-api"));
        assert_eq!(spec.program, PathBuf::from("warp-terminal"));
        assert_eq!(spec.args, vec![deep_link(id())]);
        assert_eq!(spec.cwd, Some(PathBuf::from("/proj/my-api")));
    }
}
