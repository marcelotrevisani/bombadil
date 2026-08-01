use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

/// A single environment variable entry.
///
/// When `secret` is true, `value` is always empty in the persisted config: the
/// real value lives in the OS keychain under `env/<project-id>/<key>` for a
/// per-project `EnvVar`, or `env/global/<key>` for one in `Config::global_env`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EnvVar {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}

// Hand-written rather than derived: this is the enforcement point for "a
// secret EnvVar never persists its value" as a security invariant, not just
// a `validate()` hint. `serde(serialize_with)` on the `value` field can't see
// `secret`, so the substitution has to happen at the struct level.
impl Serialize for EnvVar {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("EnvVar", 3)?;
        state.serialize_field("key", &self.key)?;
        if self.secret {
            state.serialize_field("value", "")?;
        } else {
            state.serialize_field("value", &self.value)?;
        }
        state.serialize_field("secret", &self.secret)?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_var_never_serializes_its_value() {
        let var = EnvVar {
            key: "TOKEN".into(),
            value: "leaked".into(),
            secret: true,
        };

        let text = toml::to_string_pretty(&var).expect("serialize");

        assert!(text.contains("value = \"\""), "got:\n{text}");
        assert!(!text.contains("leaked"), "got:\n{text}");
    }

    #[test]
    fn a_non_secret_var_round_trips_its_value_unchanged() {
        let var = EnvVar {
            key: "HTTP_PROXY".into(),
            value: "http://proxy:8080".into(),
            secret: false,
        };

        let text = toml::to_string_pretty(&var).expect("serialize");
        assert!(text.contains("http://proxy:8080"), "got:\n{text}");

        let back: EnvVar = toml::from_str(&text).expect("deserialize");
        assert_eq!(back, var);
    }
}
