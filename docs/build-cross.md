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

Create or edit `.cargo/config.toml` and add the following content:

```toml
[target.x86_64-pc-windows-gnu]
linker = "/usr/local/bin/x86_64-w64-mingw32-gcc"
ar = "/usr/local/bin/x86_64-w64-mingw32-ar"

[env]
CC_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-gcc"
CXX_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-g++"
AR_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-ar"
CFLAGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
CXXFLAGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
```

**Important:** The paths above are examples for typical macOS installations using Homebrew. You **must** update these paths according to your actual MinGW-w64 installation:

- **macOS with Homebrew**: Use `/usr/local/bin/x86_64-w64-mingw32-*` (as shown in examples)
- **Linux**: Typically use `/usr/bin/x86_64-w64-mingw32-*`
- **Custom installations**: Use the actual paths where your MinGW-w64 binaries are located

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
