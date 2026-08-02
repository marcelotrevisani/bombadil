//! Acquires the pinned uv binary for the *target* triple and places it in
//! OUT_DIR for `include_bytes!`.
//!
//! Network access happens here, at build time, never at runtime. Set
//! BOMBADIL_UV_ARCHIVE to a pre-downloaded archive to build offline.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=uv-manifest.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BOMBADIL_UV_ARCHIVE");

    let target = std::env::var("TARGET").expect("TARGET is set by cargo");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));

    let manifest: toml::Value = toml::from_str(
        &std::fs::read_to_string("uv-manifest.toml").expect("uv-manifest.toml is readable"),
    )
    .expect("uv-manifest.toml parses");

    let version = manifest["version"].as_str().expect("version is a string");
    let entry = manifest
        .get("targets")
        .and_then(|t| t.get(&target))
        .unwrap_or_else(|| {
            panic!(
                "uv-manifest.toml has no entry for target {target}. \
                 Add its asset name and sha256 from the uv 0.12.1 release."
            )
        });
    let asset = entry["asset"].as_str().expect("asset is a string");
    let expected_sha = entry["sha256"].as_str().expect("sha256 is a string");

    let archive = match std::env::var("BOMBADIL_UV_ARCHIVE") {
        Ok(path) => std::fs::read(&path)
            .unwrap_or_else(|e| panic!("BOMBADIL_UV_ARCHIVE={path} could not be read: {e}")),
        Err(_) => download(&format!(
            "https://github.com/astral-sh/uv/releases/download/{version}/{asset}"
        )),
    };

    let actual_sha = hex(&Sha256::digest(&archive));
    assert_eq!(
        actual_sha, expected_sha,
        "checksum mismatch for {asset}: refusing to embed an unverified binary"
    );

    let exe_name = if target.contains("windows") {
        "uv.exe"
    } else {
        "uv"
    };
    let binary = if asset.ends_with(".zip") {
        extract_from_zip(&archive, exe_name)
    } else {
        extract_from_tar_gz(&archive, exe_name)
    };

    std::fs::write(out_dir.join(exe_name), &binary).expect("write uv to OUT_DIR");
    println!("cargo:rustc-env=BOMBADIL_UV_EXE_NAME={exe_name}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// ureq 3's default has no global timeout, so a stalled body read would
/// otherwise hang the build (and CI) indefinitely.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn download(url: &str) -> Vec<u8> {
    let mut body = Vec::new();
    ureq::get(url)
        .config()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .call()
        .unwrap_or_else(|e| panic!("could not download {url}: {e}"))
        .into_body()
        .into_reader()
        .read_to_end(&mut body)
        .unwrap_or_else(|e| panic!("could not read body of {url}: {e}"));
    body
}

/// Match on file name, not on a hardcoded path: the archives wrap the binary
/// in a top-level directory whose name has changed between uv releases.
fn matches_name(path: &Path, exe_name: &str) -> bool {
    path.file_name().map(|n| n == exe_name).unwrap_or(false)
}

fn extract_from_tar_gz(archive: &[u8], exe_name: &str) -> Vec<u8> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().expect("tar entries") {
        let mut entry = entry.expect("tar entry");
        let path = entry.path().expect("entry path").into_owned();
        if matches_name(&path, exe_name) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read uv from tar");
            return bytes;
        }
    }
    panic!("no {exe_name} member found in the uv archive");
}

fn extract_from_zip(archive: &[u8], exe_name: &str) -> Vec<u8> {
    let reader = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(reader).expect("open uv zip");
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).expect("zip entry");
        let name = entry.name().to_string();
        if matches_name(Path::new(&name), exe_name) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read uv from zip");
            return bytes;
        }
    }
    panic!("no {exe_name} member found in the uv archive");
}
