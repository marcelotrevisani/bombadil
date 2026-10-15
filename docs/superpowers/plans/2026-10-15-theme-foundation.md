# Theme Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Light (Parchment), dark and follow-the-system appearance, five button roles applied to every button, and Lucide icons — the theme layer the rest of the UX overhaul builds on.

**Architecture:** `theme.rs` replaces its public `const Color`s with a `Palette` struct and two values (`DARK`, `LIGHT`). Every style function reads the palette through `theme::palette(&Theme)`, keyed on iced's `extended_palette().is_dark`, so the mode is a value `App::theme()` returns — no global state. A `ThemeChoice` in core `Settings` picks System/Light/Dark; System tracks `iced::system::theme()` and `theme_changes()`.

**Tech Stack:** Rust 2024, iced 0.14 (adds the `svg` feature), serde, Lucide icons (ISC).

**Spec:** `docs/superpowers/specs/2026-10-15-theme-foundation-design.md`

## Global Constraints

- **Do not commit.** The user asked for no commits; every task ends with the full test run instead of a commit step. Leave all changes in the working tree.
- Dark palette values are unchanged: ink `#1B1D1A`, bark `#242723`, parchment `#E8E4D9`, slate `#8B9187`, jacket `#5E94D6`, jacket_hover `#7CA9E0`, boot `#E3B23C`, moss `#5F9169`, edge `#3A3E37`, scrim `#11120F` at alpha 0.8.
- Light (Parchment) values: ink `#F5F1E6`, bark `#EBE6D8`, parchment `#1B1D1A`, slate `#5C6258`, jacket `#2E66A8`, jacket_hover `#245489`, boot `#8A5F00`, moss `#3B6B46`, edge `#CFC8B6`, scrim `#1B1D1A` at alpha 0.4.
- Every text colour ≥ 4.5:1 on `ink` and `bark` in both palettes; glyphs ≥ 3.0:1; `edge` ≥ 1.2:1.
- `ThemeChoice` default is `System`; a `config.toml` without the field loads as `System`.
- `view` code stays untested by convention (`crates/bombadil-gui/src/lib.rs` module docs); logic goes in free functions that are tested.
- Never edit `deny.toml`. The `svg` feature was verified to pass `cargo deny check licenses`.
- Never compare a `display()`ed path to a literal (lib.rs module docs).
- Verification command for every task: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks && cargo test --workspace`. (`chunks_exact_to_as_chunks` is a pre-existing lint in `tray.rs`, out of scope.)

## Review Focus

1. **A colour left as a literal.** A view that still paints a hard-coded `Color` stays dark-styled in light mode. Pinned by Task 2 removing every public `const Color` except the palettes, so a missed site fails to compile; Task 2 Step 6 greps for `color!(` outside `theme.rs`.
2. **OS reports no preference (`Mode::None`).** Many Linux sessions answer nothing. Expected: keep the last known mode, start dark. Pinned in Task 3 (`system_mode_none_keeps_previous`).
3. **The user picks Light while the OS is dark, then the OS flips.** Expected: the explicit choice wins; OS changes are remembered but not applied. Pinned in Task 3 (`an_explicit_choice_ignores_the_os`).
4. **Old config file.** A `config.toml` from 0.2.2 with no `theme` key must load, not reset settings. Pinned in Task 3 (core serde test).
5. **Disabled buttons unreadable in light mode.** A disabled primary/danger draws `slate` on `bark`; must still be ≥ 4.5:1 in both palettes. Pinned in Task 5 (`every_role_is_legible_in_every_state`).

---

### Task 1: Palette struct, both palettes, palette lookup

**Files:**
- Modify: `crates/bombadil-gui/src/theme.rs` (palette section, `theme()` fn, tests module)

**Interfaces:**
- Produces:
  - `pub struct Palette { pub ink, pub bark, pub parchment, pub slate, pub jacket, pub jacket_hover, pub boot, pub moss, pub edge, pub scrim: Color }` (all `iced::Color`, `#[derive(Debug, Clone, Copy, PartialEq)]`)
  - `pub const DARK: Palette`, `pub const LIGHT: Palette`
  - `pub enum Swatch { Ink, Bark, Parchment, Slate, Jacket, JacketHover, Boot, Moss, Edge }` (`Debug, Clone, Copy, PartialEq, Eq`) and `impl Palette { pub fn get(&self, swatch: Swatch) -> Color }`
  - `pub fn palette(theme: &Theme) -> &'static Palette`
  - `pub fn theme(mode: iced::theme::Mode) -> Theme` (replaces the zero-arg `theme()`; `Mode::Light` → LIGHT, anything else → DARK)

The old `INK`…`SCRIM` consts stay for now, redefined as `DARK.ink` etc., so the crate keeps compiling. Task 2 deletes them.

- [ ] **Step 1: Write the failing tests** — replace the four palette tests in `theme.rs`'s `mod tests` (`every_text_colour_is_legible_on_every_ground_it_is_rendered_on`, `text_on_a_filled_button_is_legible`, `a_state_glyph_is_legible_as_a_graphic_on_both_grounds`, `the_hairline_is_visible_against_both_grounds`) with palette-parameterised versions, and add the lookup tests:

```rust
    const PALETTES: [(&str, &Palette); 2] = [("dark", &DARK), ("light", &LIGHT)];

    #[test]
    fn every_text_colour_is_legible_on_every_ground_in_both_palettes() {
        // `slate` sets nearly every path, version and label; it failed this
        // once already in the dark palette's first draft.
        for (mode, p) in PALETTES {
            for (name, colour) in [
                ("parchment", p.parchment),
                ("slate", p.slate),
                ("jacket", p.jacket),
                ("boot", p.boot),
                ("moss", p.moss),
            ] {
                for (ground_name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                    let ratio = contrast_ratio(colour, ground);
                    assert!(
                        ratio >= 4.5,
                        "{mode}: {name} on {ground_name} is {ratio:.2}:1, below WCAG AA (4.5:1)"
                    );
                }
            }
        }
    }

    #[test]
    fn text_on_a_filled_button_is_legible_in_both_palettes() {
        // Filled buttons paint their text `ink`: parchment on jacket is
        // under AA in both palettes.
        for (mode, p) in PALETTES {
            for (name, fill) in [("jacket", p.jacket), ("jacket_hover", p.jacket_hover), ("boot", p.boot)] {
                let ratio = contrast_ratio(p.ink, fill);
                assert!(ratio >= 4.5, "{mode}: ink on {name} is {ratio:.2}:1");
            }
            assert!(
                contrast_ratio(p.parchment, p.jacket) < 4.5,
                "{mode}: parchment on jacket clears AA now -- revisit why filled buttons use ink"
            );
        }
    }

    #[test]
    fn the_hairline_is_visible_in_both_palettes() {
        for (mode, p) in PALETTES {
            for (name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                let ratio = contrast_ratio(p.edge, ground);
                assert!(ratio >= 1.2, "{mode}: the hairline is {ratio:.2}:1 on {name}");
            }
        }
    }

    #[test]
    fn the_theme_built_for_a_mode_reads_back_as_that_palette() {
        // `palette()` keys on `is_dark`; if `theme()` ever builds a light
        // palette iced classifies as dark, every style reads the wrong colours.
        assert_eq!(palette(&theme(iced::theme::Mode::Dark)), &DARK);
        assert_eq!(palette(&theme(iced::theme::Mode::Light)), &LIGHT);
        assert_eq!(palette(&theme(iced::theme::Mode::None)), &DARK);
    }

    #[test]
    fn the_dark_palette_is_unchanged() {
        assert_eq!(DARK.ink, color!(0x1B1D1A));
        assert_eq!(DARK.parchment, color!(0xE8E4D9));
        assert_eq!(DARK.slate, color!(0x8B9187));
        assert_eq!(DARK.jacket, color!(0x5E94D6));
    }
```

Keep the glyph test as-is for now (Task 2 parameterises it once `Glyph` carries a `Swatch`).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p bombadil-gui theme::tests`
Expected: compile errors — `Palette`, `DARK`, `LIGHT`, `palette` not found; `theme` takes 0 arguments.

- [ ] **Step 3: Implement.** In `theme.rs`, replace the block from `pub const INK` through the end of `pub fn theme() -> Theme { ... }` with:

```rust
/// Every colour the interface uses, by role. Field names keep their meaning
/// across palettes: `ink` is always the ground and `parchment` always the
/// main text, so in [`LIGHT`] `ink` is the light one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The application ground.
    pub ink: Color,
    /// Raised surfaces: sidebar, top bar, drawer, rows, modal cards.
    pub bark: Color,
    /// Main text.
    pub parchment: Color,
    /// Secondary text: paths, versions, labels.
    pub slate: Color,
    /// Interactive: selection, focus, links, the primary action.
    pub jacket: Color,
    /// The primary action under the pointer.
    pub jacket_hover: Color,
    /// Rationed attention: drift and destructive confirms only.
    pub boot: Color,
    /// Healthy.
    pub moss: Color,
    /// Hairlines and outlines.
    pub edge: Color,
    /// The backdrop behind a modal.
    pub scrim: Color,
}

/// Today's palette, unchanged.
pub const DARK: Palette = Palette {
    ink: color!(0x1B1D1A),
    bark: color!(0x242723),
    parchment: color!(0xE8E4D9),
    slate: color!(0x8B9187),
    jacket: color!(0x5E94D6),
    jacket_hover: color!(0x7CA9E0),
    boot: color!(0xE3B23C),
    moss: color!(0x5F9169),
    edge: color!(0x3A3E37),
    scrim: Color {
        a: 0.8,
        ..color!(0x11120F)
    },
};

/// Parchment: the dark palette inverted -- warm paper ground, ink text --
/// with every accent darkened until it clears AA on both light grounds.
pub const LIGHT: Palette = Palette {
    ink: color!(0xF5F1E6),
    bark: color!(0xEBE6D8),
    parchment: color!(0x1B1D1A),
    slate: color!(0x5C6258),
    jacket: color!(0x2E66A8),
    jacket_hover: color!(0x245489),
    boot: color!(0x8A5F00),
    moss: color!(0x3B6B46),
    edge: color!(0xCFC8B6),
    scrim: Color {
        a: 0.4,
        ..color!(0x1B1D1A)
    },
};

/// A palette entry named without choosing a palette: what a call site holds
/// when the colour depends on the theme it renders in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swatch {
    Ink,
    Bark,
    Parchment,
    Slate,
    Jacket,
    JacketHover,
    Boot,
    Moss,
    Edge,
}

impl Palette {
    pub fn get(&self, swatch: Swatch) -> Color {
        match swatch {
            Swatch::Ink => self.ink,
            Swatch::Bark => self.bark,
            Swatch::Parchment => self.parchment,
            Swatch::Slate => self.slate,
            Swatch::Jacket => self.jacket,
            Swatch::JacketHover => self.jacket_hover,
            Swatch::Boot => self.boot,
            Swatch::Moss => self.moss,
            Swatch::Edge => self.edge,
        }
    }
}

/// The palette `theme` was built from.
///
/// Keyed on `is_dark` because there are exactly two palettes, one of each.
/// A third palette needs a real key -- the theme's name, say.
pub fn palette(theme: &Theme) -> &'static Palette {
    if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// The application's [`Theme`] for `mode`. `Mode::None` -- the OS gave no
/// answer -- is dark, which is what Bombadil looked like before this existed.
pub fn theme(mode: iced::theme::Mode) -> Theme {
    let (name, p) = match mode {
        iced::theme::Mode::Light => ("Bombadil Light", &LIGHT),
        _ => ("Bombadil", &DARK),
    };
    Theme::custom(
        name.to_string(),
        iced::theme::Palette {
            background: p.ink,
            text: p.parchment,
            primary: p.jacket,
            success: p.moss,
            // Warning and danger are both `boot`: one rationed colour for
            // "needs attention" and "about to destroy something".
            warning: p.boot,
            danger: p.boot,
        },
    )
}

// Transitional: the old names, so the crate builds until Task 2 moves every
// call site onto the theme. Deleted there.
pub const INK: Color = DARK.ink;
pub const BARK: Color = DARK.bark;
pub const PARCHMENT: Color = DARK.parchment;
pub const SLATE: Color = DARK.slate;
pub const JACKET: Color = DARK.jacket;
pub const JACKET_HOVER: Color = DARK.jacket_hover;
pub const BOOT: Color = DARK.boot;
pub const MOSS: Color = DARK.moss;
pub const EDGE: Color = DARK.edge;
pub const SCRIM: Color = DARK.scrim;
```

In `crates/bombadil-gui/src/lib.rs` change `.theme(theme::theme())` to `.theme(|_: &app::App| theme::theme(iced::theme::Mode::Dark))` (Task 3 replaces this with the real choice).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p bombadil-gui theme::tests`
Expected: PASS. If `the_theme_built_for_a_mode_reads_back_as_that_palette` fails for LIGHT, iced derives `is_dark` from the background's luminance — check `#F5F1E6` is classified light (it should be) before changing anything else.

- [ ] **Step 5: Full verification** — run the Global Constraints verification command. Expected: all green.

---

### Task 2: Move every colour onto the theme; delete the constants

**Files:**
- Modify: `crates/bombadil-gui/src/theme.rs` (style fns, `labeled_value`, `labeled_prose`, `hairline_*`, `Glyph`, `state_glyph`, tests)
- Modify (call sites): `add_project.rs`, `app.rs`, `banner.rs`, `context_menu.rs`, `deps.rs`, `detail.rs`, `drawer.rs`, `env_editor.rs`, `index_editor.rs`, `members_editor.rs`, `preferences.rs`, `scripts_editor.rs`, `settings_editor.rs`, `shell.rs`, `sidebar.rs` (all under `crates/bombadil-gui/src/`)

**Interfaces:**
- Consumes: `Palette`, `Swatch`, `palette(&Theme)` from Task 1.
- Produces:
  - `pub fn tinted(swatch: Swatch) -> impl Fn(&Theme) -> iced::widget::text::Style`
  - `pub fn text_muted(theme: &Theme) -> text::Style` (slate), `text_strong` (parchment), `text_accent` (jacket), `text_warning` (boot), `text_ok` (moss)
  - `pub struct Glyph { pub glyph: char, pub swatch: Swatch }`
  - `pub fn button_bare(resting: Swatch) -> impl Fn(&Theme, button::Status) -> button::Style`
  - `pub fn fill(swatch: Swatch) -> impl Fn(&Theme) -> container::Style` (a plain background fill, for the ad-hoc container closures)
  - The public `INK`…`SCRIM` consts no longer exist.

- [ ] **Step 1: Write the failing tests.** In `theme.rs` tests, replace `each_state_gets_a_distinct_glyph_and_colour`, `only_the_drifted_state_uses_the_attention_colour` and `a_state_glyph_is_legible_as_a_graphic_on_both_grounds` with:

```rust
    #[test]
    fn each_state_gets_a_distinct_glyph_and_swatch() {
        let all = [State::Present, State::Absent, State::Drifted].map(state_glyph);
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.glyph, b.glyph);
                assert_ne!(a.swatch, b.swatch);
            }
        }
    }

    #[test]
    fn only_the_drifted_state_uses_the_attention_colour() {
        assert_eq!(state_glyph(State::Drifted).swatch, Swatch::Boot);
        assert_ne!(state_glyph(State::Present).swatch, Swatch::Boot);
        assert_ne!(state_glyph(State::Absent).swatch, Swatch::Boot);
    }

    #[test]
    fn a_state_glyph_is_legible_as_a_graphic_in_both_palettes() {
        for (mode, p) in PALETTES {
            for state in [State::Present, State::Absent, State::Drifted] {
                for (ground_name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                    let ratio = contrast_ratio(p.get(state_glyph(state).swatch), ground);
                    assert!(ratio >= 3.0, "{mode}: the {state:?} glyph is {ratio:.2}:1 on {ground_name}");
                }
            }
        }
    }

    #[test]
    fn text_styles_follow_the_theme() {
        // The point of the whole task: the same style fn gives a different
        // colour per palette.
        let dark = theme(iced::theme::Mode::Dark);
        let light = theme(iced::theme::Mode::Light);
        assert_eq!(text_muted(&dark).color, Some(DARK.slate));
        assert_eq!(text_muted(&light).color, Some(LIGHT.slate));
        assert_eq!(tinted(Swatch::Boot)(&light).color, Some(LIGHT.boot));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p bombadil-gui theme::tests`
Expected: compile errors — `Glyph` has no field `swatch`; `text_muted`, `tinted` not found.

- [ ] **Step 3: Rewrite `theme.rs` internals.**

Add, after `palette()`:

```rust
use iced::widget::text;

/// Text in `swatch`, resolved against whichever palette is rendering.
pub fn tinted(swatch: Swatch) -> impl Fn(&Theme) -> text::Style {
    move |theme| text::Style {
        color: Some(palette(theme).get(swatch)),
    }
}

/// Secondary text: paths, versions, labels. The most used colour in the app.
pub fn text_muted(theme: &Theme) -> text::Style {
    tinted(Swatch::Slate)(theme)
}
pub fn text_strong(theme: &Theme) -> text::Style {
    tinted(Swatch::Parchment)(theme)
}
pub fn text_accent(theme: &Theme) -> text::Style {
    tinted(Swatch::Jacket)(theme)
}
pub fn text_warning(theme: &Theme) -> text::Style {
    tinted(Swatch::Boot)(theme)
}
pub fn text_ok(theme: &Theme) -> text::Style {
    tinted(Swatch::Moss)(theme)
}

/// A plain background fill in `swatch`, for a container that is only a
/// coloured block (a marker bar, a row background).
pub fn fill(swatch: Swatch) -> impl Fn(&Theme) -> iced::widget::container::Style {
    move |theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(palette(theme).get(swatch))),
        ..iced::widget::container::Style::default()
    }
}
```

Then, in every existing style fn in `theme.rs`, rename the ignored `_theme` parameter to `theme`, add `let p = palette(theme);` as the first line, and replace each constant with the field: `INK`→`p.ink`, `BARK`→`p.bark`, `PARCHMENT`→`p.parchment`, `SLATE`→`p.slate`, `JACKET`→`p.jacket`, `JACKET_HOVER`→`p.jacket_hover`, `BOOT`→`p.boot`, `EDGE`→`p.edge`. That covers `surface`, `panel`, `button_primary`, `button_quiet`, `button_choice` (its inner closure: `move |theme, status| { let p = palette(theme); button::Style { ... } }`), `button_danger`.

`hairline_row` / `hairline_column`: replace their `.style(|_theme| ...EDGE...)` with `.style(fill(Swatch::Edge))`.

`button_bare`:

```rust
pub fn button_bare(resting: Swatch) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let p = palette(theme);
        button::Style {
            background: match status {
                button::Status::Hovered | button::Status::Pressed => {
                    Some(iced::Background::Color(p.bark))
                }
                _ => None,
            },
            text_color: match status {
                button::Status::Hovered | button::Status::Pressed => p.parchment,
                button::Status::Disabled | button::Status::Active => p.get(resting),
            },
            border: iced::Border {
                radius: RADIUS.into(),
                ..iced::Border::default()
            },
            ..button::Style::default()
        }
    }
}
```

`labeled_value` / `labeled_prose`: `.color(SLATE)` → `.style(text_muted)`.

`Glyph` / `state_glyph`: field `colour: Color` becomes `swatch: Swatch`; `MOSS`→`Swatch::Moss`, `SLATE`→`Swatch::Slate`, `BOOT`→`Swatch::Boot`.

Finally delete the transitional block (`pub const INK: Color = DARK.ink;` … `SCRIM`) added in Task 1, and fix doc links that pointed at `[`INK`]` etc. to name the palette field in prose (`` `ink` ``).

- [ ] **Step 4: Migrate call sites.** Run the mechanical replacement first:

```bash
cd /home/trevisani/projetos/bombadil/crates/bombadil-gui/src
sed -i 's/\.color(theme::SLATE)/.style(theme::text_muted)/g; s/\.color(theme::PARCHMENT)/.style(theme::text_strong)/g; s/\.color(theme::BOOT)/.style(theme::text_warning)/g; s/\.color(theme::JACKET)/.style(theme::text_accent)/g; s/\.color(theme::MOSS)/.style(theme::text_ok)/g; s/\.color(glyph\.colour)/.style(theme::tinted(glyph.swatch))/g; s/theme::button_bare(theme::SLATE)/theme::button_bare(theme::Swatch::Slate)/g; s/theme::button_bare(theme::PARCHMENT)/theme::button_bare(theme::Swatch::Parchment)/g' *.rs
cargo build -p bombadil-gui 2>&1 | grep -E '^error' -A6
```

Then fix each remaining compile error by hand with these rules:
- `button_bare(if x { theme::PARCHMENT } else { theme::SLATE })` → `button_bare(if x { theme::Swatch::Parchment } else { theme::Swatch::Slate })` (`preferences.rs` section list, `shell.rs` menus).
- A container closure `|_theme| Style { background: Some(Background::Color(theme::BARK)), ..}` → `.style(theme::fill(theme::Swatch::Bark))` (`deps.rs`, `env_editor.rs`, `members_editor.rs`, `scripts_editor.rs`).
- A closure that reads a constant under a condition (`sidebar.rs` selection marker: `is_selected.then_some(Background::Color(theme::JACKET))`; selected row fill `theme::INK`) → take the theme: `move |theme| { let p = theme::palette(theme); container::Style { background: is_selected.then_some(iced::Background::Color(p.jacket)), ..Default::default() } }`.
- `theme::SCRIM` in a modal backdrop (`shell.rs`, `preferences.rs`) → `move |theme| container::Style { background: Some(Background::Color(theme::palette(theme).scrim)), ..Default::default() }`.
- `.color(...)` on a widget with a computed colour (`drawer.rs`, `banner.rs` boot status) → `.style(theme::text_warning)` or `.style(theme::tinted(swatch))` with the `Swatch` chosen where the colour was.
- Tests that compared `glyph.colour` (`sidebar.rs`, `deps.rs`, `app.rs` test modules) compare `glyph.swatch` against a `Swatch`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p bombadil-gui`
Expected: PASS.

- [ ] **Step 6: Check nothing paints a literal colour**

Run: `grep -rn 'color!(\|Color::from_rgb\|Color {' crates/bombadil-gui/src --include='*.rs' | grep -v 'src/theme.rs'`
Expected: no output. Anything listed is a colour that will not follow the mode — move it onto a `Swatch`.

- [ ] **Step 7: Full verification** — run the Global Constraints verification command. Expected: all green.

---

### Task 3: The `ThemeChoice` setting and following the system

**Files:**
- Modify: `crates/bombadil-core/src/model/settings.rs` (add `ThemeChoice`, field on `Settings`, test)
- Modify: `crates/bombadil-core/src/model/mod.rs` (re-export `ThemeChoice` alongside `Settings` — check the existing `pub use` line and add it there)
- Modify: `crates/bombadil-gui/src/app.rs` (`App.system_mode`, `Message::SystemThemeChanged`, `App::theme_mode()`, boot task, tests)
- Modify: `crates/bombadil-gui/src/lib.rs` (`.theme(...)`, subscription)

**Interfaces:**
- Consumes: `theme::theme(Mode)` from Task 1.
- Produces:
  - core: `pub enum ThemeChoice { #[default] System, Light, Dark }` with `#[serde(rename_all = "snake_case")]`; `Settings.theme: ThemeChoice` with `#[serde(default)]`.
  - gui: `App.system_mode: iced::theme::Mode` (initial `Mode::Dark`); `Message::SystemThemeChanged(iced::theme::Mode)`; `pub fn theme_mode(&self) -> iced::theme::Mode` on `App`.

- [ ] **Step 1: Write the failing core tests** in `settings.rs` (add `#[cfg(test)] mod tests` at the bottom if the file has none, `use super::*;`):

```rust
    #[test]
    fn a_config_written_before_themes_existed_loads_as_system() {
        let settings: Settings = toml::from_str("[uv_source]\nkind = \"auto\"\n").unwrap();
        assert_eq!(settings.theme, ThemeChoice::System);
    }

    #[test]
    fn the_theme_choice_round_trips_in_snake_case() {
        let settings = Settings {
            theme: ThemeChoice::Light,
            ..Settings::default()
        };
        let text = toml::to_string(&settings).unwrap();
        assert!(text.contains("theme = \"light\""), "{text}");
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p bombadil-core settings`
Expected: compile error — `ThemeChoice` not found, no field `theme`.

- [ ] **Step 3: Implement core.** In `settings.rs`, before `pub struct Settings`:

```rust
/// Light, dark, or whatever the desktop is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follow the OS, live.
    #[default]
    System,
    Light,
    Dark,
}
```

and in `Settings`:

```rust
    /// Appearance. `System` follows the desktop's light/dark preference.
    #[serde(default)]
    pub theme: ThemeChoice,
```

Re-export `ThemeChoice` wherever `Settings` is re-exported in `model/mod.rs`.

- [ ] **Step 4: Run core tests** — `cargo test -p bombadil-core settings`. Expected: PASS.

- [ ] **Step 5: Write the failing GUI tests** in `app.rs`'s `mod tests`:

```rust
    use iced::theme::Mode;

    fn with_theme(choice: bombadil_core::model::ThemeChoice) -> App {
        let mut config = Config::default();
        config.settings.theme = choice;
        App::new(config)
    }

    #[test]
    fn system_follows_what_the_os_last_reported() {
        let mut app = with_theme(bombadil_core::model::ThemeChoice::System);
        assert_eq!(app.theme_mode(), Mode::Dark, "dark until the OS answers");
        let _ = update(&mut app, Message::SystemThemeChanged(Mode::Light), unused_runner());
        assert_eq!(app.theme_mode(), Mode::Light);
    }

    #[test]
    fn an_explicit_choice_ignores_the_os() {
        let mut app = with_theme(bombadil_core::model::ThemeChoice::Light);
        let _ = update(&mut app, Message::SystemThemeChanged(Mode::Dark), unused_runner());
        assert_eq!(app.theme_mode(), Mode::Light);
        let mut app = with_theme(bombadil_core::model::ThemeChoice::Dark);
        let _ = update(&mut app, Message::SystemThemeChanged(Mode::Light), unused_runner());
        assert_eq!(app.theme_mode(), Mode::Dark);
    }

    #[test]
    fn system_mode_none_keeps_previous() {
        // Many Linux sessions report no preference at all. That is "no new
        // information", not "switch to something".
        let mut app = with_theme(bombadil_core::model::ThemeChoice::System);
        let _ = update(&mut app, Message::SystemThemeChanged(Mode::Light), unused_runner());
        let _ = update(&mut app, Message::SystemThemeChanged(Mode::None), unused_runner());
        assert_eq!(app.theme_mode(), Mode::Light);
    }

    #[test]
    fn boot_asks_the_os_for_its_theme() {
        let (_, task) = boot(Loaded::Fresh(Config::default()));
        // The probes plus the system theme query; exact count is brittle, so
        // assert it grew by one relative to an app with no projects' probes.
        assert!(task.units() >= 1);
    }
```

Before writing `boot_asks_the_os_for_its_theme`, check the `Loaded` variant name used elsewhere in `app.rs` tests (`grep -n 'Loaded::' crates/bombadil-gui/src/app.rs | head`) and use that one. If no test constructs a `Loaded`, drop this test — the subscription and boot task are wiring, covered by running the app.

- [ ] **Step 6: Run to verify failure** — `cargo test -p bombadil-gui system_follows`. Expected: compile errors.

- [ ] **Step 7: Implement GUI.**

In `App` add the field (with a doc comment) and initialise it to `iced::theme::Mode::Dark` in `App::new`:

```rust
    /// What the OS last said its appearance is. Only read when the setting
    /// is `System`. Dark until the first answer lands.
    pub system_mode: iced::theme::Mode,
```

Add the message variant:

```rust
    /// The OS reported its light/dark preference, at boot or on a change.
    SystemThemeChanged(iced::theme::Mode),
```

Handle it in `update`:

```rust
        Message::SystemThemeChanged(mode) => {
            // `None` is "no preference reported", not a preference.
            if mode != iced::theme::Mode::None {
                app.system_mode = mode;
            }
        }
```

Add to `impl App`:

```rust
    /// The mode to render in: the explicit choice, or the OS's for `System`.
    pub fn theme_mode(&self) -> iced::theme::Mode {
        use bombadil_core::model::ThemeChoice;
        match self.config.settings.theme {
            ThemeChoice::Light => iced::theme::Mode::Light,
            ThemeChoice::Dark => iced::theme::Mode::Dark,
            ThemeChoice::System => self.system_mode,
        }
    }
```

In `boot`, push `iced::system::theme().map(Message::SystemThemeChanged)` onto the `probes` vec before it is batched.

In `lib.rs`: replace the `.theme(...)` line with `.theme(|state: &app::App| theme::theme(state.theme_mode()))`, and add `iced::system::theme_changes().map(app::Message::SystemThemeChanged),` to the `Subscription::batch([...])` list.

- [ ] **Step 8: Run tests** — `cargo test -p bombadil-gui`. Expected: PASS. If a test counting `boot`'s task units fails, it now has one more unit (the theme query); update its expected count and say so in its message.

- [ ] **Step 9: Full verification** — run the Global Constraints verification command.

---

### Task 4: Appearance section in Preferences

**Files:**
- Modify: `crates/bombadil-gui/src/preferences.rs` (`Section::Appearance`, labels, `inherited_label`, `view` dispatch, `appearance_panel`, tests)
- Modify: `crates/bombadil-gui/src/app.rs` (`Message::ThemeChoiceSelected`, handler, the `Section` match near the "back to inherited" handler)
- Modify: `crates/bombadil-gui/src/shell.rs` (only if its test asserts the Settings menu length — update it)

**Interfaces:**
- Consumes: `ThemeChoice` (Task 3), `persist_settings_edit(app)` (existing in `app.rs`), `theme::button_choice` (existing).
- Produces: `Section::Appearance` (first in `Section::ALL`, which becomes `[Section; 8]`); `Message::ThemeChoiceSelected(ThemeChoice)`.

- [ ] **Step 1: Write the failing test** in `app.rs` tests:

```rust
    #[test]
    fn choosing_an_appearance_applies_and_persists_it() {
        let mut app = App::new(Config::default());
        let task = update(
            &mut app,
            Message::ThemeChoiceSelected(bombadil_core::model::ThemeChoice::Light),
            unused_runner(),
        );
        assert_eq!(app.config.settings.theme, bombadil_core::model::ThemeChoice::Light);
        assert_eq!(app.theme_mode(), iced::theme::Mode::Light);
        assert!(task.units() > 0, "the choice must be persisted, not just applied");
    }
```

and in `preferences.rs` tests:

```rust
    #[test]
    fn appearance_is_global_only_and_first() {
        assert_eq!(Section::ALL[0], Section::Appearance);
        // Nothing to inherit: there is no per-project appearance.
        assert_eq!(
            inherited_label(Section::Appearance, &Config::default(), Os::Linux),
            String::new()
        );
    }
```

(Check the existing test module's imports for `Config`/`Os` and reuse them.)

- [ ] **Step 2: Run to verify failure** — `cargo test -p bombadil-gui appearance`. Expected: compile errors.

- [ ] **Step 3: Implement.**
- `Section`: add `Appearance` as the **first** variant and make it `#[default]` instead of `Environments`; `ALL: [Section; 8]` with `Section::Appearance` first; `menu_label` → `"Appearance"`; `label` → `"Appearance"`.
- `inherited_label`: add `Section::Appearance` to the arm that returns `String::new()`.
- `view`'s panel match: `(Section::Appearance, _) => appearance_panel(config),`
- New fn in `preferences.rs`:

```rust
/// Light, dark, or the desktop's. Global only: one window, one appearance.
fn appearance_panel<'a>(config: &Config) -> iced::Element<'a, Message> {
    use bombadil_core::model::ThemeChoice;
    let current = config.settings.theme;
    let choice = |label: &'static str, value: ThemeChoice| {
        iced::widget::button(iced::widget::text(label).size(theme::BODY))
            .on_press(Message::ThemeChoiceSelected(value))
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(theme::button_choice(current == value))
    };
    iced::widget::column![
        iced::widget::text("Appearance").size(theme::TITLE),
        iced::widget::row![
            choice("System", ThemeChoice::System),
            choice("Light", ThemeChoice::Light),
            choice("Dark", ThemeChoice::Dark),
        ]
        .spacing(theme::SPACE_1),
        iced::widget::text("System follows your desktop's light or dark setting as it changes.")
            .size(theme::DATA)
            .style(theme::text_muted),
    ]
    .spacing(theme::SPACE_3)
    .into()
}
```

- In `app.rs`, add `ThemeChoiceSelected(bombadil_core::model::ThemeChoice)` to `Message` and handle:

```rust
        Message::ThemeChoiceSelected(choice) => {
            app.config.settings.theme = choice;
            return persist_settings_edit(app);
        }
```

- In the `preferences::Section` match in the reset-to-inherited handler (the one listing `Environments | Scripts | EnvVars | Indexes => return Task::none()`), add `preferences::Section::Appearance` to that arm.

- [ ] **Step 4: Run tests** — `cargo test -p bombadil-gui`. Expected: PASS. Fix any test that asserted `Section::ALL[0] == Environments` or a menu length of 7: those now expect `Appearance` / 8 — update the expectation, not the order.

- [ ] **Step 5: Full verification.**

---

### Task 5: Five button roles, every button assigned

**Files:**
- Modify: `crates/bombadil-gui/src/theme.rs` (`button_secondary`, disabled states, role tests)
- Modify: `env_editor.rs`, `members_editor.rs`, `scripts_editor.rs`, `settings_editor.rs`, `deps.rs` (under `crates/bombadil-gui/src/`)

**Interfaces:**
- Consumes: `palette()` from Task 1.
- Produces: `pub fn button_secondary(theme: &Theme, status: button::Status) -> button::Style`. All five role fns return a disabled style of `slate` text and no fill.

- [ ] **Step 1: Write the failing test** in `theme.rs` tests:

```rust
    /// The text and background a style actually paints, with an absent
    /// background meaning the `ground` it sits on.
    fn painted(style: button::Style, ground: Color) -> (Color, Color) {
        let bg = match style.background {
            Some(iced::Background::Color(c)) => c,
            _ => ground,
        };
        (style.text_color, bg)
    }

    #[test]
    fn every_role_is_legible_in_every_state() {
        use button::Status::*;
        for (mode_name, mode) in [("dark", iced::theme::Mode::Dark), ("light", iced::theme::Mode::Light)] {
            let t = theme(mode);
            let p = palette(&t);
            let roles: [(&str, Box<dyn Fn(&Theme, button::Status) -> button::Style>); 6] = [
                ("primary", Box::new(button_primary)),
                ("secondary", Box::new(button_secondary)),
                ("quiet", Box::new(button_quiet)),
                ("danger", Box::new(button_danger)),
                ("choice/on", Box::new(button_choice(true))),
                ("choice/off", Box::new(button_choice(false))),
            ];
            for (role, style) in &roles {
                for status in [Active, Hovered, Pressed, Disabled] {
                    for (ground_name, ground) in [("ink", p.ink), ("bark", p.bark)] {
                        let (fg, bg) = painted(style(&t, status), ground);
                        let ratio = contrast_ratio(fg, bg);
                        assert!(
                            ratio >= 4.5,
                            "{mode_name}: {role} {status:?} on {ground_name} is {ratio:.2}:1"
                        );
                    }
                }
            }
        }
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p bombadil-gui every_role`. Expected: compile error, `button_secondary` not found.

- [ ] **Step 3: Implement `button_secondary`** after `button_primary`:

```rust
/// Every other real action on a surface: an `edge` outline, `parchment`
/// text, a `bark` fill under the pointer. Visibly a button, unlike quiet;
/// never competing with the one primary.
pub fn button_secondary(theme: &Theme, status: button::Status) -> button::Style {
    let p = palette(theme);
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => Some(iced::Background::Color(p.bark)),
            _ => None,
        },
        text_color: match status {
            button::Status::Disabled => p.slate,
            _ => p.parchment,
        },
        border: iced::Border {
            color: if status == button::Status::Disabled { p.edge } else { p.slate },
            width: HAIRLINE,
            radius: RADIUS.into(),
        },
        ..button::Style::default()
    }
}
```

Change `button_primary` and `button_danger`'s disabled fill from `p.bark` to no background (`None`), keeping `p.slate` text, so disabled reads the same in every role. Change `button_quiet`'s border to `None`-width at rest (`width: 0.0` unless hovered/pressed) — quiet is "no border, muted until hovered" per the spec — and its resting text to `p.slate`, hovered `p.parchment`. Update its doc comment: quiet is now the toolbar/row role; outline-with-text is `button_secondary`.

- [ ] **Step 4: Run** — `cargo test -p bombadil-gui every_role`. Expected: PASS. If a state fails, fix the colour choice in that style, not the threshold.

- [ ] **Step 5: Assign every unstyled button.** Add `.style(...)` to each:

| File:approx line | Button | Role |
|---|---|---|
| `members_editor.rs:381` | Sync members | `theme::button_secondary` |
| `members_editor.rs:290` | Sync anyway | `theme::button_danger` |
| `members_editor.rs:292` | Cancel | `theme::button_quiet` |
| `scripts_editor.rs:191` | Add global script | `theme::button_secondary` |
| `scripts_editor.rs:207` | Add project script | `theme::button_secondary` |
| `scripts_editor.rs:129,131,135` | up / down / remove | `theme::button_quiet` |
| `env_editor.rs:418` | save value | `theme::button_quiet` |
| `env_editor.rs:427` | remove | `theme::button_quiet` |
| `settings_editor.rs:152` | `?` | `theme::button_quiet` |
| `settings_editor.rs:194` | validate and use | `theme::button_secondary` |
| `settings_editor.rs:235` | alongside each project | `theme::button_choice(<is alongside selected>)` — use the same condition the neighbouring "central folder" display uses |
| `settings_editor.rs:244` | choose a central folder… | `theme::button_secondary` |

And in `deps.rs`'s toolbar, the tab's **Sync** changes from `theme::button_primary` to `theme::button_secondary` (the header Sync stays the one primary on the detail surface); **Recreate venv** changes from `button_quiet` to `button_secondary`.

Then find any button still unstyled:

```bash
cd /home/trevisani/projetos/bombadil && python3 - <<'EOF'
import re,glob
for f in sorted(glob.glob('crates/bombadil-gui/src/*.rs')):
    s=open(f).read().split('#[cfg(test)]')[0]
    for m in re.finditer(r'iced::widget::button\(', s):
        stmt=s[m.start():m.start()+700].split(';')[0]
        if '.style(' not in stmt:
            print(f"{f}:{s[:m.start()].count(chr(10))+1}")
EOF
```

Expected: no output. Each line printed gets a role by the spec's rule table.

- [ ] **Step 6: Full verification.**

---

### Task 6: Lucide icons via iced `svg`

**Files:**
- Modify: `crates/bombadil-gui/Cargo.toml` (`iced` features)
- Create: `crates/bombadil-gui/src/icons.rs`
- Create: `crates/bombadil-gui/LUCIDE-LICENSE`
- Modify: `crates/bombadil-gui/src/lib.rs` (`mod icons;`)
- Modify: `crates/bombadil-gui/src/about.rs` (Lucide notice), `crates/bombadil-gui/src/shell.rs` (licences list)
- Modify: `crates/bombadil-gui/src/detail.rs` (header actions), `crates/bombadil-gui/src/shell.rs` (sponsor button), `crates/bombadil-gui/src/scripts_editor.rs`, `crates/bombadil-gui/src/env_editor.rs` (row actions), `crates/bombadil-gui/src/deps.rs` (toolbar)

**Interfaces:**
- Consumes: `palette()`, `Swatch` (Tasks 1–2), role styles (Task 5).
- Produces: `pub enum Icon { Terminal, Code, Folder, Refresh, Rotate, Trash, Plus, Pencil, X, Check, GitBranch, Alert, ChevronRight, ChevronDown, ChevronUp, Heart, Settings }`; `pub fn icon<'a, M: 'a>(which: Icon, size: f32, swatch: Swatch) -> iced::Element<'a, M>`; `pub fn icon_button<'a>(which: Icon, tip: &'static str, on_press: Message) -> iced::Element<'a, Message>` (quiet style, with tooltip).

- [ ] **Step 1: Enable the feature.** In `crates/bombadil-gui/Cargo.toml` change `iced = "0.14"` to `iced = { version = "0.14", features = ["svg"] }`. Run `cargo deny check licenses` — expected `licenses ok` (verified during design). If it fails, stop and report; do not edit `deny.toml`.

- [ ] **Step 2: Write the failing test** in the new `icons.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_parseable_svg() {
        // A typo in a path string renders as nothing at all, silently.
        for which in Icon::ALL {
            let doc = document(which);
            assert!(doc.starts_with("<svg") && doc.ends_with("</svg>"), "{which:?}");
            assert!(
                roxmltree::Document::parse(&doc).is_ok(),
                "{which:?} is not well-formed XML"
            );
        }
    }
}
```

`roxmltree` is already in the tree via the `svg` feature; add it to `[dev-dependencies]` as `roxmltree = "0.20"` (matches the locked version — no new entry in `Cargo.lock`).

- [ ] **Step 3: Run to verify failure** — `cargo test -p bombadil-gui icons`. Expected: compile error (module missing). Add `mod icons;` to `lib.rs` first.

- [ ] **Step 4: Implement `icons.rs`:**

```rust
//! Lucide icons (https://lucide.dev, ISC licence -- see `LUCIDE-LICENSE` and
//! Help > Bundled licences), copied in as path data: only the ones used.
//! Drawn in black and tinted at render time from the active palette, so they
//! follow light and dark like text does.

use crate::app::Message;
use crate::theme::{self, Swatch};
use iced::widget::svg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Terminal, Code, Folder, Refresh, Rotate, Trash, Plus, Pencil, X, Check,
    GitBranch, Alert, ChevronRight, ChevronDown, ChevronUp, Heart, Settings,
}

impl Icon {
    pub const ALL: [Icon; 17] = [
        Icon::Terminal, Icon::Code, Icon::Folder, Icon::Refresh, Icon::Rotate,
        Icon::Trash, Icon::Plus, Icon::Pencil, Icon::X, Icon::Check,
        Icon::GitBranch, Icon::Alert, Icon::ChevronRight, Icon::ChevronDown,
        Icon::ChevronUp, Icon::Heart, Icon::Settings,
    ];

    /// Lucide's path data for this icon (24x24, stroke 2).
    fn body(self) -> &'static str {
        match self {
            Icon::Terminal => r#"<polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/>"#,
            Icon::Code => r#"<polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>"#,
            Icon::Folder => r#"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"#,
            Icon::Refresh => r#"<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/>"#,
            Icon::Rotate => r#"<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/>"#,
            Icon::Trash => r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/><line x1="10" x2="10" y1="11" y2="17"/><line x1="14" x2="14" y1="11" y2="17"/>"#,
            Icon::Plus => r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#,
            Icon::Pencil => r#"<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/>"#,
            Icon::X => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
            Icon::Check => r#"<path d="M20 6 9 17l-5-5"/>"#,
            Icon::GitBranch => r#"<line x1="6" x2="6" y1="3" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/>"#,
            Icon::Alert => r#"<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>"#,
            Icon::ChevronRight => r#"<path d="m9 18 6-6-6-6"/>"#,
            Icon::ChevronDown => r#"<path d="m6 9 6 6 6-6"/>"#,
            Icon::ChevronUp => r#"<path d="m18 15-6-6-6 6"/>"#,
            Icon::Heart => r#"<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>"#,
            Icon::Settings => r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"#,
        }
    }
}

/// The full SVG document for `which`.
fn document(which: Icon) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        which.body()
    )
}

/// `which` at `size`, tinted `swatch` in whichever palette is rendering.
pub fn icon<'a, M: 'a>(which: Icon, size: f32, swatch: Swatch) -> iced::Element<'a, M> {
    svg(svg::Handle::from_memory(document(which).into_bytes()))
        .width(size)
        .height(size)
        .style(move |theme, _status| svg::Style {
            color: Some(theme::palette(theme).get(swatch)),
        })
        .into()
}

/// A quiet icon-only button. The tooltip is required: an icon alone is
/// unlabelled for anyone who does not already know it.
pub fn icon_button<'a>(which: Icon, tip: &'static str, on_press: Message) -> iced::Element<'a, Message> {
    iced::widget::tooltip(
        iced::widget::button(icon(which, 16.0, Swatch::Parchment))
            .on_press(on_press)
            .padding([theme::SPACE_1, theme::SPACE_2])
            .style(theme::button_quiet),
        iced::widget::container(iced::widget::text(tip).size(theme::LABEL))
            .padding(theme::SPACE_1)
            .style(theme::panel),
        iced::widget::tooltip::Position::Bottom,
    )
    .into()
}
```

Verify the path data against https://lucide.dev for each icon before relying on it (fetch the SVG for each name: terminal, code, folder, refresh-cw, rotate-ccw, trash-2, plus, pencil, x, check, git-branch, triangle-alert, chevron-right, chevron-down, chevron-up, heart, settings) and replace any that differ.

- [ ] **Step 5: Run** — `cargo test -p bombadil-gui icons`. Expected: PASS.

- [ ] **Step 6: Licence notice.** Create `crates/bombadil-gui/LUCIDE-LICENSE` with Lucide's ISC licence text (from https://github.com/lucide-icons/lucide/blob/main/LICENSE, verbatim). In `about.rs`, add `pub fn lucide_license() -> &'static str { include_str!("../LUCIDE-LICENSE") }` beside the existing licence fns, and in `shell.rs`'s licences list add `("Lucide icons - ISC License", about::lucide_license()),` after the font entries. If `about.rs` has a test listing every bundled licence, add Lucide to it.

- [ ] **Step 7: Use the icons.**
- `detail.rs` header: replace the `>_` button+tooltip with `icons::icon_button(Icon::Terminal, "Open a terminal here, with the environment activated", Message::OpenTerminalHereRequested(index))`; the `PC` one with `icons::icon_button(Icon::Code, "Open in PyCharm, in this folder, with this environment as the interpreter", Message::OpenPycharmRequested(index))`; Sync becomes `button(row![icons::icon(Icon::Refresh, 14.0, Swatch::Ink), text("Sync")].spacing(theme::SPACE_1).align_y(Center))` keeping `button_primary` (icon in `Ink`, the text colour on a primary fill). Delete the comments explaining the `>_`/`PC` glyph choice; replace with one line naming Lucide.
- `deps.rs` toolbar: Recreate venv and Sync get `Icon::Rotate` / `Icon::Refresh` before their labels (tinted `Swatch::Parchment`).
- `scripts_editor.rs` row actions: up → `icon_button(Icon::ChevronUp, "Move up", <its existing message>)`, down → `icon_button(Icon::ChevronDown, "Move down", <its existing message>)`, remove → `icon_button(Icon::Trash, "Remove", <its existing message>)`.
- `env_editor.rs` remove → `icon_button(Icon::Trash, "Remove", …)`; save value → `icon_button(Icon::Check, "Save value", …)`.
- `shell.rs` `sponsor_button`: prepend `icons::icon(Icon::Heart, 14.0, Swatch::Boot)` in a row with the label.

- [ ] **Step 8: Full verification**, plus `cargo deny check licenses`. Then launch the app (`just run`) and check by eye in both modes (Preferences → Appearance → Light / Dark): every icon visible, tinted, tooltips appear. State in the summary that this is visual-only verification.

---

## Self-review notes

- Spec §1 palettes → Task 1. §2 reading → Tasks 1–2. §3 setting → Tasks 3–4. §4 roles → Task 5. §5 icons → Task 6. Testing section → tests in each task. Risks → Task 2 Step 6 grep.
- `ChevronUp` was missing from the spec's icon list but the script reorder buttons need it; it is in Task 6's `Icon` from the start (17 icons).
- Names cross-checked: `palette`, `Swatch`, `tinted`, `text_muted`, `fill`, `button_bare(Swatch)`, `Glyph.swatch`, `theme_mode`, `SystemThemeChanged`, `ThemeChoiceSelected`, `Section::Appearance`, `icon`, `icon_button`.
