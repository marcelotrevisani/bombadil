//! Preferences > Move to another computer: export this setup to a
//! `.bombadil` file, or import one.

use crate::app::{ConfigPersist, Message};
use bombadil_core::backup::crypto::Kdf;
use bombadil_core::backup::import::{Mode, Report};
use bombadil_core::backup::mapping::FolderMap;
use bombadil_core::backup::{self, Bundle, Exported, Secrets};
use bombadil_core::model::Config;
use bombadil_core::secrets::SecretStore;
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub struct State {
    pub passphrase: String,
    pub confirm: String,
    pub import: Option<Import>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("import", &self.import)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Import {
    pub path: PathBuf,
    pub bundle: Bundle,
    pub passphrase: String,
    /// `Some` once unlocked, or straight away for a file with no secrets.
    pub secrets: Option<Secrets>,
    pub maps: Vec<FolderMap>,
    pub mode: Option<Mode>,
    pub error: Option<String>,
}

impl std::fmt::Debug for Import {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Import")
            .field("path", &self.path)
            .field("unlocked", &self.secrets.is_some())
            .field("maps", &self.maps)
            .field("mode", &self.mode)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

pub fn export_blocking(
    config: &Config,
    store: &dyn SecretStore,
    passphrase: &str,
    kdf: Kdf,
    home: &Path,
    path: &Path,
) -> Result<Exported, String> {
    let out = backup::export(config, store, passphrase, kdf, home).map_err(|e| e.to_string())?;
    std::fs::write(path, &out.text).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Saves first: a config that can't be written must not leave secrets in
/// the keychain for projects that never arrived.
pub fn import_blocking(
    current: &Config,
    incoming: Config,
    secrets: Secrets,
    mode: Mode,
    persist: &dyn ConfigPersist,
    store: &dyn SecretStore,
    exists: fn(&Path) -> bool,
) -> Result<(Config, Report, Option<PathBuf>), String> {
    let planned = backup::import::plan(current, incoming, secrets, mode, &exists);
    let copy = match mode {
        Mode::Replace => persist.backup()?,
        Mode::Merge => None,
    };
    persist.save(&planned.config)?;
    // The config is on disk now, so the app has to take it even when the
    // keychain refuses: an `Err` here would leave the window showing the old
    // list and its next save writing it back over the import.
    let report = match backup::import::write_secrets(&planned.secrets, store, mode) {
        Ok(written) => Report {
            secrets: written,
            ..planned.report
        },
        Err(e) => Report {
            secrets: 0,
            secrets_error: Some(e.to_string()),
            ..planned.report
        },
    };
    Ok((planned.config, report, copy))
}

pub fn view<'a>(state: &'a State) -> iced::Element<'a, Message> {
    use crate::index_editor::CredentialInput;
    use crate::theme;
    let button = |label: &'static str, msg: Option<Message>, primary: bool| {
        iced::widget::button(iced::widget::text(label).size(theme::BODY))
            .on_press_maybe(msg)
            .padding([theme::SPACE_1, theme::SPACE_3])
            .style(if primary {
                theme::button_primary
            } else {
                theme::button_secondary
            })
    };
    // Always shown: whether a GitHub token is in the keychain isn't known
    // without reading it, and an export that finds secrets and no
    // passphrase says so.
    let export = iced::widget::column![
        iced::widget::text("Move to another computer").size(theme::TITLE),
        iced::widget::text(
            "Export saves your projects, indexes, variables, scripts and settings to one \
             file, with the secrets encrypted by this passphrase. Needed to import the \
             secrets. Bombadil can't recover it."
        )
        .size(theme::BODY)
        .style(theme::text_muted),
        iced::widget::text_input("Passphrase", &state.passphrase)
            .secure(true)
            .on_input(|v| Message::BackupPassphraseChanged(CredentialInput(v)))
            .style(theme::text_field),
        iced::widget::text_input("Passphrase again", &state.confirm)
            .secure(true)
            .on_input(|v| Message::BackupConfirmChanged(CredentialInput(v)))
            .on_submit(Message::BackupExportRequested)
            .style(theme::text_field),
        iced::widget::row![
            button("Export\u{2026}", Some(Message::BackupExportRequested), true),
            button(
                "Import\u{2026}",
                state
                    .import
                    .is_none()
                    .then_some(Message::BackupImportRequested),
                false
            ),
        ]
        .spacing(theme::SPACE_2),
    ]
    .spacing(theme::SPACE_3);
    let Some(import) = &state.import else {
        return export.into();
    };
    let mut steps = iced::widget::column![
        theme::hairline_row(),
        iced::widget::text(format!("Importing {}", import.path.display())).size(theme::TITLE),
    ]
    .spacing(theme::SPACE_3);
    if import.secrets.is_none() {
        steps = steps.push(
            iced::widget::row![
                iced::widget::text_input("Passphrase for this file", &import.passphrase)
                    .secure(true)
                    .on_input(|v| Message::BackupUnlockChanged(CredentialInput(v)))
                    .on_submit(Message::BackupUnlockRequested)
                    .style(theme::text_field),
                button("Unlock", Some(Message::BackupUnlockRequested), true),
            ]
            .spacing(theme::SPACE_2),
        );
        if let Some(error) = &import.error {
            steps = steps.push(
                iced::widget::text(error.clone())
                    .size(theme::BODY)
                    .style(theme::text_warning),
            );
        }
    } else {
        steps = steps.push(
            iced::widget::text("Where are these folders on this computer?").size(theme::BODY),
        );
        for (row, map) in import.maps.iter().enumerate() {
            // ponytail: one stat per row per render; a handful of rows.
            let found = if map.to.exists() {
                iced::widget::text("found").style(theme::text_muted)
            } else {
                iced::widget::text("not found").style(theme::text_warning)
            };
            steps = steps.push(
                iced::widget::row![
                    iced::widget::text(map.from.clone())
                        .font(theme::FONT_DATA)
                        .size(theme::DATA)
                        .width(iced::Length::FillPortion(2)),
                    iced::widget::text_input("", &map.to.display().to_string())
                        .on_input(move |v| Message::BackupMapChanged(row, v))
                        .style(theme::text_field)
                        .width(iced::Length::FillPortion(3)),
                    found.size(theme::BODY).width(iced::Length::Fixed(72.0)),
                    button("Browse\u{2026}", Some(Message::BackupMapBrowse(row)), false),
                ]
                .spacing(theme::SPACE_2)
                .align_y(iced::Alignment::Center),
            );
        }
        let choice = |label: &'static str, mode: Mode| {
            iced::widget::button(iced::widget::text(label).size(theme::BODY))
                .on_press(Message::BackupModeSelected(mode))
                .padding([theme::SPACE_1, theme::SPACE_3])
                .style(theme::button_choice(import.mode == Some(mode)))
        };
        steps = steps
            .push(
                iced::widget::row![choice("Merge", Mode::Merge), choice("Replace", Mode::Replace)]
                    .spacing(theme::SPACE_1),
            )
            .push(
                iced::widget::text(match import.mode {
                    Some(Mode::Merge) => {
                        "Adds what isn't here yet. Your projects, settings and secrets stay."
                    }
                    Some(Mode::Replace) => {
                        "The file becomes your setup. Your current config.toml is copied aside first."
                    }
                    None => "Choose Merge or Replace.",
                })
                .size(theme::BODY)
                .style(theme::text_muted),
            );
    }
    steps = steps.push(
        iced::widget::row![
            button(
                "Import",
                (import.secrets.is_some() && import.mode.is_some())
                    .then_some(Message::BackupImportConfirmed),
                true
            ),
            button("Cancel", Some(Message::BackupImportCancelled), false),
        ]
        .spacing(theme::SPACE_2),
    );
    iced::widget::column![export, steps]
        .spacing(theme::SPACE_3)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bombadil_core::model::{EnvVar, Project};
    use bombadil_core::secrets::{InMemorySecretStore, SecretKey, SecretStore};

    fn setup() -> (Config, InMemorySecretStore) {
        let id = uuid::Uuid::new_v4();
        let config = Config {
            projects: vec![Project {
                id,
                label: "api".into(),
                pyproject_path: "/home/a/code/api/pyproject.toml".into(),
                env: vec![EnvVar {
                    key: "T".into(),
                    value: String::new(),
                    secret: true,
                }],
                ..Default::default()
            }],
            ..Config::default()
        };
        let store = InMemorySecretStore::new();
        store
            .set(
                &SecretKey::ProjectEnv {
                    project_id: id,
                    key: "T".into(),
                },
                "s3cret",
            )
            .unwrap();
        (config, store)
    }

    #[test]
    fn export_writes_the_file() {
        let (config, store) = setup();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.bombadil");
        let out = export_blocking(
            &config,
            &store,
            "correct horse",
            Kdf::FAST,
            Path::new("/home/a"),
            &path,
        )
        .unwrap();
        assert_eq!(out.secrets, 1);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[secrets]") && !text.contains("s3cret"));
    }

    #[test]
    fn import_saves_then_writes_secrets() {
        let (config, store) = setup();
        let secrets = Secrets(
            [(
                format!("env/{}/T", config.projects[0].id),
                "s3cret".to_string(),
            )]
            .into(),
        );
        let persist = RecordingPersist::default();
        let target = InMemorySecretStore::new();
        let (saved, report, backup) = import_blocking(
            &Config::default(),
            config.clone(),
            secrets,
            Mode::Merge,
            &persist,
            &target,
            |_| true,
        )
        .unwrap();
        assert_eq!(saved.projects.len(), 1);
        assert_eq!(report.secrets, 1);
        assert_eq!(backup, None, "merge makes no backup copy");
        assert_eq!(persist.saved.lock().unwrap().len(), 1);
        let key = SecretKey::ProjectEnv {
            project_id: config.projects[0].id,
            key: "T".into(),
        };
        assert_eq!(target.get(&key).unwrap().as_deref(), Some("s3cret"));
        let _ = store;
    }

    #[test]
    fn replace_backs_up_before_saving() {
        let (config, _) = setup();
        let persist = RecordingPersist {
            backup: Some("/c/config.toml.bak-1".into()),
            ..Default::default()
        };
        let (_, _, backup) = import_blocking(
            &Config::default(),
            config,
            Secrets::default(),
            Mode::Replace,
            &persist,
            &InMemorySecretStore::new(),
            |_| true,
        )
        .unwrap();
        assert_eq!(backup.as_deref(), Some(Path::new("/c/config.toml.bak-1")));
        assert_eq!(persist.order.lock().unwrap().as_slice(), ["backup", "save"]);
    }

    #[test]
    fn import_into_a_read_only_store_writes_no_secrets() {
        let (config, _) = setup();
        let secrets = Secrets(
            [(
                format!("env/{}/T", config.projects[0].id),
                "s3cret".to_string(),
            )]
            .into(),
        );
        let persist = RecordingPersist {
            refuse: true,
            ..Default::default()
        };
        let target = InMemorySecretStore::new();
        let err = import_blocking(
            &Config::default(),
            config.clone(),
            secrets,
            Mode::Merge,
            &persist,
            &target,
            |_| true,
        )
        .unwrap_err();
        assert!(err.contains("refused"), "{err}");
        let key = SecretKey::ProjectEnv {
            project_id: config.projects[0].id,
            key: "T".into(),
        };
        assert_eq!(target.get(&key).unwrap(), None);
    }

    struct NoKeychain;

    impl SecretStore for NoKeychain {
        fn get(
            &self,
            key: &SecretKey,
        ) -> Result<Option<String>, bombadil_core::error::SecretError> {
            Err(bombadil_core::error::SecretError::Backend {
                account: key.account(),
                message: "no Secret Service".into(),
            })
        }
        fn set(&self, key: &SecretKey, _: &str) -> Result<(), bombadil_core::error::SecretError> {
            self.get(key).map(|_| ())
        }
        fn delete(&self, key: &SecretKey) -> Result<(), bombadil_core::error::SecretError> {
            self.get(key).map(|_| ())
        }
        fn is_available(&self) -> bool {
            false
        }
    }

    #[test]
    fn a_keychain_failure_still_returns_the_saved_config() {
        let (config, _) = setup();
        let secrets = Secrets(
            [(
                format!("env/{}/T", config.projects[0].id),
                "s3cret".to_string(),
            )]
            .into(),
        );
        let persist = RecordingPersist::default();
        let (saved, report, _) = import_blocking(
            &Config::default(),
            config,
            secrets,
            Mode::Merge,
            &persist,
            &NoKeychain,
            |_| true,
        )
        .expect("the config was saved, so the app must take it");
        assert_eq!(saved.projects.len(), 1);
        assert_eq!(persist.saved.lock().unwrap().len(), 1);
        assert_eq!(report.secrets, 0);
        assert!(report.secrets_error.is_some());
    }

    #[derive(Default)]
    struct RecordingPersist {
        saved: std::sync::Mutex<Vec<Config>>,
        order: std::sync::Mutex<Vec<&'static str>>,
        backup: Option<PathBuf>,
        refuse: bool,
    }

    impl crate::app::ConfigPersist for RecordingPersist {
        fn save(&self, config: &Config) -> Result<(), String> {
            self.order.lock().unwrap().push("save");
            if self.refuse {
                return Err("refused: read-only".into());
            }
            self.saved.lock().unwrap().push(config.clone());
            Ok(())
        }
        fn backup(&self) -> Result<Option<PathBuf>, String> {
            self.order.lock().unwrap().push("backup");
            Ok(self.backup.clone())
        }
    }
}
