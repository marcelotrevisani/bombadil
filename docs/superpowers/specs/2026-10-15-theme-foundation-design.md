# Theme foundation: light/dark/system, button roles, icons

Sub-project 1 of 5 in the UX/UI overhaul (theme foundation → feedback and
liveness → discoverability → layout and hierarchy → flow streamlining). The
later four restyle buttons, add states and add icons; this one gives them a
theme layer to do it with, so none of them does it twice.

## Intent

**Said by the user:** the user can switch between light and dark mode, or have
Bombadil follow the system. The app should feel more dynamic and easier to use.

**Agreed during design:**

- A three-way choice, **System / Light / Dark**, default **System**.
- **System** follows the OS live, with no restart.
- The light palette is **Parchment**: the dark identity inverted, warm
  off-white grounds and ink text. Not a neutral grey light theme.
- The dark palette is unchanged.
- Colours come from iced's `Theme`, not a global. **Approach 1**, chosen over
  an Orthanc-style global flag.
- Every button has one of five roles. Nothing renders in iced's default blue.
- Icons are Lucide (ISC) inline SVG via iced's `svg` feature.

**Out of scope:** tray and window icons, per-project themes, layout changes
(sub-project 4), busy/disabled behaviour wiring (sub-project 2 uses the
disabled state defined here).

**Success:**

- With **System**, flipping the desktop's dark-mode switch restyles the open
  window within a second.
- Every text colour clears WCAG AA (4.5:1) on both grounds of both palettes,
  enforced by tests.
- No button uses iced's default style.

## 1. Palettes

`theme.rs` replaces its nine `const Color`s with one struct and two values.
Field names keep their current meaning by *role*, not by literal colour:
`ink` is always the ground and `parchment` always the main text, so in the
light palette `ink` is light and `parchment` is dark.

```rust
pub struct Palette {
    pub ink: Color,          // application ground
    pub bark: Color,         // raised surfaces
    pub parchment: Color,    // main text
    pub slate: Color,        // secondary text, paths, versions
    pub jacket: Color,       // interactive accent
    pub jacket_hover: Color,
    pub boot: Color,         // attention / destructive
    pub moss: Color,         // healthy
    pub edge: Color,         // hairlines, outlines
    pub scrim: Color,        // modal backdrop
}
pub const DARK: Palette = /* today's values, unchanged */;
pub const LIGHT: Palette = /* Parchment, below */;
```

Parchment values, checked before writing this spec:

| Field | Value | Text contrast on `ink` / `bark` |
|---|---|---|
| ink | `#F5F1E6` | — |
| bark | `#EBE6D8` | — |
| parchment | `#1B1D1A` | 15.04 / 13.61 |
| slate | `#5C6258` | 5.57 / 5.04 |
| jacket | `#2E66A8` | 5.19 / 4.70 |
| boot | `#8A5F00` | 5.01 / 4.53 |
| moss | `#3B6B46` | 5.51 / 4.99 |
| edge | `#CFC8B6` | — (hairline, not text) |
| jacket_hover | `#245489` | `ink` text on it: 6.88 (darker on hover, as light themes do) |
| scrim | `#1B1D1A` at 0.4 alpha | — |

Text on filled buttons uses `ink` in both palettes: 5.19 on light `jacket`,
5.01 on light `boot`.

## 2. Reading the palette

`theme::theme(mode: Mode) -> Theme` builds `Theme::custom` from the chosen
palette, as today, but parameterised. iced's `Palette` carries only six
colours, so style functions find Bombadil's full palette with:

```rust
pub fn palette(theme: &Theme) -> &'static Palette {
    if theme.extended_palette().is_dark { &DARK } else { &LIGHT }
}
```

Every style function (`button_*`, `panel`, `surface`, …) reads colours through
`palette(theme)`. Direct colour use in views changes as follows:

- `.color(theme::SLATE)` (83 sites) becomes `.style(theme::text_muted)`.
- Other direct uses get siblings: `text_warning` (boot), `text_ok` (moss),
  `text_accent` (jacket).
- `theme::state_glyph` returns a style function rather than a `Color`.

No `const Color` stays public except the two palettes. That is what makes a
missed call site a compile error, not a dark-only patch of colour in light mode.

## 3. The setting

- `bombadil_core::model::Settings` gains
  `theme: ThemeChoice { System, Light, Dark }`, default `System`,
  `#[serde(default)]` so existing `config.toml` files load unchanged.
- `App` gains `system_mode: Mode`, with `Dark` until the OS answers.
- `App::theme()` resolves `Light`/`Dark` directly, and `System` from
  `system_mode`.
- At boot, `iced::system::theme()` is dispatched as a `Task`; its answer lands
  as `Message::SystemThemeChanged(Mode)`.
- `iced::system::theme_changes()` joins the subscription batch and feeds the
  same message. `Mode::None` (no preference; stock GNOME with its dark style
  off) reads as Light -- changed after review at the user's request.
- Preferences gains an **Appearance** section with the three choices as a
  `button_choice` group. Writing it persists through the existing
  settings-persist path.

## 4. Button roles

| Role | Style fn | Rule |
|---|---|---|
| Primary | `button_primary` | At most one per surface: the action the surface exists for. |
| Secondary | `button_secondary` (new) | Other real actions on that surface. Outline in `edge`, text in `parchment`. |
| Quiet | `button_quiet` | Toolbar and row controls; muted until hovered. |
| Danger | `button_danger` | Only the destructive confirm itself, never the button that opens it. |
| Choice | `button_choice` | Mutually exclusive options. |

`button_bare` stays as a helper for disclosure/menu text, not as a role.

Each role defines active, hovered, pressed and **disabled**. Disabled is
`slate` text on no fill, at reduced alpha; sub-project 2 uses it for "a
command is running".

The 14 unstyled buttons found by audit get explicit roles:

- **Secondary:** Sync members, Add global script, Add project script,
  validate and use, choose a central folder…
- **Danger:** Sync anyway (it removes members).
- **Quiet:** save value, remove (env), up/down/remove (scripts), the settings
  `?`.
- **Choice:** alongside each project.

The header **Sync** stays Primary. The Dependencies tab's Sync becomes
Secondary, so the detail surface keeps one Primary.

## 5. Icons

- Enable `iced`'s `svg` feature. Verified: `cargo deny check licenses` passes
  with it (adds resvg, usvg, roxmltree, rustybuzz, fontdb, kurbo,
  svgtypes, simplecss, data-url, imagesize).
- New `crates/bombadil-gui/src/icons.rs`: one private `icon(body, size)` that
  wraps a 24×24 stroke SVG and tints it through `svg::Style` from
  `palette(theme)`, plus one public fn per icon.
- The set, copied from Lucide and only what is used: terminal, code (PyCharm),
  folder, refresh-cw (Sync), rotate-ccw (Recreate), trash-2, plus, pencil, x,
  check, git-branch, triangle-alert, chevron-right, chevron-down, heart
  (Sponsor), settings.
- Lucide's ISC notice ships in **Help → Bundled licences** beside uv's and the
  fonts', from a `LUCIDE-LICENSE` file under the crate.
- Usage:
  - Header actions become icon + tooltip; `>_` and `PC` go.
  - Sync keeps its label beside its icon.
  - Row actions become quiet icon buttons.
  - Every icon-only button has a tooltip.
  - Status glyphs (● ◆ ○) stay text; they are already contrast-tested.

## Testing

- The existing contrast tests run over **both** palettes, each test
  parameterised over `[("dark", DARK), ("light", LIGHT)]`. The failure message
  names the palette.
- New: every button role's text on its fill clears 4.5:1 in both palettes,
  including `jacket_hover`.
- New: `App::theme()` resolves System through `system_mode`, and Light/Dark
  ignore it.
- New: `SystemThemeChanged(Mode::None)` reads as Light.
- New: a `config.toml` without `theme` loads as `System`.
- New: the Appearance choice persists.
- `view` stays untested by convention. Light-mode rendering is checked by eye
  and with screenshots, and that limit is stated in the PR.

## Risks

- **About 120 mechanical call-site edits across about 12 files.** Removing the
  public `const Color`s turns every missed site into a compile error, which
  is the safety net.
- **`is_dark` as the palette key.** It works because there are exactly two
  palettes, one dark and one light. A third palette needs a real key, for
  example the theme's name.
