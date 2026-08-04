//! The binary. `cargo install bombadil` is meant to be the only line anyone
//! has to read, so this crate exists to own that name and nothing else.
//!
//! It has to exist rather than being an alias: `cargo install` installs
//! binaries defined in the package being installed, never in its
//! dependencies, so a package that only *depended* on the application would
//! install nothing at all.
//!
//! Everything real lives in `bombadil-gui`, and `bombadil-core` under that.

fn main() {
    // The error is printed rather than returned so the message reaches the
    // user as a sentence instead of a `Debug` rendering, which is what
    // `fn main() -> Result` would produce.
    if let Err(error) = bombadil_gui::run() {
        eprintln!("bombadil: {error}");
        std::process::exit(1);
    }
}
