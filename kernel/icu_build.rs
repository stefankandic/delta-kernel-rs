use std::env;
use std::path::PathBuf;
use std::process::Command;

// ICU Version Configuration - Change here to upgrade ICU version.
pub const ICU_VERSION: &str = "75.1";

pub fn setup() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    // Check if compatible ICU version is available via pkg-config
    if let Ok(output) = Command::new("pkg-config")
        .args(&["--modversion", "icu-uc"])
        .output()
    {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();

            // Check major.minor match (allow any patch version)
            if version.starts_with(&format!("{}.", ICU_VERSION)) {
                println!("cargo:warning=Using system ICU {}", version);
                return; // System ICU found with correct major.minor version
            } else {
                println!("cargo:warning=Found ICU {} but need {} specifically.", version, ICU_VERSION);
            }
        }
    }

    // Check for local ICU installation
    let project_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let icu_dir = project_root.join(format!(".icu/icu-{}", ICU_VERSION));

    if icu_dir.exists() && icu_dir.join("bin/icu-config").exists() {
        println!("cargo:warning=Using local ICU {} from .icu/", ICU_VERSION);
        setup_env(&icu_dir);
        return;
    }

    // ICU not found - download and build it automatically
    println!("cargo:warning=ICU {} not found, downloading and building automatically...", ICU_VERSION);
    println!("cargo:warning=This is a one-time build and will take ~1 minute...");

    download_and_build(&project_root);
    setup_env(&icu_dir);
}

fn setup_env(icu_dir: &PathBuf) {
    let pkg_config_path = icu_dir.join("lib/pkgconfig");
    let ld_library_path = icu_dir.join("lib");

    // IMPORTANT: These env vars only affect THIS build script.
    // To build/test with ICU 75.1, you MUST set these in your shell BEFORE running cargo:
    //   export PKG_CONFIG_PATH="$PWD/.icu/icu-75.1/lib/pkgconfig"
    //   export LD_LIBRARY_PATH="$PWD/.icu/icu-75.1/lib"
    // Build scripts run in isolation and cannot propagate env vars to dependency build scripts.

    // Set for local use in this build script
    if let Ok(existing) = env::var("PKG_CONFIG_PATH") {
        env::set_var(
            "PKG_CONFIG_PATH",
            format!("{}:{}", pkg_config_path.display(), existing),
        );
    } else {
        env::set_var("PKG_CONFIG_PATH", pkg_config_path);
    }

    if let Ok(existing) = env::var("LD_LIBRARY_PATH") {
        env::set_var(
            "LD_LIBRARY_PATH",
            format!("{}:{}", ld_library_path.display(), existing),
        );
    } else {
        env::set_var("LD_LIBRARY_PATH", ld_library_path);
    }

    println!("cargo:warning=ICU {} environment configured.", ICU_VERSION);
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
            .args(&["--enable-static", "--enable-shared"])
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
