//! ICU build configuration and setup.
//!
//! This module handles:
//! - Detecting or downloading/building ICU
//! - Generating FFI bindings with bindgen
//! - Configuring static linking

use std::env;
use std::path::PathBuf;
use std::process::Command;

// ============================================================================
// ICU Version Configuration
// ============================================================================
pub const ICU_VERSION: &str = "75.1";
const ICU_MAJOR: u8 = parse_major_version(ICU_VERSION);

// Parse major version at compile time (works for any number of digits)
const fn parse_major_version(version: &str) -> u8 {
    let bytes = version.as_bytes();
    let mut result = 0u8;
    let mut i = 0;
    // Parse digits until we hit '.'
    while i < bytes.len() && bytes[i] != b'.' {
        result = result * 10 + (bytes[i] - b'0');
        i += 1;
    }
    result
}

/// Main entry point for ICU setup.
/// This function:
/// 1. Detects or builds ICU
/// 2. Generates FFI bindings with bindgen
/// 3. Configures linking (static linking + libstdc++)
pub fn setup_icu() {
    setup_icu_installation();
    generate_bindings();
    configure_linking();
}

fn setup_icu_installation() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    let project_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let icu_dir = project_root.join(format!(".icu/icu-{}", ICU_VERSION));

    if icu_dir.exists() && icu_dir.join("bin/icu-config").exists() {
        // ICU already available, set up environment silently
        setup_env(&icu_dir);
        return;
    }

    // ICU not found - download and build it automatically
    println!("cargo:warning=ICU {} not found, downloading and building...", ICU_VERSION);

    download_and_build(&project_root);
    setup_env(&icu_dir);
}

fn setup_env(icu_dir: &PathBuf) {
    let pkg_config_path = icu_dir.join("lib/pkgconfig");

    // Set PKG_CONFIG_PATH so bindgen can find ICU headers
    if let Ok(existing) = env::var("PKG_CONFIG_PATH") {
        env::set_var("PKG_CONFIG_PATH", format!("{}:{}", pkg_config_path.display(), existing));
    } else {
        env::set_var("PKG_CONFIG_PATH", pkg_config_path);
    }
}

fn download_and_build(project_root: &PathBuf) {
    // Derive version formats from ICU_VERSION
    let icu_version_underscore = ICU_VERSION.replace('.', "_");
    let icu_release_tag = format!("release-{}", ICU_VERSION.replace('.', "-"));

    let icu_base_dir = project_root.join(".icu");
    let icu_install_dir = icu_base_dir.join(format!("icu-{}", ICU_VERSION));
    let icu_tarball = icu_base_dir.join(format!("icu4c-{}-src.tgz", icu_version_underscore));
    let icu_src_dir = icu_base_dir.join("icu/source");

    // Create .icu directory
    std::fs::create_dir_all(&icu_base_dir).expect("Failed to create .icu directory");

    // Download ICU if not already downloaded
    if !icu_tarball.exists() {
        println!("cargo:warning=Downloading ICU {} source...", ICU_VERSION);
        let download_url = format!(
            "https://github.com/unicode-org/icu/releases/download/{}/icu4c-{}-src.tgz",
            icu_release_tag, icu_version_underscore
        );
        let status = Command::new("curl")
            .args(&[
                "-L",
                "-o",
                icu_tarball.to_str().unwrap(),
                &download_url,
            ])
            .status()
            .expect("Failed to download ICU. Is curl installed?");

        if !status.success() {
            panic!("Failed to download ICU {}", ICU_VERSION);
        }
    }

    // Extract if not already extracted
    if !icu_src_dir.exists() {
        println!("cargo:warning=Extracting ICU source...");
        let status = Command::new("tar")
            .args(&["xzf", icu_tarball.to_str().unwrap()])
            .current_dir(&icu_base_dir)
            .status()
            .expect("Failed to extract ICU. Is tar installed?");

        if !status.success() {
            panic!("Failed to extract ICU {}", ICU_VERSION);
        }
    }

    // Build ICU if not already built
    if !icu_install_dir.join("bin/icu-config").exists() {
        println!("cargo:warning=Configuring ICU {}...", ICU_VERSION);
        let status = Command::new("./configure")
            .arg(format!("--prefix={}", icu_install_dir.display()))
            .args(&["--enable-static", "--disable-shared"])  // Use only static libraries
            .current_dir(&icu_src_dir)
            .status()
            .expect("Failed to configure ICU");

        if !status.success() {
            panic!("Failed to configure ICU {}", ICU_VERSION);
        }

        println!("cargo:warning=Building ICU {} ...", ICU_VERSION);
        let num_jobs = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        let status = Command::new("make")
            .arg("-j")
            .arg(num_jobs.to_string())
            .current_dir(&icu_src_dir)
            .status()
            .expect("Failed to build ICU");

        if !status.success() {
            panic!("Failed to build ICU {}", ICU_VERSION);
        }

        println!("cargo:warning=Installing ICU {}...", ICU_VERSION);
        let status = Command::new("make")
            .arg("install")
            .current_dir(&icu_src_dir)
            .status()
            .expect("Failed to install ICU");

        if !status.success() {
            panic!("Failed to install ICU {}", ICU_VERSION);
        }

        println!("cargo:warning=ICU {} successfully built and installed!", ICU_VERSION);
    }
}

/// Generate Rust FFI bindings for ICU using bindgen.
fn generate_bindings() {
    println!("cargo:rerun-if-changed=icu_wrapper.h");

    // Get ICU include path from pkg-config
    let icu_include = Command::new("pkg-config")
        .args(&["--cflags-only-I", "icu-uc"])
        .output()
        .expect("Failed to run pkg-config for ICU");

    let include_paths = String::from_utf8(icu_include.stdout)
        .expect("Failed to parse pkg-config output");

    // Write a minimal wrapper header
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let wrapper_path = out_dir.join("icu_wrapper.h");
    std::fs::write(
        &wrapper_path,
        r#"
#include <unicode/ucol.h>
#include <unicode/ustring.h>
#include <unicode/uchar.h>
#include <unicode/uversion.h>
#include <unicode/uloc.h>
"#,
    )
    .expect("Failed to write wrapper header");

    // Generate bindings
    let mut builder = bindgen::Builder::default()
        .header(wrapper_path.to_str().unwrap())
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // Add include paths from pkg-config to clang args
    for include in include_paths.split_whitespace() {
        if include.starts_with("-I") {
            builder = builder.clang_arg(include);
        }
    }

    // Build list of versioned functions
    let version_suffix = format!("_{}", ICU_MAJOR);
    let bindings = builder
        // Allow list only the functions we need (with version suffix)
        .allowlist_function(format!("u_getVersion{}", version_suffix))
        .allowlist_function(format!("u_tolower{}", version_suffix))
        .allowlist_function(format!("ucol_open{}", version_suffix))
        .allowlist_function(format!("ucol_close{}", version_suffix))
        .allowlist_function(format!("ucol_strcollUTF8{}", version_suffix))
        .allowlist_function(format!("ucol_getAvailable{}", version_suffix))
        .allowlist_function(format!("ucol_countAvailable{}", version_suffix))
        .allowlist_function(format!("uloc_getISO3Country{}", version_suffix))
        .allowlist_function(format!("uloc_getLanguage{}", version_suffix))
        .allowlist_function(format!("uloc_getScript{}", version_suffix))
        // Allow list types
        .allowlist_type("UCollator")
        .allowlist_type("UErrorCode")
        .allowlist_type("UCollationResult")
        .allowlist_var("U_.*")
        .generate()
        .expect("Unable to generate ICU bindings");

    let out_path = out_dir.join("icu_bindings.rs");
    bindings
        .write_to_file(&out_path)
        .expect("Couldn't write ICU bindings");
}

/// Configure linking for ICU libraries.
/// Uses static linking and includes libstdc++ for C++ support.
fn configure_linking() {
    // Get ICU library path from pkg-config
    let icu_libs = Command::new("pkg-config")
        .args(&["--libs-only-L", "icu-uc"])
        .output()
        .expect("Failed to run pkg-config for ICU libs");

    let lib_paths = String::from_utf8(icu_libs.stdout)
        .expect("Failed to parse pkg-config output");

    // Add library search paths
    for lib_path in lib_paths.split_whitespace() {
        if lib_path.starts_with("-L") {
            let path = &lib_path[2..];
            println!("cargo:rustc-link-search=native={}", path);
        }
    }

    // Link against ICU libraries statically
    println!("cargo:rustc-link-lib=static=icuuc");
    println!("cargo:rustc-link-lib=static=icui18n");
    println!("cargo:rustc-link-lib=static=icudata");

    // ICU is C++ code, so we need to link against the C++ standard library
    println!("cargo:rustc-link-lib=stdc++");
}
