use crate::error::EnvError;
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
///
/// If the parent shell already exports `UV_INDEX` or `UV_DEFAULT_INDEX` and
/// `indexes` selects none of that kind, that ambient value is left untouched
/// and survives into the composed environment -- deliberate inheritance, not
/// a bug.
///
/// Two index names that normalize to the same credential variable (uv
/// uppercases the name and maps `-` to `_`, so `my-index` and `my_index`
/// collide) would otherwise have the second silently overwrite the first,
/// sending one index's password to the other's server. This is rejected
/// here, but only once credentials are actually about to be written --
/// two colliding names that inject no credentials cannot misroute anything
/// and must keep working.
pub fn inject(
    indexes: &[&Index],
    store: &dyn SecretStore,
    into: &mut BTreeMap<String, String>,
) -> Result<(), EnvError> {
    let mut extras = Vec::new();
    // Which index last claimed a given password variable, so a second index
    // normalizing to the same variable is caught before it overwrites the
    // first rather than after.
    let mut claimed_by: BTreeMap<String, String> = BTreeMap::new();

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
        let Some(secret) = store
            .get(&SecretKey::Index {
                name: index.name.clone(),
            })
            .map_err(|source| EnvError::Secret {
                message: source.to_string(),
            })?
        else {
            continue;
        };

        let password_var = index_credential_var(&index.name, "PASSWORD");
        if let Some(first) = claimed_by
            .get(&password_var)
            .filter(|first| **first != index.name)
        {
            return Err(EnvError::IndexCredentialCollision {
                first: first.clone(),
                second: index.name.clone(),
                var: password_var,
            });
        }
        claimed_by.insert(password_var.clone(), index.name.clone());

        into.insert(index_credential_var(&index.name, "USERNAME"), username);
        into.insert(password_var, secret);
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
        assert!(!env.contains_key("UV_INDEX"), "got {env:?}");
    }

    #[test]
    fn a_default_and_an_extra_index_do_not_cross_contaminate() {
        // Nothing else would catch a regression that mixed these two up --
        // e.g. an Extra index's pair landing in UV_DEFAULT_INDEX, or vice
        // versa, once both kinds are present in one call.
        let default_idx = index("alpha", IndexKind::Default, IndexAuth::None);
        let extra_idx = index("beta", IndexKind::Extra, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&default_idx, &extra_idx], &store, &mut env).unwrap();

        let default_val = env.get("UV_DEFAULT_INDEX").expect("UV_DEFAULT_INDEX");
        let extra_val = env.get("UV_INDEX").expect("UV_INDEX");
        assert!(default_val.contains("alpha"), "got {default_val}");
        assert!(!default_val.contains("beta"), "got {default_val}");
        assert!(extra_val.contains("beta"), "got {extra_val}");
        assert!(!extra_val.contains("alpha"), "got {extra_val}");
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

    #[test]
    fn colliding_index_names_with_credentials_are_rejected() {
        // uv uppercases the name and maps '-' to '_', so these two both
        // resolve to UV_INDEX_MY_INDEX_PASSWORD. Letting the second overwrite
        // the first would send one index's password to the other's server.
        let a = index(
            "my-index",
            IndexKind::Extra,
            IndexAuth::Basic {
                username: "alice".into(),
            },
        );
        let b = index(
            "my_index",
            IndexKind::Extra,
            IndexAuth::Basic {
                username: "bob".into(),
            },
        );
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::Index {
                    name: "my-index".into(),
                },
                "alice-secret",
            )
            .unwrap();
        store
            .set(
                &SecretKey::Index {
                    name: "my_index".into(),
                },
                "bob-secret",
            )
            .unwrap();
        let mut env = BTreeMap::new();

        let err = inject(&[&a, &b], &store, &mut env).unwrap_err();

        let rendered = err.to_string();
        assert!(rendered.contains("my-index"), "got {rendered}");
        assert!(rendered.contains("my_index"), "got {rendered}");
        assert!(
            rendered.contains("UV_INDEX_MY_INDEX_PASSWORD"),
            "got {rendered}"
        );
    }

    #[test]
    fn colliding_index_names_with_no_credentials_still_succeed() {
        // Two names that normalize the same cannot misroute anything unless
        // credentials are actually injected -- this must keep working.
        let a = index("my-index", IndexKind::Extra, IndexAuth::None);
        let b = index("my_index", IndexKind::Extra, IndexAuth::None);
        let store = InMemorySecretStore::new();
        let mut env = BTreeMap::new();

        inject(&[&a, &b], &store, &mut env).unwrap();

        assert!(env.contains_key("UV_INDEX"));
    }
}
