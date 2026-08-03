# Streaming uv Output Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** The drawer shows uv's output as it arrives, not after the command exits.

**Architecture:** An additive `CommandRunner::stream` alongside `run` and `spawn`, following the precedent `spawn` set — a default implementation delegating to `run`, so every existing implementation and call site is unaffected. The GUI consumes it through `Task::stream`.

**Tech Stack:** Rust edition 2024, `iced` 0.14, `futures` (already in the lockfile via iced).

## Why this is a plan and not a patch

Spec §8 says the drawer "streams live uv output". It does not: `CommandRunner::run` calls `Command::output()`, which waits for the child to exit and buffers everything. Plan 3c's drawer was built line-by-line against a seam that could only ever deliver one batch.

`CommandRunner` is the single seam every module spawns through — `uv::resolve`, `envspec::script`, `terminal::detect`, `terminal::open`, and six GUI call sites. Changing it is a core change with a blast radius, which is why it gets a plan.

## Facts verified by compiling a spike before writing this

- **`Task::stream` exists in iced 0.14** and accepts a `futures::Stream`; `futures::stream::unfold` over a `std::sync::mpsc::Receiver` compiles and delivers each item to `update` as a `Message`.
- **`futures` is already in `Cargo.lock`** via iced, so declaring it in `bombadil-gui` adds no new crate.
- **`Command::stdout(Stdio::piped()).spawn()` plus a `BufReader` line loop on a thread** reads a child's output without waiting for exit. The spike ran `sh -c 'for i in 1 2 3; do echo line $i; sleep 1; done'` and compiled clean.
- **`rx.recv()` inside the async block blocks an iced executor thread**, not the render thread — the same tradeoff `job::run` already documents and accepts.

## Global Constraints

- Rust edition 2024, `rust-version = "1.88"`. **The only new dependency declaration is `futures`**, already in the lockfile.
- **`bombadil-core` must not depend on `iced`.**
- **`stream` must be additive.** `spawn` set the precedent: a trait method with a default that delegates, so no existing implementation breaks. **Do not change `run`'s signature or behaviour** — `uv::resolve`, `detect` and `parse_*` all need its buffered output.
- **Secrets must never enter a log line, a `Debug` render, or a failure report.** A streamed line goes through the drawer's existing redaction on the way in.
- **Never block the render thread.**
- **No test may open a window, spawn a real process, touch the real keychain, or reach the network** — except the feature-gated `uv_integration` file, which may.
- Clippy lints denied in CI: `derivable_impls`, `field_reassign_with_default`.

---

### Task 1: `CommandRunner::stream` in core

**Files:** modify `crates/bombadil-core/src/process/{mod.rs,system.rs,fake.rs}`

**Interfaces:** `CommandRunner::stream(&self, spec, on_line: &mut dyn FnMut(String)) -> Result<CommandOutcome, CommandError>`

> A callback rather than a channel keeps core free of any async runtime and lets the GUI own its own transport. The default implementation runs `run`, hands the whole stdout to `on_line` a line at a time, and returns the outcome — so an implementation that has not overridden it still behaves correctly, just not incrementally.

**Criteria — each needs a mutation proof:**

1. **`SystemCommandRunner::stream` delivers lines before the child exits.** Run a command that prints, sleeps, then prints, and assert the first line arrives before the process ends. This is the whole point; a test that only checks both lines eventually arrive would pass against `run`. **This test spawns a real process, so it belongs in the feature-gated integration file**, not the unit tests.
2. **Interleaved stdout and stderr both reach the callback**, tagged so the drawer can tell them apart. uv writes progress to stderr.
3. **The returned `CommandOutcome` still carries the exit status**, so every existing failure check keeps working.
4. **`FakeCommandRunner`'s `stream` is distinguishable from `run`**, the way `blocking_calls()` distinguishes `spawn`. Without this no GUI test can prove the streaming path is taken — and that exact gap made "spawn, never run" unprovable for two plans.

---

### Task 2: The GUI consumes it

**Files:** modify `crates/bombadil-gui/src/{app.rs,drawer.rs}`, `Cargo.toml`

**Criteria — each needs a mutation proof:**

1. **A sync dispatches through `stream`, not `run`.** Assert on the fake's stream counter, not on `Task::units()` — several existing tests use `units() > 0` to mean "a job was dispatched", and it cannot tell *which*.
2. **Each line reaches the drawer as it arrives**, appended in order, still bounded to the existing cap and still dropping the oldest.
3. **Redaction happens on the way in**, per line, before the line is stored — not at render. A secret in a streamed line must never reach `drawer.lines`.
4. **A failure still shows the transcript**, and the drawer does not collapse on failure — the behaviour plan 3a had to fix once already.

> `run_sync` currently composes the environment, runs, and pushes the whole transcript in one `SyncFinished`. Streaming splits that into many `SyncLine` messages plus a terminal `SyncFinished` carrying the outcome. **`Message` derives `Debug`, so a line must be redacted before the message is constructed**, on the job thread, exactly as the batch path does now.

---

### Task 3: Prove it against a real uv

**Files:** modify `crates/bombadil-core/tests/uv_integration.rs`

Run a real `uv sync` through `stream` and assert output arrives incrementally — at least one line observed before the command completes. This is the only test that can show the feature works; everything above proves the wiring.

Skip loudly if uv is absent, following the file's existing convention: it hard-`expect`s a real uv rather than skipping silently, because the `uv-integration` feature gate already means "I have a toolchain".

---

## Done when

- `just ci` passes on Linux, macOS and Windows.
- `uv-integration` proves a line arrives before the command exits.
- `run` and `spawn` are unchanged, and every existing call site still compiles untouched.
- No secret can reach `drawer.lines`.

## What this does not do

- **No cancellation.** Stopping a running uv is a separate feature needing process-group handling; `Child::kill` orphans uv's children, which is recorded in the ledgers from plan 2c.
- **No progress parsing.** uv's progress output stays raw text.
