use std::fs::File;
use std::io::{self, Cursor};
use std::path::PathBuf;

const PDFIUM_VERSION: &str = "7243";

fn main() {
    let out_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("node");
    if out_dir.join("pdfium.wasm").exists() {
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=node/pdfium.wasm");
        return;
    }

    if !out_dir.exists() {
        std::fs::create_dir_all(&out_dir).expect("Failed to create output directory");
    }
    let url = format!(
        "https://github.com/paulocoutinhox/pdfium-lib/releases/download/{}/wasm.tgz",
        PDFIUM_VERSION
    );

    println!("Downloading pdfium from {}", url);
    let response = reqwest::blocking::get(&url).expect("Failed to download pdfium");
    let archive_bytes = response.bytes().expect("Failed to get response bytes");

    let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes)).unwrap();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let outpath = match file.enclosed_name() {
            Some(path) => out_dir.join(path),
            None => continue,
        };

        if (*file.name()).ends_with('/') {
            std::fs::create_dir_all(&outpath).unwrap();
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    std::fs::create_dir_all(p).unwrap();
                }
            }
            let mut outfile = File::create(&outpath).unwrap();
            io::copy(&mut file, &mut outfile).unwrap();
        }
    }

    println!("cargo:rerun-if-changed=build.rs");
}
