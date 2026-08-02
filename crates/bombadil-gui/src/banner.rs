//! The configuration-problem banner.
//!
//! `bombadil_core::validate::validate` has had no caller since plan 1. This
//! module is the wiring: turn its `Problem`s into what `app::view` shows.
//!
//! No caching, no field on `App`: [`problems`] is a pure function of
//! `Config`, called fresh by `app::view` on every render. That is what makes
//! "recomputed after a config change, not only at startup" true by
//! construction -- the same trick `detail::view`'s unconditional Sync button
//! uses -- rather than something a future edit could accidentally break by
//! forgetting to invalidate a cache.
//!
//! On secrets: `validate::validate` takes only `&Config`, never a
//! `SecretStore` -- a stored secret lives in the keychain (see
//! `bombadil_core::secrets`), not in `Config`, and `IndexAuth` (the one
//! config field that describes an index's auth) "never holds secret
//! material" by its own doc comment. So there is no value in scope, at any
//! point in `validate` or here, that a stored secret could reach a `Problem`
//! message through. The test below guards that invariant rather than
//! catching a live bug -- see the task report for the fuller reasoning.

use bombadil_core::model::Config;
use bombadil_core::store::Loaded;
use bombadil_core::validate::{self, Problem};

/// The problems found in the current config. An empty list means no banner.
pub type Problems = Vec<Problem>;

/// Problems from *loading* the config file, as opposed to problems *in* a
/// config that loaded cleanly.
///
/// `Loaded::Unreadable` and `Loaded::ReadOnly` both hand back a default
/// `Config`, so a user whose file is corrupt or was written by a newer build
/// sees an empty project list. Discarding the variant makes that
/// indistinguishable from having registered no projects at all, and the
/// reasonable conclusion for the user to draw is that their data is gone.
///
/// Unlike [`problems`] this cannot be recomputed per render -- the load
/// happened once at startup -- so `App` carries the result instead.
pub fn load_problems(loaded: &Loaded) -> Problems {
    match loaded {
        Loaded::Ok(_) | Loaded::Fresh(_) => Vec::new(),
        Loaded::ReadOnly { found, .. } => vec![Problem {
            message: format!(
                "the config file uses schema version {found}, which this build of \
                 Bombadil does not understand; it is shown read-only and will not \
                 be written back"
            ),
        }],
        Loaded::Unreadable { message, .. } => vec![Problem {
            message: format!(
                "the config file could not be read, so no projects are shown; \
                 the file has been left untouched: {message}"
            ),
        }],
    }
}

/// Recomputes every problem in `config` from scratch. Thin wrapper over
/// [`validate::validate`] -- kept as its own function so `app::view` has a
/// stable name to call and so the "no caching" claim above has one obvious
/// place to verify.
pub fn problems(config: &Config) -> Problems {
    validate::validate(config)
}

/// Renders the banner. Deliberately dumb: all the logic worth testing lives
/// in `problems`. Empty `problems` renders nothing -- a banner that showed
/// unconditionally would train users to ignore it, which is exactly what
/// criterion 1 (see the task brief) exists to catch.
pub fn view<'a>(problems: &Problems) -> iced::Element<'a, crate::app::Message> {
    if problems.is_empty() {
        return iced::widget::text("").into();
    }
    let mut list = iced::widget::column![].spacing(2);
    for problem in problems {
        list = list.push(iced::widget::text(problem.message.clone()));
    }
    iced::widget::container(list).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::{Index, IndexAuth, IndexKind};
    use bombadil_core::secrets::{InMemorySecretStore, SecretKey, SecretStore};

    fn index(name: &str) -> Index {
        Index {
            name: name.into(),
            url: "https://example.com/simple".into(),
            kind: IndexKind::Extra,
            default_for_new_projects: false,
            auth: IndexAuth::None,
        }
    }

    fn config_with_indexes(indexes: Vec<Index>) -> Config {
        Config {
            indexes,
            ..Config::default()
        }
    }

    fn messages(found: &Problems) -> String {
        found
            .iter()
            .map(|p| p.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_clean_config_has_no_problems() {
        // No problems is what makes `view` render no banner -- `view` itself
        // is untested by convention, so this is the substantive half of
        // criterion 1: a banner that always shows would still pass a test
        // that only checked `view`'s branch.
        let found = problems(&Config::default());
        assert!(found.is_empty(), "got {found:?}");
    }

    #[test]
    fn a_collision_names_both_index_names() {
        // uv maps '-' to '_' deriving the credential var, so these two
        // distinct names both become UV_INDEX_MY_INDEX_USERNAME. A message
        // naming only one would leave the user unable to tell which pair
        // collided.
        let config = config_with_indexes(vec![index("my-index"), index("my_index")]);
        let found = problems(&config);
        let joined = messages(&found);
        assert!(
            joined.contains("my-index") && joined.contains("my_index"),
            "expected a problem naming both colliding index names; got {found:?}"
        );
    }

    #[test]
    fn problems_are_recomputed_after_a_config_change() {
        // Two different config states through the *same* call, not two
        // separately-constructed configs: proves recomputation happens per
        // call rather than at some earlier point that got cached.
        let mut config = config_with_indexes(vec![index("my-index"), index("my_index")]);
        let before = problems(&config);
        assert!(
            before.iter().any(|p| p.message.contains("my_index")),
            "expected the collision problem before the fix; got {before:?}"
        );

        // Fix the collision, then introduce a distinct, unrelated problem.
        config.indexes[1].name = "other".into();
        config.indexes[0].url = "ftp://example.com/simple".into();
        let after = problems(&config);

        assert!(
            !after.iter().any(|p| p.message.contains("collide")),
            "the fixed collision must not still be reported; got {after:?}"
        );
        assert!(
            after.iter().any(|p| p.message.contains("ftp://")),
            "the newly introduced URL problem must appear; got {after:?}"
        );
    }

    #[test]
    fn an_unreadable_config_is_reported_rather_than_looking_empty() {
        // Both non-clean variants hand back a default Config, so without a
        // banner the user sees zero projects and no explanation -- and would
        // reasonably conclude their data is gone.
        let found = load_problems(&Loaded::Unreadable {
            config: Config::default(),
            message: "expected `=` at line 4".into(),
        });
        let joined = messages(&found);
        assert!(
            joined.contains("could not be read"),
            "the banner must say the config could not be read; got {found:?}"
        );
        assert!(
            joined.contains("expected `=` at line 4"),
            "the banner must carry the underlying cause; got {found:?}"
        );
    }

    #[test]
    fn a_too_new_config_is_reported_with_the_version_it_declares() {
        let found = load_problems(&Loaded::ReadOnly {
            config: Config::default(),
            found: 99,
        });
        let joined = messages(&found);
        assert!(
            joined.contains("99"),
            "the banner must name the schema version found; got {found:?}"
        );
        assert!(
            joined.contains("read-only"),
            "the banner must say the file will not be written back; got {found:?}"
        );
    }

    #[test]
    fn a_cleanly_loaded_config_produces_no_load_banner() {
        assert!(load_problems(&Loaded::Ok(Config::default())).is_empty());
        assert!(load_problems(&Loaded::Fresh(Config::default())).is_empty());
    }

    #[test]
    fn the_banner_never_contains_a_stored_secret() {
        // A distinctive stored secret for the same index a real problem
        // names -- if anything downstream of `validate` ever started
        // building messages from the keychain instead of `Config`, this is
        // the shape of leak it would produce.
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "priv".into(),
                },
                "s3cr3t-token",
            )
            .unwrap();

        let config = config_with_indexes(vec![Index {
            url: "ftp://example.com/simple".into(),
            ..index("priv")
        }]);
        let found = problems(&config);
        let joined = messages(&found);

        assert!(
            joined.contains("priv"),
            "expected a problem referencing the index by name; got {found:?}"
        );
        assert!(
            !joined.contains("s3cr3t-token"),
            "a stored secret must never appear in banner text; got {found:?}"
        );
    }
}
