# Bombadil Visual Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Bombadil a visual identity that could not be mistaken for a generic desktop tool, and make the state of every environment legible at a glance.

**Architecture:** One `theme` module owning every colour, type and spacing decision. No module hardcodes a colour or a size. iced 0.14 supports this through `Theme::custom`, `Font`, and per-widget style functions.

**Tech Stack:** Rust edition 2024, `iced` 0.14, embedded font files.

---

## The direction

**Subject.** Bombadil keeps Python virtual environments for developers who juggle several projects, monorepos and private registries. Its single job is to make the state of every environment legible, and to make the dangerous operations obvious.

**Where the palette comes from.** Tolkien describes Tom Bombadil in exactly two colours — *"bright blue his jacket was, and his boots were yellow"* — and he is a keeper of a bounded domain who owns nothing he tends. That is literally what this app does: it never owns an environment, it keeps an accurate account of one. Blue is the interactive colour; yellow means *this needs you*; the ground is a dark green-cast ink rather than neutral grey, because the domain is earth, not chrome.

**Why not the obvious.** A dev tool defaults to near-black with one acid accent, or warm cream with a serif display. Both were considered and rejected: the first says "terminal", which is what the user is escaping; the second says "editorial", which has nothing to do with packaging. The blue/yellow pair is specific to this product and carries a functional load, which is the test that matters.

### Tokens

```
ink        #1B1D1A   ground; near-black with a green cast, not neutral grey
bark       #24272300 → #242723  raised surfaces (sidebar, drawer, rows)
parchment  #E8E4D9   primary text on ink; warm, never pure white
slate      #7C8279   secondary text, labels, disabled
jacket     #4A82C4   interactive: selection, focus, links, primary action
boot       #E3B23C   attention only: drift, absent, destructive confirm
moss       #4E7A57   confirmation: installed, present, success
```

**`boot` is rationed.** It appears only where the user must look: a venv that does not exist, a dependency declared but not installed, a member that syncing will remove, and the confirm button of a destructive action. If it appears anywhere else it stops meaning anything.

### Type

Two faces, both chosen for a functional reason rather than a mood.

- **Atkinson Hyperlegible** (UI, all prose and labels). Designed by the Braille Institute to maximise character disambiguation — the `l` has a tail, the `I` has serifs, `0` is slashed. This app is *read*, not skimmed, and a misread version string or path is a real failure. Licence: SIL OFL 1.1.
- **IBM Plex Mono** (all data). Every value the user compares character by character — versions, constraints, paths, environment variable keys, index URLs — is monospaced. The distinction is semantic: **prose is Atkinson, data is Plex**. Licence: SIL OFL 1.1.

Scale, in points: `display 22 / title 17 / body 13 / label 11 / data 12`. Weights: 400 body, 600 for titles and the selected sidebar row. Nothing is bold for emphasis; emphasis is colour and the glyph.

### Layout

Unchanged in structure — sidebar, detail header, tab bar, pane, drawer — because the structure is sound. What changes is rhythm: a 4pt spacing base, rows on a 28pt grid, and the sidebar fixed at 240pt so the dependency table keeps the full remaining width.

```
┌──────────────┬────────────────────────────────────────────┐
│ ● my-api     │  my-api                       3.12.4       │
│   3.12.4     │  /home/t/proj/my-api            [ Sync ]   │
│              │ ───────────────────────────────────────────│
│ ○ web        │  Dependencies  Members  Env  Scripts  …    │
│   no env     │ ───────────────────────────────────────────│
│              │  ● httpx        >=0.27      0.27.2         │
│ ◍ etl        │  ○ pytest       >=8.0       not installed  │
│   3.11.9     │  ◍ ruff         >=0.6       0.5.1          │
├──────────────┴────────────────────────────────────────────┤
│ ▸ idle                                                    │
└───────────────────────────────────────────────────────────┘
```

### Signature: the state glyph column

One fixed-width column at the left of **every** row in the application — sidebar projects, dependency rows, member rows, index rows — carrying a single glyph in one of three states:

- **● filled, `moss`** — present and agreeing: the venv exists, the package is installed at the declared constraint, the member is installed editable.
- **○ hollow, `slate`** — known but absent: registered without an environment, declared but not installed, discovered but deselected.
- **◍ ringed, `boot`** — present but not agreeing: installed at a version outside the constraint, a member syncing will remove, a venv whose Python does not satisfy `requires-python`.

The user learns this vocabulary once and it holds everywhere. It is already half-implemented — the sidebar's filled/hollow marker is the seed.

**The risk, taken deliberately:** the glyph column is the *only* symbolic vocabulary in the application. No other icons anywhere — every other affordance is a word. That will read as austere. It is justified because a tool whose entire job is reporting state should have exactly one way of reporting state, and because icons in a dense data table are the fastest route to ambiguity.

---

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`. **The only new dependencies are the two font files** (embedded bytes, no crate).
- **`bombadil-core` must not depend on `iced`.**
- **No module may hardcode a colour, a font size, or a spacing value.** Everything comes from `theme`. A task that needs a value the theme lacks adds it to the theme.
- **No test may open a window, spawn a real process, touch the real keychain, or reach the network.**
- `view` is untested by convention — so **every visual task must state plainly that its change has no mutation proof**, per the crate's own conventions in `main.rs`.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`.

## What is testable here, and what is not

Almost none of this is. `view` returns an opaque `Element` and no test can drive a renderer. So:

- **Testable, and required:** the `theme` module's pure functions — `state_glyph(state)` returning the right glyph and colour, the type scale, and any function mapping domain state to a visual state. These get mutation proofs.
- **Not testable, and must be said so:** every `view` change. Do not write a test that appears to check a visual property. **Fifteen tests on this project have been found passing for the wrong reason**; a test asserting a container exists is the sixteenth.

---

### Task 1: The theme module

**Files:** create `crates/bombadil-gui/src/theme.rs`, `crates/bombadil-gui/fonts/`; modify `main.rs`, `Cargo.toml`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_gets_a_distinct_glyph_and_colour() {
        // The glyph column is the application's only symbolic vocabulary, so
        // two states rendering identically would make it meaningless.
        let present = state_glyph(State::Present);
        let absent = state_glyph(State::Absent);
        let drift = state_glyph(State::Drifted);
        assert_ne!(present.glyph, absent.glyph);
        assert_ne!(absent.glyph, drift.glyph);
        assert_ne!(present.glyph, drift.glyph);
        assert_ne!(present.colour, absent.colour);
        assert_ne!(absent.colour, drift.colour);
        assert_ne!(present.colour, drift.colour);
    }

    #[test]
    fn only_the_drifted_state_uses_the_attention_colour() {
        // `boot` is rationed: if it appears where nothing is wrong it stops
        // meaning anything.
        assert_eq!(state_glyph(State::Drifted).colour, BOOT);
        assert_ne!(state_glyph(State::Present).colour, BOOT);
        assert_ne!(state_glyph(State::Absent).colour, BOOT);
    }

    #[test]
    fn the_type_scale_is_strictly_ordered() {
        // A scale whose steps collide gives no hierarchy at all.
        let steps = [LABEL, DATA, BODY, TITLE, DISPLAY];
        for pair in steps.windows(2) {
            assert!(pair[0] < pair[1], "{:?} must be smaller than {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn spacing_is_a_multiple_of_the_base() {
        for s in [SPACE_1, SPACE_2, SPACE_3, SPACE_4] {
            assert_eq!(s % SPACE_BASE, 0.0, "{s} is off the 4pt grid");
        }
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p bombadil-gui theme`, expect `cannot find function state_glyph`.

- [ ] **Step 3: Implement**

Define the seven colours as `iced::Color` constants, the five type sizes, the spacing scale, `State { Present, Absent, Drifted }`, `state_glyph(State) -> Glyph { glyph: char, colour: Color }`, and an `iced::Theme` built with `Theme::custom` from the palette.

Download **Atkinson Hyperlegible** and **IBM Plex Mono** (both SIL OFL 1.1) into `crates/bombadil-gui/fonts/`, embed with `include_bytes!`, register both in `main.rs` via `.font(...)`, and set the default via `.default_font(...)`.

**Add the licences.** Both fonts are OFL 1.1, which requires the licence to accompany the font. `about.rs` already carries uv's Apache-2.0 notice — extend it, and say in your report that you did. Shipping a font without its licence is the same class of problem as shipping uv without its notice.

- [ ] **Step 4: Verify and commit**

```bash
just fmt-check && just clippy && just test && just deny
git add crates/bombadil-gui Cargo.toml Cargo.lock
git commit -m "feat(gui): add the theme module, fonts and the state glyph

Palette from Tolkien's description of Tom Bombadil -- blue jacket, yellow
boots -- on a green-cast ink ground. Blue is interactive, yellow is rationed
to things needing attention, green confirms.

Atkinson Hyperlegible for prose because a misread version string is a real
failure; IBM Plex Mono for every value the user compares character by
character. Both SIL OFL 1.1, licences carried in About."
```

---

### Task 2: The sidebar and the glyph

**Files:** modify `sidebar.rs`, `app.rs`

Apply the theme to the project list, and replace the ad-hoc filled/hollow marker with `state_glyph`. Sidebar fixed at `SIDEBAR_WIDTH`. Selected row: `jacket` background, 600 weight. Each row carries label (Atkinson, body) and Python version (Plex, data, `slate`) — or the words `no env` in `slate` when absent.

**Testable:** a pure function mapping a sidebar entry to a `State`. A project with a venv is `Present`; without, `Absent`; with a venv whose Python does not satisfy `requires-python`, `Drifted`. Mutation proof required on that mapping.

**Not testable:** everything in `view`. Say so.

---

### Task 3: The detail header and tab bar

**Files:** modify `detail.rs`, `app.rs`

Header: project label at `DISPLAY`, path beneath at `DATA` in `slate`, Python version right-aligned at `DATA`. Sync as the primary action in `jacket`.

Tab bar: a single row of text labels, selected one in `parchment` with a 2pt `jacket` underline, the rest `slate`. **No boxes, no pills** — the underline is the only indicator, consistent with the no-icons rule.

**Testable:** nothing new. Say so plainly.

---

### Task 4: The data panes

**Files:** modify `deps.rs`, `members_editor.rs`, `index_editor.rs`

Every row gets the glyph column. All data columns — versions, constraints, paths, URLs — in Plex at `DATA`; all prose in Atkinson. Columns aligned on a shared grid so versions stack vertically and can be compared by eye.

Dependencies: `● installed and satisfying / ○ declared, not installed / ◍ installed outside the constraint`. The third case needs a version-satisfaction check — **reuse `interpreter.rs`'s PEP 440 comparison** rather than writing a second one; if it does not generalise, say so and do the simplest correct thing, but do not duplicate the rule.

Members: `● installed editable / ○ discovered, deselected / ◍ selected but syncing will remove it`.

**Testable:** the domain-state → `State` mapping for each pane. Mutation proofs required.

---

### Task 5: The editors, drawer and destructive states

**Files:** modify `env_editor.rs`, `scripts_editor.rs`, `settings_editor.rs`, `drawer.rs`, `context_menu.rs`, `about.rs`

Editors: keys and values in Plex, labels in Atkinson, rows on the 28pt grid. A secret's value renders as `slate` dots — never the value, and never a placeholder that could be mistaken for one.

Drawer: `bark` surface, collapsed to one status line, transcript in Plex at `DATA`. A failed command's banner uses `boot`.

**Destructive confirmations use `boot` for the confirm button and nothing else on the screen does.** Recreate venv is the only action that can destroy work; it should be the only yellow button in the application.

**Testable:** nothing new here beyond what exists. Say so.

---

## Done when

- `just ci` passes on Linux, macOS and Windows.
- No module outside `theme.rs` contains a colour, a font size or a spacing literal.
- Both font licences are carried in About alongside uv's.
- Every task's report states plainly which of its changes have no mutation proof.

## What this plan does not do

- **No animation.** iced 0.14 has the means, and a tool people keep open all day is the wrong place to spend it.
- **No dark/light switching.** One palette, executed well, beats two executed adequately — and the ground is already dark.
- **No icon set.** The glyph column is the whole symbolic vocabulary. That is the risk this plan takes.
