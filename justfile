# Bombadil task runner. `just` with no arguments lists everything.
#
# The recipes here mirror what CI actually runs, so `just ci` passing locally
# means the pull request should be green. Where a recipe differs from CI, the
# comment says why.

msrv := "1.88.0"

# List the available recipes.
default:
    @just --list --unsorted

# --- everyday ---------------------------------------------------------------

# Build the whole workspace.
build:
    cargo build --workspace

# Build optimised. The GUI is noticeably more responsive than a debug build.
build-release:
    cargo build --workspace --release

# Run the GUI.
run:
    cargo run -p bombadil-gui

# Run the GUI optimised.
run-release:
    cargo run -p bombadil-gui --release

# Run the hermetic test suite: no processes, no network, no keychain, no display.
test:
    cargo test --workspace --locked

# Run one test by name substring, e.g. `just test-one redact`.
test-one PATTERN:
    cargo test --workspace --locked -- {{PATTERN}} --nocapture

# Format everything.
fmt:
    cargo fmt --all

# --- checks CI also runs ----------------------------------------------------

# Fail if anything is unformatted. CI runs this, not `just fmt`.
fmt-check:
    cargo fmt --all --check

# Lint with warnings denied, exactly as CI does.
clippy:
    cargo clippy --all-targets --all-features --locked -- -D warnings

# Audit licences, advisories, bans and sources.
deny:
    cargo deny check

# Check against the minimum supported Rust version (needs that toolchain installed).
msrv:
    @# rustup toolchain install {{msrv}} if this fails to find it
    cargo +{{msrv}} check --workspace --locked

# Everything CI checks, in the order most likely to fail first.
ci: fmt-check clippy test deny msrv
    @echo "all clean"

# --- integration ------------------------------------------------------------

# Integration tests against a real uv, venv and keychain (needs uv on PATH + network).
test-integration:
    @# Deliberately not in `just ci`: spawns real processes and reaches the
    @# network. CI runs it as a separate job on all three platforms. The
    @# keychain test skips itself when no backend is present.
    cargo test -p bombadil-core --locked --features uv-integration --test uv_integration

# --- housekeeping -----------------------------------------------------------

# Run the pre-commit hooks over every file, as the `hygiene` CI job does.
hygiene:
    pre-commit run --all-files

# Remove build artefacts. The embedded uv blob makes target/ large.
clean:
    cargo clean

# Show what the workspace resolves to and how big the tree is.
tree:
    @cargo tree --workspace --depth 1
    @printf 'crates in tree: '
    @cargo tree --prefix none | awk '{print $1}' | sort -u | wc -l
