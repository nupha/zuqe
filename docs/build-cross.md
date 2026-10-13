# Cross-Compiling Windows Binaries from Linux/macOS

This guide explains how to cross-compile Windows binaries from Linux or macOS using Rust and MinGW-w64.

## Prerequisites

### Install Rust Target

Install the Windows GNU target for Rust:

```bash
rustup target add x86_64-pc-windows-gnu
```

### Install MinGW-w64

#### macOS

Install MinGW-w64 using Homebrew:

```bash
brew install mingw-w64
```

#### Linux

Install MinGW-w64 using your distribution's package manager:

**Debian/Ubuntu:**

```bash
sudo apt update
sudo apt install mingw-w64
```

**Arch/Manjaro:**

```bash
sudo pacman -S mingw-w64-gcc
```

**Fedora/RHEL:**

```bash
sudo dnf install mingw64-gcc
```

**openSUSE:**

```bash
sudo zypper install mingw64-gcc
```

## Configuration

Merge the following settings into the existing `.cargo/config.toml`. Do **not** overwrite that file — it already holds the `[patch.crates-io]` entries this project needs:

```toml
[target.x86_64-pc-windows-gnu]
linker = "rust-lld"
ar = "/usr/local/bin/x86_64-w64-mingw32-ar"

# rust-lld does not inherit MinGW library search paths automatically
# (unlike the MinGW GCC linker driver). Pass them explicitly.
rustflags = [
    "-C", "link-arg=-L/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/lib",
    "-C", "link-arg=-lwinpthread",
]

[env]
CC_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-gcc"
CXX_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-g++"
AR_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-ar"
CFLAGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
CXXFLAGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
```

**Important:** Do **not** use `x86_64-w64-mingw32-gcc` (MinGW's GNU ld) as the linker. It emits malformed `.pdata` unwind entries for large Rust projects, causing a pre-main `STATUS_ACCESS_VIOLATION` crash in debug builds; `rust-lld`, bundled with the Rust toolchain, generates correct `.pdata` (see `docs/build-windows-gnu.md`, Step 5).

**Important:** The paths above are examples for typical macOS installations using Homebrew. You **must** update these paths according to your actual MinGW-w64 installation:

- **macOS with Homebrew**: Use `/usr/local/bin/x86_64-w64-mingw32-*` (as shown in examples)
- **Linux**: Typically use `/usr/bin/x86_64-w64-mingw32-*`
- **Custom installations**: Use the actual paths where your MinGW-w64 binaries are located
- **Link search path**: Point the `-L` argument in `rustflags` at your MinGW-w64 runtime library directory

To find the correct paths on your system, use:

```bash
which x86_64-w64-mingw32-gcc
whereis x86_64-w64-mingw32-gcc
```

## Cross-Compilation

Build your project for Windows using:

```bash
cargo build --target x86_64-pc-windows-gnu --example zjs
```

The compiled Windows binary will be located in:

```
target/x86_64-pc-windows-gnu/debug/examples/zjs.exe
```
