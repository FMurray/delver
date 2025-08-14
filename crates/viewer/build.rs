use flate2::read::GzDecoder;
use std::fs::{self};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tar::Archive;

// Helper function to recursively copy directories
fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

// Helper function to copy pdfium files based on target OS
fn copy_pdfium_files(source_dir: &Path, out_dir: &Path, target_os: &str) {
    println!("cargo:warning=Files in source directory:");
    for entry in fs::read_dir(source_dir).expect("Failed to read source dir") {
        let entry = entry.expect("Failed to read entry");
        let path = entry.path();
        if path.is_file() {
            println!(
                "cargo:warning=  File: {}",
                path.file_name().unwrap().to_string_lossy()
            );
        } else if path.is_dir() {
            println!(
                "cargo:warning=  Dir: {}",
                path.file_name().unwrap().to_string_lossy()
            );
        }
    }

    for entry in fs::read_dir(source_dir).expect("Failed to read source dir") {
        let entry = entry.expect("Failed to read entry");
        let from = entry.path();
        let filename = from.file_name().unwrap().to_string_lossy();

        // Determine if we should copy this file based on target OS
        let should_copy = match target_os {
            "macos" => {
                filename.ends_with(".dylib")
                    || filename.contains("pdfium")
                    || (from.is_dir() && (filename == "lib" || filename.contains("pdfium")))
            }
            "linux" => {
                filename.ends_with(".so")
                    || filename.contains("pdfium")
                    || (from.is_dir() && (filename == "lib" || filename.contains("pdfium")))
            }
            "windows" => {
                filename.ends_with(".dll")
                    || filename.ends_with(".lib")
                    || filename.contains("pdfium")
                    || (from.is_dir() && (filename == "lib" || filename.contains("pdfium")))
            }
            _ => {
                // WASM and others - copy all pdfium files
                filename.contains("pdfium")
            }
        };

        if should_copy {
            if from.is_dir() {
                // For directories, copy all contents to the output directory
                println!(
                    "cargo:warning=Copying directory contents from {} to {}",
                    from.display(),
                    out_dir.display()
                );
                copy_dir_all(&from, out_dir).expect("Failed to copy directory");
            } else {
                let to = out_dir.join(from.file_name().unwrap());
                println!(
                    "cargo:warning=Copying {} to {}",
                    from.display(),
                    to.display()
                );
                fs::copy(&from, &to).expect("Failed to copy file");
            }
        } else {
            println!(
                "cargo:warning=Skipping {} (not needed for {})",
                filename, target_os
            );
        }
    }
}

const PDFIUM_VERSION_WASM: &str = "7243";
const PDFIUM_VERSION_NATIVE: &str = "7337";

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // For WASM builds, use node/ directory. For native builds, use target output directory
    let out_dir = if target_os == "macos" || target_os == "linux" || target_os == "windows" {
        // For native builds, place library in target directory where binary will be
        PathBuf::from(std::env::var("OUT_DIR").unwrap())
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    } else {
        // For WASM builds, use node/ directory
        manifest_dir.join("node")
    };

    println!("cargo:warning=out_dir: {:?}", out_dir);
    println!("cargo:warning=target_os: {}", target_os);

    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    println!("cargo:warning=target_arch: {}", target_arch);

    // Check for existing files based on target OS
    if target_os == "macos" && out_dir.join("libpdfium.dylib").exists() {
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=libpdfium.dylib");
        return;
    } else if target_os != "macos" && out_dir.join("pdfium.wasm").exists() {
        println!("cargo:rerun-if-changed=build.rs");
        println!("cargo:rerun-if-changed=node/pdfium.wasm");
        return;
    }

    // For macOS and other native targets, proceed with download
    if target_os == "macos" || target_os == "linux" || target_os == "windows" {
        println!(
            "cargo:warning=Building for native target ({}) - downloading prebuilt library",
            target_os
        );
    }

    if !out_dir.exists() {
        fs::create_dir_all(&out_dir).expect("Failed to create output directory");
    }

    let url = match target_os.as_str() {
        "macos" | "linux" | "windows" => {
            // Use bblanchon/pdfium-binaries for native targets
            let arch_suffix = match target_arch.as_str() {
                "aarch64" => "arm64",
                "x86_64" => "x64",
                "x86" => "x86",
                _ => "x64", // Default to x64
            };

            let os_name = match target_os.as_str() {
                "macos" => "mac",
                "windows" => "win",
                _ => target_os.as_str(), // linux stays as linux
            };

            format!(
                "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F{}/pdfium-{}-{}.tgz",
                PDFIUM_VERSION_NATIVE, os_name, arch_suffix
            )
        }
        _ => {
            // Use paulocoutinhox/pdfium-lib for WASM and other targets
            format!(
                "https://github.com/paulocoutinhox/pdfium-lib/releases/download/{}/wasm.tgz",
                PDFIUM_VERSION_WASM
            )
        }
    };

    println!("cargo:warning=Downloading pdfium from {}", url);
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

    // Handle different archive structures based on source
    let source_dirs_to_try =
        if target_os == "macos" || target_os == "linux" || target_os == "windows" {
            // bblanchon/pdfium-binaries: files are likely in root or lib/ subdirectory
            vec![
                temp_dir.clone(),         // Try root first
                temp_dir.join("lib"),     // Try lib/ subdirectory
                temp_dir.join("release"), // Fallback to release/ if needed
            ]
        } else {
            // paulocoutinhox/pdfium-lib: files are in release/node/
            vec![temp_dir.join("release").join("node")]
        };

    // Find the first directory that exists and contains files
    let source_dir = source_dirs_to_try
        .into_iter()
        .find(|dir| {
            dir.exists()
                && dir
                    .read_dir()
                    .map(|mut entries| entries.any(|_| true))
                    .unwrap_or(false)
        })
        .unwrap_or_else(|| temp_dir.clone());

    println!("cargo:warning=Looking for source_dir: {:?}", source_dir);

    if source_dir.exists() {
        println!("cargo:warning=Found source directory, copying appropriate files");

        // Copy files based on target OS and source
        copy_pdfium_files(&source_dir, &out_dir, &target_os);
    } else {
        println!("cargo:warning=Source directory not found, checking temp_dir contents");
        // Check what's actually in the temp directory
        for entry in fs::read_dir(&temp_dir).expect("Failed to read temp dir") {
            let entry = entry.expect("Failed to read entry");
            println!("cargo:warning=Found in temp: {}", entry.path().display());
        }
    }

    fs::remove_dir_all(&temp_dir).expect("Failed to remove temp dir");

    println!("cargo:rerun-if-changed=build.rs");

    // Set environment variable for the library path so the code can find it
    match target_os.as_str() {
        "macos" | "linux" | "windows" => {
            // For native builds, also copy to project root for cargo-leptos compatibility
            let project_root = manifest_dir.parent().unwrap().parent().unwrap(); // Go up from crates/viewer to project root

            // Determine library filename based on OS
            let lib_filename = match target_os.as_str() {
                "macos" => "libpdfium.dylib",
                "linux" => "libpdfium.so",
                "windows" => "pdfium.dll",
                _ => "libpdfium.dylib", // fallback
            };

            let project_lib_path = project_root.join(lib_filename);

            // Copy the library to project root where cargo-leptos can find it
            if let Some(lib_file) = std::fs::read_dir(&out_dir).ok().and_then(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .find(|e| e.file_name().to_string_lossy().contains("pdfium"))
            }) {
                let _ = std::fs::copy(lib_file.path(), &project_lib_path);
                println!(
                    "cargo:warning=Copied library to project root: {}",
                    project_lib_path.display()
                );
            }

            // Set the library path to the exact target directory containing the dylib/so/dll
            println!("cargo:rustc-env=PDFIUM_LIBRARY_PATH={}", out_dir.display());
        }
        _ => {
            // For WASM builds, use node directory
            println!(
                "cargo:rustc-env=PDFIUM_LIBRARY_PATH={}",
                manifest_dir.join("node").display()
            );
        }
    }

    // Set appropriate rerun conditions based on target OS
    match target_os.as_str() {
        "macos" => {
            println!("cargo:rerun-if-changed=libpdfium.dylib");
        }
        "linux" => {
            println!("cargo:rerun-if-changed=libpdfium.so");
        }
        "windows" => {
            println!("cargo:rerun-if-changed=pdfium.dll");
        }
        _ => {
            // WASM and other targets
            println!("cargo:rerun-if-changed=node/pdfium.wasm");
        }
    }
}
