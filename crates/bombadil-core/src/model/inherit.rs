//! How a project says "not the global value".
//!
//! One shape for every setting that has a global default and a per-project
//! override, so a reader learns it once. The alternative -- an `Option<T>` per
//! field, meaning "inherit" by convention -- reads as "absent", and gives a
//! setting whose own value is optional nowhere to put that. [`PythonPin`] is
//! exactly such a setting: "no pin" is a value, not the absence of one.

use serde::{Deserialize, Serialize};

/// A project's answer for one setting: follow the global value, or replace it.
///
/// [`Override::Inherit`] is the default, which is what makes every field of
/// this type `#[serde(default)]`-able: a config written before the field
/// existed loads as "follow the global", never as an override the user never
/// set.
///
/// `#[derive(Default)]` adds a `T: Default` bound this type does not itself
/// need. Written by hand it would not carry that bound -- but clippy's
/// `derivable_impls` is denied in CI, and every `T` used here has a `Default`
/// anyway, so the bound costs nothing real.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Override<T> {
    #[default]
    Inherit,
    Set(T),
}

impl<T> Override<T> {
    /// The overriding value, if this is one.
    pub fn as_set(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            Self::Inherit => None,
        }
    }

    /// This override applied to `global`.
    ///
    /// Callers go through `crate::resolve::effective` rather than calling this
    /// directly wherever more than one setting is involved: one call site per
    /// setting is how the answers drift apart.
    pub fn resolve<'a>(&'a self, global: &'a T) -> &'a T {
        self.as_set().unwrap_or(global)
    }
}

/// Which interpreter a project's environment is built with.
///
/// Three states are needed, not two, and `Override<PythonPin>` is how a
/// project gets them: inherit the global pin, set its own, or explicitly
/// carry no pin at all (`Set(Unpinned)`) against a global that has one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PythonPin {
    /// Let uv choose, subject to the manifest's `requires-python`. What
    /// happened before this existed, and what every project resolves to until
    /// someone sets a pin.
    #[default]
    Unpinned,
    /// Build with this version. May be as coarse as `3.12` or as precise as
    /// `3.12.13`; see `interpreter::satisfies_pin` for how the two are
    /// compared, and why it is never string equality.
    Version(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherit_yields_the_global_value() {
        let global = "global".to_string();
        assert_eq!(Override::Inherit.resolve(&global), "global");
    }

    #[test]
    fn a_set_override_replaces_the_global_value() {
        let global = "global".to_string();
        let over = Override::Set("mine".to_string());
        assert_eq!(over.resolve(&global), "mine");
    }

    #[test]
    fn a_field_missing_from_an_older_config_reads_as_inherit() {
        // Every new field is `#[serde(default)]`, so a config written before
        // this existed must still load -- and must load as "follow the
        // global", never as an override of something the user never set.
        #[derive(serde::Deserialize)]
        struct Holder {
            #[serde(default)]
            value: Override<String>,
        }
        let holder: Holder = toml::from_str("").unwrap();
        assert_eq!(holder.value, Override::Inherit);
    }

    #[test]
    fn an_override_round_trips_through_toml() {
        // Adjacent tagging, not internal: `Set` is a newtype variant, and
        // internal tagging compiles and then fails at runtime for exactly
        // that shape -- the mistake `TerminalChoice` already documents.
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Holder {
            value: Override<String>,
        }
        let holder = Holder {
            value: Override::Set("mine".to_string()),
        };
        let text = toml::to_string(&holder).unwrap();
        assert_eq!(toml::from_str::<Holder>(&text).unwrap(), holder);
    }

    #[test]
    fn an_unpinned_python_is_not_the_same_as_inheriting_one() {
        // The three states a project needs. Without `Unpinned` as a value in
        // its own right, a project could not say "no pin" against a global
        // that has one -- `Inherit` would hand it the global's pin back.
        let global = PythonPin::Version("3.12".to_string());
        assert_eq!(
            Override::Set(PythonPin::Unpinned).resolve(&global),
            &PythonPin::Unpinned
        );
        assert_eq!(Override::<PythonPin>::Inherit.resolve(&global), &global);
    }
}
