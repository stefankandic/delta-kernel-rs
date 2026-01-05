# Building with ICU Collation Support

**ICU 75.x is REQUIRED** to build the default engine. This ensures collation behavior matches Java/C++ Delta Lake implementations.

## Upgrading ICU Version

To upgrade to a different ICU version, change the single constant at the top of `kernel/icu_build.rs`:

```rust
pub const ICU_VERSION: &str = "75.1";  // Update this to upgrade
```

All version-related paths and URLs are automatically derived from this constant.

## Automatic ICU Installation

**ICU 75.1 is automatically downloaded and built** if not found on your system. The first build will take ~1 minute to compile ICU, but subsequent builds are instant.

## Requirements

- **curl** - For downloading ICU source
- **tar** - For extracting ICU source
- **make** - For building ICU
- **libclang** - Required by bindgen (only for ICU bindings)

## Installing ICU 75 (Optional)

The build system automatically downloads and compiles ICU 75.1 if needed. However, you can install it system-wide for faster builds:

### Option 1: System Package Manager (Faster builds)

#### Ubuntu 24.04+
```bash
sudo apt-get install libicu-dev pkg-config libclang-dev
# Ubuntu 24.04 includes ICU 75.1
```

#### macOS (Homebrew)
```bash
brew install icu4c pkg-config llvm
export PKG_CONFIG_PATH="/opt/homebrew/opt/icu4c/lib/pkgconfig:$PKG_CONFIG_PATH"
```

#### Fedora/RHEL
```bash
sudo dnf install libicu-devel pkgconf-pkg-config clang-devel
```

### Option 2: Build ICU 75 from Source

If your system doesn't have ICU 75, build it locally:

```bash
# Download and build ICU 75.1
bash scripts/setup-icu75.sh

# Set environment variables (add to ~/.bashrc or ~/.zshrc)
export PKG_CONFIG_PATH="$PWD/.icu/icu-75.1/lib/pkgconfig:$PKG_CONFIG_PATH"
export LD_LIBRARY_PATH="$PWD/.icu/icu-75.1/lib:$LD_LIBRARY_PATH"
```

## Building the Project

### Build with Default Engine (includes ICU)
```bash
cargo build --features default-engine-rustls
# or
cargo build --features default-engine-native-tls
```

### Build Core Kernel Only (no ICU required)
```bash
cargo build
# This builds only the core kernel without the default engine
```

### Verify ICU Version
```bash
cargo run --example check_icu_version --features icu-collation
```

Expected output:
```
ICU Version:     75.1.0.0
✓ ICU 75.1 is compatible with Java/C++ ICU 75.x
```

## Configuration

### Using .cargo/config.toml (Recommended)

Create `.cargo/config.toml` in your project or home directory:

```toml
[env]
# Point to your ICU installation
PKG_CONFIG_PATH = { value = "/path/to/icu-75.1/lib/pkgconfig", relative = false }
LD_LIBRARY_PATH = { value = "/path/to/icu-75.1/lib", relative = false }
```

### Using Environment Variables

```bash
export PKG_CONFIG_PATH="/path/to/icu-75.1/lib/pkgconfig:$PKG_CONFIG_PATH"
export LD_LIBRARY_PATH="/path/to/icu-75.1/lib:$LD_LIBRARY_PATH"
```

## Troubleshooting

### "ICU 66.x detected instead of 75.x"

This means cargo is finding system ICU instead of ICU 75. Solutions:

1. Ensure `PKG_CONFIG_PATH` is set correctly
2. Verify: `pkg-config --modversion icu-uc` shows `75.1`
3. Run `cargo clean` to force rebuild of ICU bindings

### "bindgen errors during build"

Install libclang:
```bash
# Ubuntu/Debian
sudo apt-get install libclang-dev

# macOS
brew install llvm
export LIBCLANG_PATH="/opt/homebrew/opt/llvm/lib"
```

## CI/CD Setup

### GitHub Actions Example

```yaml
- name: Install ICU 75
  run: |
    bash scripts/setup-icu75.sh
    echo "PKG_CONFIG_PATH=$PWD/.icu/icu-75.1/lib/pkgconfig" >> $GITHUB_ENV
    echo "LD_LIBRARY_PATH=$PWD/.icu/icu-75.1/lib" >> $GITHUB_ENV

- name: Build with ICU
  run: cargo build --features icu-collation

- name: Test
  run: cargo test --features icu-collation
```

## Why ICU 75?

ICU versions affect collation sort order. To ensure consistent behavior across:
- Java (uses ICU4J 75.x)
- C++ (uses ICU4C 75.x)
- Rust (this project)

We require ICU 75.x specifically.

## Version Mapping

| Component    | Version | CLDR | Unicode |
|--------------|---------|------|---------|
| ICU4C/ICU4J  | 75.1    | 45   | 15.1    |
| rust_icu     | 5.4     | -    | -       |
