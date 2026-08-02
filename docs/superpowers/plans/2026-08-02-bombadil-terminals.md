# Bombadil Terminals Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect the terminal emulators actually present on this machine and open one in a project's directory with its virtual environment genuinely activated.

**Architecture:** A new `terminal` module in `bombadil-core`. Emulator detection probes for binaries (or macOS app bundles) and returns only what exists. Argv construction takes the target OS as a **function parameter, not a `cfg!`**, so Windows and macOS argv are covered by tests running on Linux. Activation is re-asserted *after* the user's shell startup files run, via a generated per-shell init file, because anything set before the shell starts can be — and on a stock Ubuntu, is — overridden.

**Tech Stack:** Rust edition 2024, `bombadil-core` only. No new dependencies. `CommandRunner` is the single seam through which anything spawns.

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`. **No new crate dependencies.**
- `bombadil-core` must not depend on `iced` or on `bombadil-uv-bin`.
- **No unit test may spawn a real process, touch the real OS keychain, reach the network, or require a display.** `FakeCommandRunner` and `InMemorySecretStore` only.
- **A secret must never appear in a log line, a `Debug` render, or a failure report.** The composed environment passed to a terminal contains secrets.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`.
- Test fixtures must name the platform's shell via the existing `shell()` helpers where they spawn through `apply`; hardcoding `"sh"` breaks the Windows CI job.
- **The target OS is a parameter, never `cfg!`,** in every function that builds argv. Per spec §10 this is what makes Windows argv testable on Linux.

## Facts Verified Empirically Before This Plan

Checked on a real Ubuntu machine with a live display. Do not re-litigate these.

- **`gnome-terminal` forwards the client's environment to the shell it starts**, even when a `gnome-terminal-server` is already running. Two back-to-back spawns with different values each saw their own. This contradicts widespread folklore that the server's environment wins; it was tested because the folklore would have changed the design.
- **`--working-directory` works**, and the shell's `pwd` is the requested directory.
- **A user's rc file can demote our activation.** Passing `PATH=/venv/bin:$PATH` and starting an interactive bash put `/venv/bin` at **position 3**, behind `~/.local/bin`, because `.bashrc` prepends after we set it. `VIRTUAL_ENV` survived, so the prompt reports the venv as active while `python` could resolve to `~/.local/bin/python` — which pipx and `pip --user` both create. Non-interactively `/venv/bin` is position 1, so this affects terminals only, not uv commands.
- **`bash --rcfile <file> -i` fixes it** — `/venv/bin` returns to position 1.
- **`bash --rcfile` is silently IGNORED for login shells.** With `-l`, bash reads `~/.bash_profile` and our file is never sourced; `/venv/bin` fell to position 5. macOS Terminal.app always starts login shells and gnome-terminal offers it as an option, so an implementation that lets the emulator choose the shell works on Linux and fails on macOS.
- **`zsh` via `ZDOTDIR` works** — `/venv/bin` at position 1.
- **`--wait` blocks until the child exits.** Bombadil must not pass it: an interactive terminal exits when the user closes the window, which would block the GUI. Consequently **the spawn's exit status says nothing about whether the terminal opened**, and "chosen terminal has disappeared" must be detected by the binary being absent, not by an exit code.

---

### Task 1: The terminal model

**Files:**
- Create: `crates/bombadil-core/src/terminal/mod.rs`
- Modify: `crates/bombadil-core/src/model/mod.rs`, `crates/bombadil-core/src/lib.rs`

**Interfaces:**
- Consumes: `model::Config` (plan 1).
- Produces:
  - `terminal::Os { Linux, MacOs, Windows }` with `Os::host()`
  - `terminal::TerminalKind` — one variant per emulator in spec §8
  - `model::TerminalChoice { Detected(TerminalKind), Custom { template: String } }`
  - `Config::terminal_defaults: BTreeMap<Os, TerminalChoice>`

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/terminal/mod.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_knows_which_os_it_belongs_to() {
        assert_eq!(TerminalKind::GnomeTerminal.os(), Os::Linux);
        assert_eq!(TerminalKind::ITerm2.os(), Os::MacOs);
        assert_eq!(TerminalKind::WindowsTerminal.os(), Os::Windows);
    }

    #[test]
    fn kinds_for_an_os_are_only_that_oss_kinds() {
        for kind in TerminalKind::for_os(Os::Linux) {
            assert_eq!(kind.os(), Os::Linux, "{kind:?} is not a Linux terminal");
        }
        assert!(TerminalKind::for_os(Os::MacOs).contains(&TerminalKind::TerminalApp));
        assert!(!TerminalKind::for_os(Os::MacOs).contains(&TerminalKind::Konsole));
    }

    #[test]
    fn the_spec_list_is_complete_for_every_os() {
        // Spec section 8 names these exactly. A missing entry means a terminal
        // the user has is never offered, which reads as "Bombadil does not
        // support my setup" rather than as a bug.
        assert_eq!(TerminalKind::for_os(Os::Windows).len(), 5);
        assert_eq!(TerminalKind::for_os(Os::MacOs).len(), 2);
        assert_eq!(TerminalKind::for_os(Os::Linux).len(), 7);
    }

    #[test]
    fn a_terminal_choice_round_trips_through_toml() {
        let choice = TerminalChoice::Detected(TerminalKind::Kitty);
        let text = toml::to_string(&Wrapper { choice: choice.clone() }).unwrap();
        let back: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(back.choice, choice);
    }

    #[test]
    fn a_custom_choice_round_trips_through_toml() {
        let choice = TerminalChoice::Custom {
            template: "myterm --cd {cwd} -e {shell}".into(),
        };
        let text = toml::to_string(&Wrapper { choice: choice.clone() }).unwrap();
        let back: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(back.choice, choice);
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrapper {
        choice: TerminalChoice,
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core terminal`
Expected: FAIL — `cannot find type Os`.

- [ ] **Step 3: Implement**

Above the tests. Put `TerminalChoice` in `model/mod.rs` and re-export it; `Os` and `TerminalKind` live here.

```rust
use serde::{Deserialize, Serialize};

/// The operating system a terminal command is being built for.
///
/// This is a parameter rather than a `cfg!` everywhere it appears, so that
/// Windows and macOS argv construction is covered by tests that run on Linux.
/// Per spec section 10 that is a deliberate testability requirement, not a
/// stylistic choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

impl Os {
    /// The OS this build is running on.
    pub fn host() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// A terminal emulator Bombadil knows how to launch. Spec section 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalKind {
    // Windows
    WindowsTerminal,
    PowerShell,
    Cmd,
    GitBash,
    Wsl,
    // macOS
    TerminalApp,
    ITerm2,
    // Linux
    GnomeTerminal,
    Konsole,
    Kitty,
    Alacritty,
    WezTerm,
    Foot,
    Xterm,
}

impl TerminalKind {
    pub fn os(self) -> Os {
        use TerminalKind::*;
        match self {
            WindowsTerminal | PowerShell | Cmd | GitBash | Wsl => Os::Windows,
            TerminalApp | ITerm2 => Os::MacOs,
            GnomeTerminal | Konsole | Kitty | Alacritty | WezTerm | Foot | Xterm => Os::Linux,
        }
    }

    /// Every terminal this OS could have, in the order they are offered.
    pub fn for_os(os: Os) -> Vec<TerminalKind> {
        use TerminalKind::*;
        match os {
            Os::Windows => vec![WindowsTerminal, PowerShell, Cmd, GitBash, Wsl],
            Os::MacOs => vec![TerminalApp, ITerm2],
            Os::Linux => vec![GnomeTerminal, Konsole, Kitty, Alacritty, WezTerm, Foot, Xterm],
        }
    }
}
```

Add to `model/mod.rs`:

```rust
/// Which terminal to open, per operating system.
///
/// Adjacently tagged, not internally tagged: `Detected` is a newtype variant
/// wrapping a unit enum, which serialises as a bare string, and serde's
/// internally-tagged representation cannot carry that. `#[serde(tag = "kind")]`
/// alone compiles and then fails at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum TerminalChoice {
    Detected(crate::terminal::TerminalKind),
    /// A command template with `{cwd}` and `{shell}` placeholders, for a
    /// terminal Bombadil does not recognise. Spec section 8.
    Custom { template: String },
}
```

and to `Config`:

```rust
    #[serde(default)]
    pub terminal_defaults: BTreeMap<crate::terminal::Os, TerminalChoice>,
```

Update `Config::default()` with `terminal_defaults: BTreeMap::new()`. Add `pub mod terminal;` to `lib.rs`.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace --locked
git add crates/bombadil-core/src/terminal crates/bombadil-core/src/model crates/bombadil-core/src/lib.rs
git commit -m "feat(terminal): add the terminal model with OS as data

Os is a parameter rather than a cfg! so Windows and macOS argv construction
is covered by tests that run on Linux, per spec section 10."
```

---

### Task 2: Re-asserting activation after the user's shell startup files

**Files:**
- Create: `crates/bombadil-core/src/terminal/activation.rs`
- Modify: `crates/bombadil-core/src/terminal/mod.rs`

**Interfaces:**
- Produces:
  - `terminal::activation::ShellFamily { Bash, Zsh, Fish, PosixSh, Unknown }`
  - `ShellFamily::detect(shell_path: &str) -> ShellFamily`
  - `activation::init_file(family, venv_bin: &Path) -> Option<InitFile>`
  - `activation::InitFile { pub relative_path: PathBuf, pub contents: String, pub shell_args: Vec<String>, pub env: BTreeMap<String, String> }`

> **Why this exists, verified on a real machine.** Passing `PATH=/venv/bin:$PATH` to an
> interactive bash put `/venv/bin` at position **3**, behind `~/.local/bin`, because
> `.bashrc` prepends after we set it. `VIRTUAL_ENV` still said the venv was active, so the
> prompt looks right while `python` resolves elsewhere — pipx and `pip --user` both put a
> `python` in `~/.local/bin`. Anything set *before* the shell starts can be overridden;
> only something running *after* the rc files can win.
>
> **`bash --rcfile` is ignored for login shells** — with `-l`, bash reads `~/.bash_profile`
> and our file is never sourced (`/venv/bin` fell to position 5). macOS Terminal.app always
> starts a login shell. Bombadil therefore spawns the shell **explicitly** as interactive
> non-login rather than letting the emulator pick, and the generated file sources the user's
> own startup files itself so nothing is lost.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/terminal/activation.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_shell_is_recognised_by_its_file_name_not_its_directory() {
        assert_eq!(ShellFamily::detect("/usr/bin/bash"), ShellFamily::Bash);
        assert_eq!(ShellFamily::detect("/bin/zsh"), ShellFamily::Zsh);
        assert_eq!(ShellFamily::detect("/usr/local/bin/fish"), ShellFamily::Fish);
        assert_eq!(ShellFamily::detect("/bin/sh"), ShellFamily::PosixSh);
        assert_eq!(ShellFamily::detect("/usr/bin/nu"), ShellFamily::Unknown);
    }

    #[test]
    fn a_bash_users_own_rc_is_sourced_before_we_prepend() {
        // Order is the whole point: sourcing after our prepend would let the
        // user's rc demote the venv again, which is the bug this file exists
        // to fix.
        let file = init_file(ShellFamily::Bash, Path::new("/envs/api/bin")).unwrap();
        let src = file.contents.find(".bashrc").expect("must source ~/.bashrc");
        let prepend = file.contents.find("/envs/api/bin").expect("must prepend");
        assert!(src < prepend, "got {}", file.contents);
    }

    #[test]
    fn bash_is_started_interactive_and_not_as_a_login_shell() {
        // Verified: bash silently ignores --rcfile when started with -l, so a
        // login shell would never source our file at all.
        let file = init_file(ShellFamily::Bash, Path::new("/envs/api/bin")).unwrap();
        assert!(file.shell_args.iter().any(|a| a == "--rcfile"));
        assert!(file.shell_args.iter().any(|a| a == "-i"));
        assert!(
            !file.shell_args.iter().any(|a| a == "-l" || a == "--login"),
            "got {:?}",
            file.shell_args
        );
    }

    #[test]
    fn zsh_uses_zdotdir_rather_than_an_argument() {
        let file = init_file(ShellFamily::Zsh, Path::new("/envs/api/bin")).unwrap();
        assert_eq!(file.relative_path, std::path::PathBuf::from(".zshrc"));
        assert_eq!(file.env.get("ZDOTDIR").map(String::as_str), Some("{dir}"));
    }

    #[test]
    fn fish_prepends_with_its_own_builtin() {
        let file = init_file(ShellFamily::Fish, Path::new("/envs/api/bin")).unwrap();
        assert!(file.contents.contains("fish_add_path"), "got {}", file.contents);
        assert!(file.contents.contains("--prepend"), "got {}", file.contents);
    }

    #[test]
    fn an_unknown_shell_gets_no_init_file_rather_than_a_broken_one() {
        // A bash rcfile handed to nushell would produce a syntax error on
        // every terminal launch, which is worse than losing the reassertion.
        assert!(init_file(ShellFamily::Unknown, Path::new("/envs/api/bin")).is_none());
    }

    #[test]
    fn a_venv_path_containing_a_quote_is_escaped() {
        let file = init_file(ShellFamily::Bash, Path::new("/home/o'brien/env/bin")).unwrap();
        assert!(
            file.contents.contains(r"'\''") || !file.contents.contains("o'brien'"),
            "an unescaped apostrophe would break every terminal launch; got {}",
            file.contents
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core terminal::activation`
Expected: FAIL — `cannot find type ShellFamily`.

- [ ] **Step 3: Implement**

```rust
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A shell family Bombadil knows how to re-assert activation in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFamily {
    Bash,
    Zsh,
    Fish,
    PosixSh,
    Unknown,
}

impl ShellFamily {
    /// Recognise by file name, so `/opt/homebrew/bin/bash` is still bash.
    pub fn detect(shell_path: &str) -> Self {
        let name = Path::new(shell_path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match name.as_str() {
            "bash" => ShellFamily::Bash,
            "zsh" => ShellFamily::Zsh,
            "fish" => ShellFamily::Fish,
            "sh" | "dash" | "ash" => ShellFamily::PosixSh,
            _ => ShellFamily::Unknown,
        }
    }
}

/// A generated startup file that re-asserts activation after the user's own
/// shell configuration has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitFile {
    /// Where to write it, relative to a Bombadil-owned temporary directory.
    pub relative_path: PathBuf,
    pub contents: String,
    /// Arguments the shell must be started with. `{file}` is replaced with the
    /// written file's absolute path by the caller.
    pub shell_args: Vec<String>,
    /// Environment the shell must be started with. `{dir}` is replaced with
    /// the containing directory's absolute path by the caller.
    pub env: BTreeMap<String, String>,
}

/// Escape a path for single-quoted POSIX shell context.
fn sq(path: &Path) -> String {
    path.to_string_lossy().replace('\'', r"'\''")
}

/// Build the startup file that puts the venv back at the front of `PATH`.
///
/// Verified on a real machine: passing `PATH=<venv>/bin:$PATH` to an
/// interactive bash leaves `<venv>/bin` at position 3, behind `~/.local/bin`,
/// because `.bashrc` prepends after we set it. Only something running after
/// the user's rc can win, so we source their configuration first and prepend
/// afterwards.
///
/// Returns `None` for a shell we do not know, because a bash rcfile handed to
/// nushell would fail on every launch — losing the re-assertion is better than
/// a syntax error every time.
pub fn init_file(family: ShellFamily, venv_bin: &Path) -> Option<InitFile> {
    let bin = sq(venv_bin);
    match family {
        ShellFamily::Bash => Some(InitFile {
            relative_path: PathBuf::from("bombadil-bashrc"),
            contents: format!(
                "# Generated by Bombadil. Sources your configuration first, then\n\
                 # re-asserts the virtual environment, which your rc may otherwise\n\
                 # push off the front of PATH.\n\
                 [ -f \"$HOME/.bash_profile\" ] && . \"$HOME/.bash_profile\"\n\
                 [ -f \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"\n\
                 export PATH='{bin}':\"$PATH\"\n"
            ),
            // NOT -l: bash silently ignores --rcfile for login shells.
            shell_args: vec!["--rcfile".into(), "{file}".into(), "-i".into()],
            env: BTreeMap::new(),
        }),
        ShellFamily::Zsh => Some(InitFile {
            relative_path: PathBuf::from(".zshrc"),
            contents: format!(
                "# Generated by Bombadil. See the bash file for why this exists.\n\
                 [ -f \"$HOME/.zshrc\" ] && . \"$HOME/.zshrc\"\n\
                 export PATH='{bin}':\"$PATH\"\n"
            ),
            shell_args: vec!["-i".into()],
            env: BTreeMap::from([("ZDOTDIR".to_string(), "{dir}".to_string())]),
        }),
        ShellFamily::Fish => Some(InitFile {
            relative_path: PathBuf::from("bombadil.fish"),
            contents: format!("fish_add_path --prepend --path '{bin}'\n"),
            shell_args: vec!["-C".into(), "source {file}".into(), "-i".into()],
            env: BTreeMap::new(),
        }),
        ShellFamily::PosixSh => Some(InitFile {
            relative_path: PathBuf::from("bombadil-env.sh"),
            contents: format!(
                "[ -f \"$HOME/.profile\" ] && . \"$HOME/.profile\"\n\
                 export PATH='{bin}':\"$PATH\"\n"
            ),
            shell_args: vec!["-i".into()],
            env: BTreeMap::from([("ENV".to_string(), "{file}".to_string())]),
        }),
        ShellFamily::Unknown => None,
    }
}
```

Add `pub mod activation;` to `terminal/mod.rs`.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace --locked
git add crates/bombadil-core/src/terminal
git commit -m "feat(terminal): re-assert activation after the user's shell rc runs

Verified on a real machine: passing PATH=<venv>/bin:\$PATH to an interactive
bash leaves the venv at position 3, behind ~/.local/bin, because .bashrc
prepends after we set it. VIRTUAL_ENV still reports the venv as active, so
the prompt looks right while python can resolve elsewhere.

bash silently ignores --rcfile for login shells, so the shell is started
explicitly as interactive non-login and the generated file sources the
user's own startup files itself."
```

---

### Task 3: Argv construction for all three operating systems

**Files:**
- Create: `crates/bombadil-core/src/terminal/argv.rs`
- Modify: `crates/bombadil-core/src/terminal/mod.rs`, `crates/bombadil-core/src/error.rs`

**Interfaces:**
- Consumes: `Os`, `TerminalKind`, `TerminalChoice`, `process::CommandSpec`.
- Produces:
  - `terminal::argv::build(choice, os, cwd, shell, shell_args) -> Result<CommandSpec, TerminalError>`
  - `error::TerminalError`

> Every case here is a pure function of its arguments, so **all three operating systems are
> tested on whichever one CI happens to be running**. That is the point of `Os` being a
> parameter — it is the only reason Windows argv is covered at all outside the Windows job.
>
> **Never pass `--wait`.** It blocks until the child exits, which for an interactive terminal
> means until the user closes the window. Bombadil's spawn must return immediately, which
> also means the exit status says nothing about whether the terminal opened.

- [ ] **Step 1: Write the failing tests**

Create `crates/bombadil-core/src/terminal/argv.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn cwd() -> &'static Path {
        Path::new("/proj/my-api")
    }

    fn built(kind: TerminalKind, os: Os) -> CommandSpec {
        build(
            &TerminalChoice::Detected(kind),
            os,
            cwd(),
            "/bin/bash",
            &["-i".to_string()],
        )
        .expect("argv must build")
    }

    #[test]
    fn gnome_terminal_gets_the_working_directory_and_the_shell_after_a_separator() {
        let spec = built(TerminalKind::GnomeTerminal, Os::Linux);
        let args = spec.args.join(" ");
        assert!(args.contains("--working-directory=/proj/my-api"), "got {args}");
        let sep = spec.args.iter().position(|a| a == "--").expect("needs --");
        let shell = spec.args.iter().position(|a| a == "/bin/bash").expect("needs shell");
        assert!(sep < shell, "the shell must follow the separator; got {args}");
    }

    #[test]
    fn no_terminal_is_ever_told_to_wait() {
        // --wait blocks until the user closes the window, which would hang the
        // GUI. Verified against gnome-terminal.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            for kind in TerminalKind::for_os(os) {
                let spec = built(kind, os);
                assert!(
                    !spec.args.iter().any(|a| a == "--wait"),
                    "{kind:?} must not wait; got {:?}",
                    spec.args
                );
            }
        }
    }

    #[test]
    fn every_terminal_on_every_os_builds_without_erroring() {
        // A missing arm would mean a detected terminal that cannot be opened.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            for kind in TerminalKind::for_os(os) {
                let spec = build(
                    &TerminalChoice::Detected(kind),
                    os,
                    cwd(),
                    "/bin/bash",
                    &[],
                )
                .unwrap_or_else(|e| panic!("{kind:?} on {os:?} failed: {e}"));
                assert!(!spec.program.as_os_str().is_empty(), "{kind:?} has no program");
            }
        }
    }

    #[test]
    fn a_terminal_from_the_wrong_os_is_refused() {
        // Offering konsole on Windows would produce an argv that cannot run.
        let err = build(
            &TerminalChoice::Detected(TerminalKind::Konsole),
            Os::Windows,
            cwd(),
            "cmd.exe",
            &[],
        )
        .unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("Konsole") || rendered.contains("konsole"), "got {rendered}");
    }

    #[test]
    fn windows_terminal_uses_its_own_directory_flag() {
        let spec = built(TerminalKind::WindowsTerminal, Os::Windows);
        let args = spec.args.join(" ");
        assert!(args.contains("-d"), "got {args}");
        assert!(args.contains("/proj/my-api"), "got {args}");
    }

    #[test]
    fn macos_terminal_app_is_launched_through_open() {
        // Terminal.app is a bundle, not a binary, so it is launched via `open`.
        let spec = built(TerminalKind::TerminalApp, Os::MacOs);
        assert_eq!(spec.program, std::path::PathBuf::from("open"));
        let args = spec.args.join(" ");
        assert!(args.contains("-a"), "got {args}");
    }

    #[test]
    fn a_custom_template_substitutes_both_placeholders() {
        let spec = build(
            &TerminalChoice::Custom {
                template: "myterm --cd {cwd} -e {shell}".into(),
            },
            Os::Linux,
            cwd(),
            "/bin/zsh",
            &[],
        )
        .unwrap();
        assert_eq!(spec.program, std::path::PathBuf::from("myterm"));
        assert_eq!(spec.args, vec!["--cd", "/proj/my-api", "-e", "/bin/zsh"]);
    }

    #[test]
    fn a_custom_template_that_is_empty_is_refused_rather_than_spawned() {
        let err = build(
            &TerminalChoice::Custom { template: "   ".into() },
            Os::Linux,
            cwd(),
            "/bin/zsh",
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty"), "got {err}");
    }

    #[test]
    fn shell_args_reach_the_spawned_shell() {
        // Task 2's --rcfile arguments are useless if they are dropped here.
        let spec = build(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            cwd(),
            "/bin/bash",
            &["--rcfile".to_string(), "/tmp/x/bombadil-bashrc".to_string(), "-i".to_string()],
        )
        .unwrap();
        let args = spec.args.join(" ");
        assert!(args.contains("--rcfile"), "got {args}");
        assert!(args.contains("/tmp/x/bombadil-bashrc"), "got {args}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core terminal::argv`
Expected: FAIL — `cannot find function build`.

- [ ] **Step 3: Implement**

Write `build` above the tests. Add to `error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("{kind:?} is not a terminal available on {os:?}")]
    WrongOs {
        kind: crate::terminal::TerminalKind,
        os: crate::terminal::Os,
    },

    #[error("the custom terminal command is empty")]
    EmptyTemplate,
}
```

`build` matches on `TerminalChoice`. For `Detected(kind)`, return `TerminalError::WrongOs` when `kind.os() != os`, then match the kind and produce a `CommandSpec` whose `cwd` is `Some(cwd.to_path_buf())` and whose args place the working directory and then the shell plus `shell_args`. Use these forms — each is the emulator's documented invocation:

| Kind | program | argv shape |
| --- | --- | --- |
| GnomeTerminal | `gnome-terminal` | `--working-directory=<cwd> -- <shell> <args…>` |
| Konsole | `konsole` | `--workdir <cwd> -e <shell> <args…>` |
| Kitty | `kitty` | `--directory <cwd> <shell> <args…>` |
| Alacritty | `alacritty` | `--working-directory <cwd> -e <shell> <args…>` |
| WezTerm | `wezterm` | `start --cwd <cwd> -- <shell> <args…>` |
| Foot | `foot` | `--working-directory=<cwd> <shell> <args…>` |
| Xterm | `xterm` | `-e <shell> <args…>` (no directory flag; rely on `spec.cwd`) |
| TerminalApp | `open` | `-a Terminal <cwd>` |
| ITerm2 | `open` | `-a iTerm <cwd>` |
| WindowsTerminal | `wt.exe` | `-d <cwd> <shell> <args…>` |
| PowerShell | `powershell.exe` | `-NoExit -Command Set-Location -LiteralPath '<cwd>'` |
| Cmd | `cmd.exe` | `/K cd /d <cwd>` |
| GitBash | `bash.exe` | `--login -i` (rely on `spec.cwd`) |
| Wsl | `wsl.exe` | `--cd <cwd>` |

Set `spec.cwd = Some(cwd.to_path_buf())` for every one — it is the fallback for the emulators with no directory flag and is harmless for the rest.

For `Custom { template }`, split on whitespace after substituting `{cwd}` and `{shell}`; the first token is the program and the rest are args. Return `EmptyTemplate` when the template has no tokens.

> **Note the macOS and Windows shell limitation.** `open -a Terminal <cwd>` cannot carry the
> shell or its arguments, and neither can `cmd /K`. Those terminals start the user's default
> shell, so Task 2's re-assertion does not reach them. Record this in the module doc as a
> known limitation for the GUI to surface; do not attempt to work around it here.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace --locked
git add crates/bombadil-core/src/terminal crates/bombadil-core/src/error.rs
git commit -m "feat(terminal): build spawn argv for all three operating systems

Os is a parameter, so every arm is covered by tests wherever CI runs -- the
only reason Windows argv is tested outside the Windows job.

No terminal is passed --wait: it blocks until the user closes the window,
which would hang the GUI, and it means the exit status cannot tell us
whether the terminal opened."
```

---

### Task 4: Detecting which terminals are actually present

**Files:**
- Create: `crates/bombadil-core/src/terminal/detect.rs`
- Modify: `crates/bombadil-core/src/terminal/mod.rs`

**Interfaces:**
- Consumes: `process::CommandRunner`, `Os`, `TerminalKind`.
- Produces: `terminal::detect::available(os, runner) -> Vec<TerminalKind>`

> Only what is found is offered (spec §8). Detection must not spawn a GUI: probe for the
> binary, never launch it.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutcome, FakeCommandRunner};

    fn found(program: &str) -> CommandOutcome {
        CommandOutcome { status: Some(0), stdout: format!("/usr/bin/{program}"), stderr: String::new() }
    }

    #[test]
    fn only_terminals_the_probe_finds_are_offered() {
        let runner = FakeCommandRunner::new();
        // `expect` already matches on the program suffix alone when
        // `args_contains` is empty -- the probe's argv carries the terminal
        // name, so match on that instead.
        runner.expect("which", &["kitty"], found("kitty"));
        let got = available(Os::Linux, &runner);
        assert_eq!(got, vec![TerminalKind::Kitty], "got {got:?}");
    }

    #[test]
    fn nothing_installed_yields_an_empty_list_rather_than_a_default() {
        // Offering a terminal that is not there produces spec section 9's
        // "chosen terminal has disappeared" at the worst possible moment.
        let runner = FakeCommandRunner::new();
        assert!(available(Os::Linux, &runner).is_empty());
    }

    #[test]
    fn detection_order_follows_the_offered_order() {
        let runner = FakeCommandRunner::new();
        runner.expect("which", &["xterm"], found("xterm"));
        runner.expect("which", &["gnome-terminal"], found("gnome-terminal"));
        assert_eq!(
            available(Os::Linux, &runner),
            vec![TerminalKind::GnomeTerminal, TerminalKind::Xterm]
        );
    }

    #[test]
    fn a_probe_that_fails_means_absent_not_an_error() {
        let runner = FakeCommandRunner::new();
        runner.expect(
            "which",
            &["konsole"],
            CommandOutcome { status: Some(1), stdout: String::new(), stderr: "not found".into() },
        );
        assert!(available(Os::Linux, &runner).is_empty());
    }

    #[test]
    fn detection_never_launches_the_terminal_itself() {
        // A probe that opened a window on startup would be unusable.
        let runner = FakeCommandRunner::new();
        runner.expect("which", &["kitty"], found("kitty"));
        let _ = available(Os::Linux, &runner);
        for call in runner.calls() {
            let prog = call.program.to_string_lossy().to_string();
            assert!(
                prog.contains("which") || prog.contains("where") || prog.contains("open"),
                "detection must probe, not launch; got {prog}"
            );
        }
    }
}
```

`FakeCommandRunner::expect(program_suffix, args_contains, outcome)` already matches the probe: the terminal's name is in the probe's argv, so `args_contains` carries it. **Do not add an `expect_program` helper and do not change `expect` — every other task depends on its current behaviour.**

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core terminal::detect`
Expected: FAIL — `cannot find function available`.

- [ ] **Step 3: Implement**

`available` iterates `TerminalKind::for_os(os)`, and for each runs the platform probe — `which <binary>` on Linux and macOS, `where <binary>` on Windows — keeping the kind when the probe exits zero. macOS bundles are probed with `open -Ra Terminal` / `open -Ra iTerm`, which tests for the app without launching it.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace --locked
git add crates/bombadil-core/src/terminal
git commit -m "feat(terminal): detect which emulators are actually installed

Only what is found is offered. Detection probes for the binary and never
launches it -- a probe that opened a window at startup would be unusable."
```

---

### Task 5: Opening a terminal

**Files:**
- Create: `crates/bombadil-core/src/terminal/open.rs`
- Modify: `crates/bombadil-core/src/terminal/mod.rs`

**Interfaces:**
- Consumes: everything above, plus `envspec::compose` and `EnvInputs`.
- Produces: `terminal::open::open(choice, os, inputs, shell, store, runner, scratch_dir) -> Result<(), EnvError>`

> **`shell` is a parameter, not read from `$SHELL` inside.** Reading the environment would
> make every test here depend on the machine running it, which is exactly the property that
> made the Windows argv untestable before `Os` became a parameter. The GUI passes `$SHELL`.

> This is the join plan 2b deliberately left open. The composed environment is handed to the
> terminal with `clear_env = true`, which discards `uv::command::project_spec`'s
> `UV_PROJECT_ENVIRONMENT` — safe only because both derive it identically from the same
> `venv_path`, which is pinned by a test on `main`.
>
> **The composed environment contains secrets.** Nothing here may log the spec, and any
> error must go through the existing redaction in `compose`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    use crate::envspec::EnvInputs;
    use crate::model::{Config, EnvVar, Project, VenvLocation};
    use crate::process::FakeCommandRunner;
    use crate::secrets::InMemorySecretStore;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn project() -> Project {
        Project {
            id: Uuid::from_u128(7),
            label: "my-api".into(),
            pyproject_path: PathBuf::from("/proj/my-api/pyproject.toml"),
            index_names: vec![],
            venv: VenvLocation::Alongside,
            env: vec![],
            scripts: vec![],
            members: Default::default(),
        }
    }

    fn inputs(project: Project) -> EnvInputs {
        EnvInputs {
            parent: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            config: Config::default(),
            project,
            venv_path: PathBuf::from("/envs/my-api"),
        }
    }

    fn opened(shell: &str, project: Project) -> Vec<crate::process::CommandSpec> {
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new();
        let tmp = tempfile::tempdir().unwrap();
        open(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            &inputs(project),
            shell,
            &store,
            &runner,
            tmp.path(),
        )
        .expect("opening must succeed");
        runner.calls()
    }

    #[test]
    fn the_terminal_receives_the_composed_environment_and_nothing_ambient() {
        // clear_env is what makes the composition authoritative; without it the
        // shell that launched Bombadil leaks in underneath.
        let p = Project {
            env: vec![EnvVar { key: "MARKER".into(), value: "set".into(), secret: false }],
            ..project()
        };
        let calls = opened("/bin/bash", p);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].clear_env, "the composition must be authoritative");
        assert_eq!(calls[0].env.get("MARKER").map(String::as_str), Some("set"));
        assert_eq!(
            calls[0].env.get("VIRTUAL_ENV").map(String::as_str),
            Some("/envs/my-api")
        );
    }

    #[test]
    fn the_working_directory_is_the_projects_directory_not_the_venvs() {
        // Spec section 8: the terminal opens where pyproject.toml lives.
        let calls = opened("/bin/bash", project());
        assert_eq!(calls[0].cwd, Some(PathBuf::from("/proj/my-api")));
    }

    #[test]
    fn a_known_shell_gets_the_reassertion_arguments() {
        // Task 2's --rcfile arguments are useless if they are dropped here:
        // activation would silently degrade to whatever the user's rc leaves.
        let calls = opened("/bin/bash", project());
        let args = calls[0].args.join(" ");
        assert!(args.contains("--rcfile"), "got {args}");
        assert!(args.contains("bombadil-bashrc"), "got {args}");
    }

    #[test]
    fn an_unknown_shell_still_opens_a_terminal() {
        // Losing the re-assertion is acceptable; refusing to open is not.
        let calls = opened("/usr/bin/nu", project());
        assert_eq!(calls.len(), 1);
        let args = calls[0].args.join(" ");
        assert!(!args.contains("--rcfile"), "got {args}");
        assert!(args.contains("/usr/bin/nu"), "got {args}");
    }

    #[test]
    fn a_terminal_that_has_disappeared_reports_the_exact_argv_attempted() {
        // Spec section 9. We cannot pass --wait, so the exit status tells us
        // nothing; a missing emulator surfaces as a spawn failure, and the
        // user needs the argv to see what was actually tried.
        let store = InMemorySecretStore::new();
        let runner = FakeCommandRunner::new(); // nothing programmed -> spawn fails
        let tmp = tempfile::tempdir().unwrap();
        let err = open(
            &TerminalChoice::Detected(TerminalKind::Kitty),
            Os::Linux,
            &inputs(project()),
            "/bin/bash",
            &store,
            &runner,
            tmp.path(),
        )
        .unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("kitty"), "must name the program; got {rendered}");
        assert!(
            rendered.contains("--directory") && rendered.contains("/proj/my-api"),
            "must name the argv, not just the program; got {rendered}"
        );
    }

    #[test]
    fn a_secret_never_appears_in_an_error_from_opening() {
        // Same constraint as compose: a failing pre-activate script must not
        // put a keychain value into a user-visible error.
        let store = InMemorySecretStore::new();
        store
            .set(
                &crate::secrets::SecretKey::ProjectEnv {
                    project_id: Uuid::from_u128(7),
                    key: "TOKEN".into(),
                },
                "s3cr3t-open-value",
            )
            .unwrap();
        let p = Project {
            env: vec![EnvVar { key: "TOKEN".into(), value: String::new(), secret: true }],
            scripts: vec![crate::model::Script {
                path: PathBuf::from("/p/pre.sh"),
                args: vec![],
                enabled: true,
            }],
            ..project()
        };
        let runner = FakeCommandRunner::new();
        runner.expect(
            crate::envspec::script::tests_shell(),
            &[],
            crate::process::CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: "+ curl -H 'Authorization: Bearer s3cr3t-open-value'".into(),
            },
        );
        let tmp = tempfile::tempdir().unwrap();
        let err = open(
            &TerminalChoice::Detected(TerminalKind::GnomeTerminal),
            Os::Linux,
            &inputs(p),
            "/bin/bash",
            &store,
            &runner,
            tmp.path(),
        )
        .unwrap_err();

        let rendered = err.to_string();
        assert!(!rendered.contains("s3cr3t-open-value"), "got {rendered}");
        assert!(rendered.contains("***"), "got {rendered}");
    }
}
```

The last test needs the platform's shell name. `envspec::script`'s tests already have a `shell()` helper; expose it for reuse as `pub(crate) fn tests_shell()` under `#[cfg(test)]`, or duplicate the two-line helper here — whichever keeps `script.rs` unchanged for other tasks. Say which you chose.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core terminal::open`
Expected: FAIL — `cannot find function open`.

- [ ] **Step 3: Implement**

`open` composes the environment, derives the shell family from the `shell` parameter, generates the init file when the family is known, writes it into `scratch_dir` substituting `{file}` and `{dir}`, builds the argv, sets `spec.env` to the composed environment with `clear_env = true`, merges the init file's `env`, and runs it. Return immediately — do not wait.

Add an `EnvError::TerminalSpawn { argv: String, message: String }` whose `#[error(...)]` renders the **whole argv**, not just the program. `CommandError::Spawn` carries only the program name, which is not enough for spec §9 — the user needs to see the flags that were tried. Build the argv string from the `CommandSpec` you just constructed, and route it through the same redaction `compose` uses, since a custom template could interpolate a value from the environment.

- [ ] **Step 4: Verify and commit**

```bash
cargo test --workspace --locked
git add crates/bombadil-core/src/terminal
git commit -m "feat(terminal): open a terminal with the composed environment

The join plan 2b left open: compose's output goes to the emulator with
clear_env set, so the composition is authoritative rather than layered on
top of whatever launched Bombadil."
```

---

### Task 6: Proving activation survives a real shell

**Files:**
- Modify: `crates/bombadil-core/tests/uv_integration.rs`

**Interfaces:** no library API.

> Every test above is hermetic. This proves the one thing they cannot: that the generated
> init file really does put the venv first **after a real shell has run the user's real
> configuration**. It needs no display and no terminal emulator — it runs the shell directly,
> which is the part that was measured to break.

- [ ] **Step 1: Write the test**

```rust
#[test]
fn the_generated_init_file_puts_the_venv_first_in_a_real_interactive_shell() {
    // Measured before this plan: without the init file an interactive bash
    // leaves the venv at position 3, behind ~/.local/bin, because .bashrc
    // prepends after us. This asserts the fix against a real bash.
    let Some(bash) = which_bash() else {
        eprintln!("no bash on this machine; skipping");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let venv_bin = tmp.path().join("envs/api/bin");
    std::fs::create_dir_all(&venv_bin).unwrap();

    let file = bombadil_core::terminal::activation::init_file(
        bombadil_core::terminal::activation::ShellFamily::Bash,
        &venv_bin,
    )
    .expect("bash is a known shell");

    let path = tmp.path().join(&file.relative_path);
    std::fs::write(&path, &file.contents).unwrap();

    let args: Vec<String> = file
        .shell_args
        .iter()
        .map(|a| a.replace("{file}", &path.to_string_lossy()))
        .collect();

    let mut spec = bombadil_core::process::CommandSpec::new(&bash);
    spec.args = args;
    spec.args.push("-c".into());
    spec.args.push("printf '%s' \"$PATH\"".into());

    let outcome = SystemCommandRunner.run(&spec).unwrap();
    assert!(outcome.success(), "bash failed: {}", outcome.stderr);

    let first = outcome.stdout.split(':').next().unwrap_or_default();
    assert_eq!(
        first,
        venv_bin.to_string_lossy(),
        "the venv must be first after the user's rc has run; got {}",
        outcome.stdout
    );
}
```

Add a `which_bash` helper following the file's existing conventions. Run the same assertion for `zsh` when it is present, using `ZDOTDIR`.

- [ ] **Step 2: Run it**

```bash
cargo test -p bombadil-core --features uv-integration --test uv_integration
```

- [ ] **Step 3: Full verification and commit**

```bash
cargo fmt --all --check \
  && cargo clippy --all-targets --all-features --locked -- -D warnings \
  && cargo test --workspace --locked \
  && cargo deny check
git add crates/bombadil-core/tests/uv_integration.rs
git commit -m "test(terminal): prove activation survives a real interactive shell

Measured before this plan: without the generated init file an interactive
bash leaves the venv at position 3, behind ~/.local/bin. This runs a real
bash with a real user configuration and asserts the venv is first."
```

---

## Done when

- `cargo test --workspace --locked` passes on Linux, macOS and Windows.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` and `cargo deny check` are clean.
- The `msrv` job at 1.88.0 passes.
- Argv for all three operating systems is covered by tests that run on every platform.
- No unit test spawns a process, opens a window, touches the keychain, or reaches the network.

## What the remaining plan picks up

**Plan 3 (the GUI)** consumes `detect::available` for the context submenu, `TerminalChoice` for the default-per-OS setting, and `open::open` for the action. It must also surface things this plan records but cannot fix.

**Six of the fourteen kinds are degraded, in two materially different ways.** Drive the warning off `TerminalKind::activation_support()`, which is the machine-readable form of this and is pinned for all fourteen kinds by a test — not off a hardcoded list in the GUI.

- **`ActivationSupport::Full`** — activation genuinely works: all seven Linux kinds (`GnomeTerminal`, `Konsole`, `Kitty`, `Alacritty`, `WezTerm`, `Foot`, `Xterm`) plus `WindowsTerminal`. No warning needed.
- **`ActivationSupport::PathOnly`** — `PowerShell`, `Cmd`, `GitBash`. The composed environment does arrive (they are spawned as ordinary child processes), but none has a flag to hand it a specific shell binary plus arguments, so the generated init file never runs and nothing re-asserts the venv after the user's rc. `Cmd` and `PowerShell` are near-fine in practice — the composed `PATH` already leads with the venv and neither has an rc that re-prepends — while `GitBash` runs `bash --login -i` and reproduces the measured demotion. Warn: "on `PATH`, but your shell configuration may push it back."
- **`ActivationSupport::None`** — `TerminalApp`, `ITerm2`, `Wsl`. **The venv is not on `PATH` at all.** `open -a Terminal <cwd>` / `open -a iTerm <cwd>` hand the request to LaunchServices, which does not propagate the calling process's environment (that is why `launchctl setenv` exists); `spec.env` and `clear_env` are delivered to the `open` binary and discarded, and only `cwd` survives because it is an argument. `wsl.exe` is the same across the WSL boundary — nothing crosses except via `WSLENV`, and a `C:\...\Scripts` path would be meaningless inside the distro anyway. Warn: "not activated." Windows users should be steered to `WindowsTerminal`.

- An unrecognised shell gets no re-assertion. The user should be told plainly rather than left with a terminal that looks activated and is not.

It must also wire `validate()`, which still has no caller — see the note carried from plan 2b.
