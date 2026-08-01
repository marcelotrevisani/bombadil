/// A uv release version.
///
/// Field order is load-bearing for the derived `Ord`: comparison walks fields
/// in declaration order, which gives major-then-minor-then-patch precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UvVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// The uv shipped inside our binary.
pub const BUNDLED_UV_VERSION: UvVersion = UvVersion {
    major: 0,
    minor: 12,
    patch: 1,
};

/// The oldest uv whose CLI surface has been verified against this code.
///
/// Lower it only after explicitly re-testing an older version; raise it
/// whenever Bombadil adopts a flag a newer uv introduced. Backwards
/// compatibility is never assumed.
pub const MIN_UV_VERSION: UvVersion = UvVersion {
    major: 0,
    minor: 12,
    patch: 1,
};

impl UvVersion {
    /// Parse a bare `0.12.1` or a full `uv 0.12.1 (target-triple)` line.
    pub fn parse(text: &str) -> Option<Self> {
        // Take the first whitespace-separated token that looks like a version,
        // so this works whether or not the `uv ` prefix is present.
        text.split_whitespace().find_map(Self::parse_token)
    }

    fn parse_token(token: &str) -> Option<Self> {
        let mut parts = token.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        // Trim anything trailing the patch number, such as a `+local` suffix.
        let patch_text = parts.next()?;
        let digits: String = patch_text
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let patch = digits.parse().ok()?;
        // A fourth component means this is not a uv version.
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for UvVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bare_version() {
        assert_eq!(
            UvVersion::parse("0.12.1"),
            Some(UvVersion {
                major: 0,
                minor: 12,
                patch: 1
            })
        );
    }

    #[test]
    fn parses_the_full_uv_version_line() {
        // Exactly what `uv --version` prints.
        assert_eq!(
            UvVersion::parse("uv 0.12.1 (x86_64-unknown-linux-gnu)"),
            Some(UvVersion {
                major: 0,
                minor: 12,
                patch: 1
            })
        );
    }

    #[test]
    fn tolerates_surrounding_whitespace_and_newlines() {
        assert_eq!(
            UvVersion::parse("  uv 0.12.1 (abc)\n"),
            Some(UvVersion {
                major: 0,
                minor: 12,
                patch: 1
            })
        );
    }

    #[test]
    fn rejects_text_with_no_version() {
        assert_eq!(UvVersion::parse("uv"), None);
        assert_eq!(UvVersion::parse(""), None);
        assert_eq!(UvVersion::parse("not a version"), None);
    }

    #[test]
    fn rejects_a_two_component_version() {
        // uv always prints three components; two means we are parsing
        // something else and should not guess a patch level.
        assert_eq!(UvVersion::parse("0.12"), None);
    }

    #[test]
    fn orders_by_major_then_minor_then_patch() {
        let v = |m, n, p| UvVersion {
            major: m,
            minor: n,
            patch: p,
        };
        assert!(v(0, 12, 1) > v(0, 12, 0));
        assert!(v(0, 12, 0) > v(0, 9, 99));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert_eq!(v(0, 12, 1), v(0, 12, 1));
    }

    #[test]
    fn the_bundled_version_satisfies_the_minimum() {
        // If this ever fails, the bundled uv would be rejected by our own
        // version gate and the app would fall back to nothing.
        assert!(BUNDLED_UV_VERSION >= MIN_UV_VERSION);
    }

    #[test]
    fn displays_in_the_form_uv_prints() {
        assert_eq!(
            UvVersion {
                major: 0,
                minor: 12,
                patch: 1
            }
            .to_string(),
            "0.12.1"
        );
    }
}
