//! What an installed package says about itself: its METADATA file.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageInfo {
    pub summary: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
}

/// The header block only: METADATA's body (after the first blank line) is
/// the long description, never fields.
pub fn parse(text: &str) -> PackageInfo {
    let mut info = PackageInfo::default();
    let mut project_home = None;
    let mut license_line = None;
    let mut classifier = None;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(": ") else {
            continue;
        };
        let value = value.trim().to_string();
        // A continuation line (indented) means a multi-line field.
        let continues = lines
            .peek()
            .is_some_and(|next| next.starts_with(' ') || next.starts_with('\t'));
        match key {
            "Summary" => info.summary = Some(value),
            "Home-page" => info.homepage = Some(value),
            "License-Expression" => info.license = Some(value),
            "License" if !continues && value.len() <= 40 => license_line = Some(value),
            "Project-URL" => {
                if let Some((label, url)) = value.split_once(", ")
                    && matches!(
                        label.to_ascii_lowercase().as_str(),
                        "homepage" | "home" | "source"
                    )
                    && project_home.is_none()
                {
                    project_home = Some(url.trim().to_string());
                }
            }
            "Classifier" if value.starts_with("License ::") => {
                classifier = value.rsplit(" :: ").next().map(str::to_string);
            }
            _ => {}
        }
    }
    info.homepage = info.homepage.or(project_home);
    info.license = info.license.or(license_line).or(classifier);
    info
}

pub fn site_packages(venv: &Path) -> Option<PathBuf> {
    let windows = venv.join("Lib").join("site-packages");
    if windows.is_dir() {
        return Some(windows);
    }
    std::fs::read_dir(venv.join("lib"))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join("site-packages"))
        .find(|p| p.is_dir())
}

fn canonical(name: &str) -> String {
    name.to_lowercase().replace(['_', '.'], "-")
}

pub fn read(venv: &Path, name: &str, version: &str) -> Option<PackageInfo> {
    let want = canonical(name);
    let dir = std::fs::read_dir(site_packages(venv)?)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            let Some(stem) = p
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".dist-info"))
            else {
                return false;
            };
            stem.rsplit_once('-')
                .is_some_and(|(n, v)| canonical(n) == want && v == version)
        })?;
    std::fs::read_to_string(dir.join("METADATA"))
        .ok()
        .map(|t| parse(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_fields_with_fallbacks() {
        let text = "Metadata-Version: 2.4\nName: Jinja2\nSummary: A very fast template engine.\nProject-URL: Changes, https://x/changes\nProject-URL: Source, https://github.com/pallets/jinja/\nClassifier: License :: OSI Approved :: BSD License\n\nLong description\nLicense: not a header\n";
        let info = parse(text);
        assert_eq!(
            info.summary.as_deref(),
            Some("A very fast template engine.")
        );
        assert_eq!(
            info.homepage.as_deref(),
            Some("https://github.com/pallets/jinja/")
        );
        assert_eq!(
            info.license.as_deref(),
            Some("BSD License"),
            "the body is not headers"
        );
        let direct = parse("Home-page: https://a\nLicense-Expression: MIT\nLicense: ignored\n");
        assert_eq!(direct.homepage.as_deref(), Some("https://a"));
        assert_eq!(direct.license.as_deref(), Some("MIT"));
        assert_eq!(parse(""), PackageInfo::default());
    }

    #[test]
    fn a_long_license_text_is_not_a_licence_name() {
        let info = parse(
            "License: Copyright (c) 2020\n        Permission is hereby granted\nClassifier: License :: OSI Approved :: MIT License\n",
        );
        assert_eq!(info.license.as_deref(), Some("MIT License"));
    }

    fn layout(windows: bool) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let sp = if windows {
            dir.path().join("Lib").join("site-packages")
        } else {
            dir.path()
                .join("lib")
                .join("python3.12")
                .join("site-packages")
        };
        std::fs::create_dir_all(&sp).unwrap();
        (dir, sp)
    }

    #[test]
    fn site_packages_is_found_on_either_layout() {
        for windows in [false, true] {
            let (dir, sp) = layout(windows);
            assert_eq!(site_packages(dir.path()), Some(sp));
        }
        assert_eq!(site_packages(std::path::Path::new("/nonexistent")), None);
    }

    #[test]
    fn dist_info_is_found_by_canonical_name() {
        let (dir, sp) = layout(false);
        let di = sp.join("typing_extensions-4.12.2.dist-info");
        std::fs::create_dir(&di).unwrap();
        std::fs::write(di.join("METADATA"), "Summary: Backported types\n").unwrap();
        let info = read(dir.path(), "Typing-Extensions", "4.12.2").unwrap();
        assert_eq!(info.summary.as_deref(), Some("Backported types"));
        assert!(
            read(dir.path(), "typing-extensions", "9.9").is_none(),
            "another version"
        );
    }
}
