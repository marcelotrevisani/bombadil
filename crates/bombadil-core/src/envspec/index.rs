use crate::error::SecretError;
use crate::model::{Index, IndexAuth, IndexKind};
use crate::secrets::{SecretKey, SecretStore};
use crate::validate::index_credential_var;
use std::collections::BTreeMap;

/// The username uv expects for token authentication, the convention
/// PyPI-compatible registries use.
const TOKEN_USERNAME: &str = "__token__";

/// Inject index configuration and credentials as environment variables.
///
/// Verified against uv 0.12.1: a named index and its credentials work purely
/// from the environment, with a `pyproject.toml` that mentions no index at
/// all, and uv sends Basic auth preemptively.
///
/// The variable name comes from [`index_credential_var`] rather than being
/// rebuilt here: two copies of that rule is how the collision check in
/// `validate` drifts away from the thing it is protecting.
///
/// Known limitation: uv takes a single `UV_DEFAULT_INDEX`, so if more than
/// one index in `indexes` is [`IndexKind::Default`], the **last** one wins
/// silently -- this function is not the layer that should reject that;
/// `validate` is where the user-facing check belongs.
pub fn inject(
    indexes: &[&Index],
    store: &dyn SecretStore,
    into: &mut BTreeMap<String, String>,
) -> Result<(), SecretError> {
    let mut extras = Vec::new();

    for index in indexes {
        let pair = format!("{}={}", index.name, index.url);
        match index.kind {
            IndexKind::Default => {
                into.insert("UV_DEFAULT_INDEX".to_string(), pair);
            }
            IndexKind::Extra => extras.push(pair),
        }

        let username = match &index.auth {
            IndexAuth::None => continue,
            IndexAuth::Basic { username } => username.clone(),
            IndexAuth::Token => TOKEN_USERNAME.to_string(),
        };

        // No stored secret means the user has not filled it in. Injecting an
        // empty password would make uv attempt an auth that cannot succeed,
        // which reports worse than attempting none.
        let Some(secret) = store.get(&SecretKey::Index {
            name: index.name.clone(),
        })?
        else {
            continue;
        };

        into.insert(index_credential_var(&index.name, "USERNAME"), username);
        into.insert(index_credential_var(&index.name, "PASSWORD"), secret);
    }

    if !extras.is_empty() {
        into.insert("UV_INDEX".to_string(), extras.join(" "));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Index, IndexAuth, IndexKind};
    use crate::secrets::InMemorySecretStore;

    fn index(name: &str, kind: IndexKind, auth: IndexAuth) -> Index {
        Index {
            name: name.into(),
            url: format!("https://{name}.example/simple"),
            kind,
            default_for_new_projects: false,
            auth,
        }
    }

    #[test]
    fn a_default_index_becomes_uv_default_index() {
        let idx = index("priv", IndexKind::Default, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_DEFAULT_INDEX").map(String::as_str),
            Some("priv=https://priv.example/simple")
        );
    }

    #[test]
    fn extra_indexes_are_space_separated_in_uv_index() {
        let a = index("one", IndexKind::Extra, IndexAuth::None);
        let b = index("two", IndexKind::Extra, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&a, &b], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_INDEX").map(String::as_str),
            Some("one=https://one.example/simple two=https://two.example/simple")
        );
        assert!(!env.contains_key("UV_DEFAULT_INDEX"));
    }

    #[test]
    fn basic_auth_credentials_come_from_the_keychain() {
        let idx = index(
            "priv",
            IndexKind::Default,
            IndexAuth::Basic {
                username: "alice".into(),
            },
        );
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "priv".into(),
                },
                "hunter2",
            )
            .unwrap();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_INDEX_PRIV_USERNAME").map(String::as_str),
            Some("alice")
        );
        assert_eq!(
            env.get("UV_INDEX_PRIV_PASSWORD").map(String::as_str),
            Some("hunter2")
        );
    }

    #[test]
    fn a_hyphenated_index_name_maps_to_underscores() {
        // uv uppercases the name AND maps '-' to '_'. Verified against uv
        // 0.12.1 with a header-capturing server.
        let idx = index(
            "my-index",
            IndexKind::Extra,
            IndexAuth::Basic {
                username: "alice".into(),
            },
        );
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "my-index".into(),
                },
                "hunter2",
            )
            .unwrap();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_INDEX_MY_INDEX_USERNAME").map(String::as_str),
            Some("alice")
        );
    }

    #[test]
    fn token_auth_uses_the_conventional_token_username() {
        let idx = index("priv", IndexKind::Extra, IndexAuth::Token);
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "priv".into(),
                },
                "tok-123",
            )
            .unwrap();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_INDEX_PRIV_USERNAME").map(String::as_str),
            Some("__token__")
        );
        assert_eq!(
            env.get("UV_INDEX_PRIV_PASSWORD").map(String::as_str),
            Some("tok-123")
        );
    }

    #[test]
    fn an_index_with_no_stored_secret_injects_no_credentials() {
        // The user has not filled it in yet. Injecting an empty password would
        // make uv attempt an auth that cannot work, producing a worse error
        // than attempting none. An implementation that inserted the username
        // before consulting the store would pass a username-with-no-password
        // through -- assert both are absent, since they are inserted together
        // or not at all.
        let idx = index(
            "priv",
            IndexKind::Extra,
            IndexAuth::Basic {
                username: "alice".into(),
            },
        );
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert!(!env.contains_key("UV_INDEX_PRIV_PASSWORD"), "got {env:?}");
        assert!(!env.contains_key("UV_INDEX_PRIV_USERNAME"), "got {env:?}");
    }

    #[test]
    fn an_index_with_no_auth_injects_no_credentials() {
        let idx = index("pub", IndexKind::Extra, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&idx], &store, &mut env).unwrap();

        assert!(!env.contains_key("UV_INDEX_PUB_USERNAME"), "got {env:?}");
    }

    #[test]
    fn the_last_default_index_wins_when_two_are_marked_default() {
        // uv takes a single UV_DEFAULT_INDEX, so two defaults cannot both
        // apply. Pinned here so the collision is deliberate rather than
        // accidental; validate/ is where the user-facing check belongs.
        let a = index("one", IndexKind::Default, IndexAuth::None);
        let b = index("two", IndexKind::Default, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&a, &b], &store, &mut env).unwrap();

        assert_eq!(
            env.get("UV_DEFAULT_INDEX").map(String::as_str),
            Some("two=https://two.example/simple")
        );
    }
}
