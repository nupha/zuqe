# Cross-Compiling Windows Binaries from Linux/macOS

This guide explains how to cross-compile Windows binaries from Linux or macOS using Rust, MinGW-w64 and clang.

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

### Install clang

`clang` is used as the linker driver (see Configuration below). On macOS it
ships with the Xcode Command Line Tools; on Linux install it from your
distribution:

```bash
# macOS (if `clang --version` doesn't work)
xcode-select --install

# Debian/Ubuntu
sudo apt install clang
```

## Configuration

Merge the following settings into the existing `.cargo/config.toml`. Do **not** overwrite that file — it may already hold entries you must keep (e.g. an active `[patch.crates-io]` override such as `gc-lite = { path = "../gc-lite-v2" }` when developing against a local gc-lite checkout):

> The example below matches a typical macOS Homebrew install. Adjust paths for your system (see the path-discovery commands further down).

```toml
[target.x86_64-pc-windows-gnu]
# clang is only the linker *driver*: it injects the MinGW CRT startup objects
# (crt2.o / dllcrt2.o) and the default library search paths. The actual
# linking is done by rust's bundled lld via -fuse-ld=lld.
linker = "clang"
ar = "/usr/local/bin/x86_64-w64-mingw32-ar"

rustflags = [
    "-C", "link-arg=--target=x86_64-w64-windows-gnu",
    "-C", "link-arg=-fuse-ld=lld",
    # rust's bundled ld.lld — the real linker; clang resolves it via -B.
    # Replace the placeholders with the output of the discovery commands below.
    "-C", "link-arg=-B<rustup-sysroot>/lib/rustlib/<host-triple>/bin/gcc-ld/",
    # MinGW toolchain root (the directory containing bin/x86_64-w64-mingw32-gcc):
    # provides the CRT startup objects (crt2.o / dllcrt2.o)
    "-C", "link-arg=-B/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/",
    # MinGW runtime libraries (libwinpthread, libmingwex, libmingw32, ...)
    "-C", "link-arg=-L/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/lib",
    # libgcc.a / libgcc_eh.a — rustc passes -lgcc / -lgcc_eh
    "-C", "link-arg=-L/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/lib/gcc/x86_64-w64-mingw32/15.2.0",
]

[env]
CC_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-gcc"
CXX_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-g++"
AR_x86_64_pc_windows_gnu = "/usr/local/bin/x86_64-w64-mingw32-ar"
CFLAGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
CXXFLAGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu = "-I/usr/local/Cellar/mingw-w64/13.0.0_2/toolchain-x86_64/x86_64-w64-mingw32/include"
```

> **Note:** clang's driver links against the Universal CRT (`api-ms-win-crt-*`, Windows 10+). MinGW GCC defaults to the legacy `msvcrt.dll`.

**Important:** Do **not** use MinGW's GNU ld as the actual linker. It emits malformed `.pdata` unwind entries for large Rust projects, causing a pre-main `STATUS_ACCESS_VIOLATION` crash in debug builds; rust's bundled `lld` generates correct `.pdata` (see `docs/build-windows-gnu.md`, Step 5). That is why `-fuse-ld=lld` is required — but note:

- **Do not set `linker = "x86_64-w64-mingw32-gcc"` hoping for `-fuse-ld=lld`**: Homebrew's GCC is configured with `--with-ld=<absolute GNU ld path>`, so it silently ignores `-fuse-ld` and always invokes GNU ld.
- **Do not set `linker = "rust-lld"` directly**: raw `rust-lld` bypasses compiler-driver conventions, so no CRT startup objects are supplied and links fail with undefined `DllMainCRTStartup` (DLLs) / `mainCRTStartup` (executables). Enabling rust's own CRT via `-C link-self-contained` doesn't work either: its bundled `dllcrt2.o` references `DllEntryPoint`, which mingw-w64 ≤ 13 does not provide.

**Important:** The paths above are examples for typical macOS installations using Homebrew. You **must** update these paths according to your actual installation:

- **macOS with Homebrew**: MinGW binaries at `/usr/local/bin/x86_64-w64-mingw32-*` (as shown in examples)
- **Linux**: typically `/usr/bin/x86_64-w64-mingw32-*`
- **Custom installations**: use the actual paths where your MinGW-w64 binaries are located
- **`-B` entries**: the rust sysroot's `gcc-ld` directory (must contain `ld.lld`) and your MinGW toolchain root
- **`-L` entries**: your MinGW-w64 runtime library directory and GCC's `lib/gcc/x86_64-w64-mingw32/<gcc-version>` directory (provides `libgcc.a` / `libgcc_eh.a`)

To find the correct paths on your system, use:

```bash
which x86_64-w64-mingw32-gcc
whereis x86_64-w64-mingw32-gcc
rustc --print sysroot                       # <rustup-sysroot>
rustc -vV | sed -n 's/^host: //p'           # <host-triple>, e.g. x86_64-apple-darwin
ls "<rustup-sysroot>/lib/rustlib/<host-triple>/bin/gcc-ld"   # must contain ld.lld
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
