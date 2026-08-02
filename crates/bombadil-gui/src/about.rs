//! The attribution surface Apache-2.0 section 4 obliges: `bombadil-uv-bin`
//! embeds a real uv binary, so a distributed build of Bombadil must carry
//! uv's name, licence and version somewhere reachable in the UI.

/// A short notice naming uv, its licence and the exact version bundled in
/// this build. Rendered as a footer line in `view` -- the point is that it
/// ships, not that it is pretty.
///
/// `bombadil_uv_bin::BUNDLED_VERSION` is only known at `bombadil-uv-bin`'s own
/// build time (its build script sets the env var `env!` reads), so it cannot
/// be spliced into a `concat!` literal here -- this crate's build never sees
/// that env var. A `OnceLock<String>` formats it once per process instead.
pub fn attribution() -> &'static str {
    use std::sync::OnceLock;
    static TEXT: OnceLock<String> = OnceLock::new();
    TEXT.get_or_init(|| {
        format!(
            "Bundles uv {}, licensed under the Apache License, Version 2.0 (Apache-2.0). See UV-LICENSE-APACHE for the full text.",
            bombadil_uv_bin::BUNDLED_VERSION
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_attribution_names_uv_and_its_licence() {
        // Apache-2.0 section 4 obliges a distributed work to carry the notice.
        // Embedding uv without this is a licence violation, not a missing nicety.
        let text = attribution();
        assert!(text.contains("uv"), "got {text}");
        assert!(
            text.contains("Apache-2.0") || text.contains("Apache License"),
            "got {text}"
        );
    }

    #[test]
    fn the_attribution_names_the_bundled_version() {
        // A notice that does not say which version was shipped is not much of
        // a notice, and the version is already a compile-time constant.
        let text = attribution();
        assert!(
            text.contains(bombadil_uv_bin::BUNDLED_VERSION),
            "must name the bundled version; got {text}"
        );
    }
}
