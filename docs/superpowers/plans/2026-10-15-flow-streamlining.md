# Flow Streamlining Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use `- [ ]`.

**Goal:** Upgrades, package/project filters, pin and unsaved cues, Sync in the tile, Open folder, local-package dependencies, add project from a folder.

**Architecture:** Each feature is a pure, tested decision function next to its view, plus messages in `app.rs` routed through the existing busy/toast/stream machinery.

**Tech Stack:** Rust, iced 0.14, uv CLI (verified flags), rfd.

**Spec:** `docs/superpowers/specs/2026-10-15-flow-streamlining-design.md`

## Global Constraints
- No commits. Verification: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks && cargo test --workspace`.
- One uv command at a time (`can_start`); every uv-starting button disabled with `wait_tip` while busy.
- Never compare a `display()`ed path to a literal in tests (Windows CI).

## Review Focus
1. Filters must never change which project/package a message acts on (indices stay real; filtered-out selected project still visible).
2. Upgrade-all and add-local must refuse while busy and never run twice.
3. `relative_to` on Windows paths and different drives.
4. A dropped/picked folder already registered must not be added twice.
5. Esc order with the new dialogs (upgrade-all confirm, add-local, project chooser).

### Task 1: Upgrades
### Task 2: Package filter + Ctrl+F
### Task 3: Project filter
### Task 4: Pin cue
### Task 5: Unsaved env-var cue
### Task 6: Sync in tile + Open folder
### Task 7: Local package dependency
### Task 8: Add project from a folder (+ drop)
Each: tests named in the spec's Testing section first (RED), implement, gate.
