use flate2::read::GzDecoder;
use std::fs::{self};
use std::io::Cursor;
use std::path::PathBuf;
use tar::Archive;

const PDFIUM_VERSION: &str = "7243";

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = manifest_dir.join("node");

    println!("out_dir: {:?}", out_dir);

    if out_dir.join("pdfium.wasm").exists() {
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=node/pdfium.wasm");
        return;
    }

    if !out_dir.exists() {
        fs::create_dir_all(&out_dir).expect("Failed to create output directory");
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let url = if target_os == "macos" {
        format!(
            "https://github.com/paulocoutinhox/pdfium-lib/releases/download/{}/macos.tgz",
            PDFIUM_VERSION
        )
    } else {
        // Default to wasm
        format!(
            "https://github.com/paulocoutinhox/pdfium-lib/releases/download/{}/wasm.tgz",
            PDFIUM_VERSION
        )
    };

    println!("Downloading pdfium from {}", url);
    let response = reqwest::blocking::get(&url).expect("Failed to download pdfium");
    let archive_bytes = response.bytes().expect("Failed to get response bytes");

    let temp_dir = manifest_dir.join("temp_pdfium_extract");
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).expect("Failed to remove old temp dir");
    }
    fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

    if url.ends_with(".zip") {
        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes)).unwrap();
        archive
            .extract(&temp_dir)
            .expect("Failed to extract zip archive");
    } else if url.ends_with(".tgz") {
        let tar = GzDecoder::new(Cursor::new(archive_bytes));
        let mut archive = Archive::new(tar);
        archive
            .unpack(&temp_dir)
            .expect("Failed to unpack tar.gz archive");
    }

    // The archives extract to a nested directory, so we need to move the files up.
    let nested_dir = temp_dir.join("release").join("node");
    if nested_dir.exists() {
        for entry in fs::read_dir(nested_dir).expect("Failed to read nested dir") {
            let entry = entry.expect("Failed to read entry");
            let from = entry.path();
            let to = out_dir.join(from.file_name().unwrap());
            fs::rename(from, to).expect("Failed to move file");
        }
    }

    fs::remove_dir_all(&temp_dir).expect("Failed to remove temp dir");

    println!("cargo:rerun-if-changed=build.rs");
}
