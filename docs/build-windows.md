# Building on Windows

This document provides instructions for compiling this project on Windows platforms.

## Prerequisites

This project requires the GNU compiler toolchain for building Windows binaries. Follow the steps below to set up your Windows development environment.

## Step 1: Install Rust

There are two scenarios depending on whether you already have Rust installed.

### Fresh Install

1. Download `rustup-init.exe` from https://www.rust-lang.org/tools/install
2. Run the installer. When prompted to choose a toolchain, **do not** select the default MSVC target 鈥?it requires Visual Studio and takes a long time to download. Instead, select the GNU option or customize the installation to use the `x86_64-pc-windows-gnu` target, which is much lighter.

### Already Have Rustup

If you already have `rustup` installed, make sure the `x86_64-pc-windows-gnu` target is available:

```bash
rustup target add x86_64-pc-windows-gnu
```

Run `rustup target list --installed` to verify it appears in the installed targets list.

## Step 2: Install MSYS2

MSYS2 provides a Unix-like build environment for Windows. Install MSYS2 from the official website:

1. Download MSYS2 from https://www.msys2.org/
2. Run the installer and follow the installation instructions
3. After installation, launch the MSYS2 MSYS shell

## Step 3: Install Required Toolchains in MSYS2

Open the MSYS2 MSYS shell and install the following packages:

```bash
# Update package database
pacman -Syu

# Install the minimum required toolchains
pacman -S base-devel mingw-w64-x86_64-gcc mingw-w64-clang-x86_64-clang
```

Each package serves a specific purpose:

| Package | Purpose |
|---|---|
| `base-devel` | Core build utilities (`make`, `patch`, etc.) |
| `mingw-w64-x86_64-gcc` | GCC compiler + binutils (`dlltool`, `ar`, `ld`) 鈥?used by the `cc` crate to compile C source code (e.g. the `lre` regex engine), and `dlltool` is required by the `libloading` crate |
| `mingw-w64-clang-x86_64-clang` | Clang compiler 鈥?provides `libclang`, which `bindgen` dynamically loads at build time to parse C/C++ headers |

> **Note:** The full `mingw-w64-x86_64-toolchain` meta-package is **not** required. It pulls in GDB, make, and many other tools this project does not need. The individual packages above are sufficient and significantly lighter.

## Step 4: Configure PATH Environment Variable

After installation, you need to add the MSYS2 toolchain binaries to your system PATH. For example, if MSYS2 was installed in dir `C:\msys64\`, then the required paths are:

- `C:\msys64\mingw64\bin` 鈥?MinGW-w64 GCC toolchain (`gcc`, `dlltool`, `ar`, etc.)
- `C:\msys64\clang64\bin` 鈥?Clang toolchain required by `bindgen`
- `C:\msys64\usr\bin` 鈥?Common Unix utilities (`git`, `make`, `pacman`, `bash`, etc.)

> ⚠️ **IMPORTANT**
>
> - Place these paths at the **top** of the PATH list (above other entries). The `bindgen` crate loads `libclang.dll` at build time, which depends on `libLLVM-22.dll` and other DLLs in `clang64\bin`. If these paths appear too low in the PATH order, Windows may load incompatible DLLs from other directories first, causing `LoadLibraryExW` failures.
>
> - Set the environment variable `MSYS2_PATH_TYPE=inherit` so that MSYS2 shells inherit the system PATH. By default, MSYS2 does **not** inherit external PATH entries, which means tools like `cargo` (installed under `%USERPROFILE%\.cargo\bin`) will not be found inside the MSYS2 shell. Setting this variable ensures `cargo`, `rustc`, and other externally installed tools are accessible.

Add these paths to your system PATH environment variable, and also create the `MSYS2_PATH_TYPE` variable:

1. Right-click on "This PC" or "My Computer" and select "Properties"
2. Click "Advanced system settings"
3. Click "Environment Variables"
4. Under "System variables", find and edit "Path"
5. Add all three directories listed above to the list
6. Under "System variables", click "New" to create a new variable: set **Variable name** to `MSYS2_PATH_TYPE` and **Variable value** to `inherit`
7. Click "OK" to save changes

You may need to restart your command prompt or terminal for the changes to take effect.

## Step 5: Configure the Linker and Build Environment

> ⚠️ **IMPORTANT — Use `rust-lld`, not MinGW's GNU ld!**
>
> The MinGW GNU ld linker (`x86_64-w64-mingw32-gcc` / `ld`) has a known defect when linking large Rust projects: it generates **invalid `.pdata` entries** (the Windows x64 function unwind-information table) when there are many input sections (e.g. ~100K entries).
>
> This causes the PE loader to crash with `STATUS_ACCESS_VIOLATION` (exit code `-1073741819`) **before `main()` is even reached** — a pre-main crash that manifests as "no output, immediate exit".
>
> The crash appears only in `debug` mode (where more individual functions exist) and only with the MinGW GNU ld linker. It is **not** caused by the `.pdata` section being too large — LLD produces an even bigger `.pdata` section that works perfectly — but by GNU ld producing **malformed** entries.
>
> The fix is to use **`rust-lld`** (the LLVM LLD linker bundled with the Rust toolchain) instead of the MinGW GNU ld. LLD generates correct `.pdata` tables for any project size.

Create or edit the `.cargo/config.toml` file in your project root directory:

```toml
[target.x86_64-pc-windows-gnu]
linker = "rust-lld"
```

Linker `rust-lld` is available at `<rust-sysroot>/lib/rustlib/x86_64-pc-windows-gnu/bin/rust-lld.exe`, bundled with every Rust toolchain — no separate installation needed. Cargo resolves it through the rustup proxy automatically.

> ⚠️ **DO NOT use `"x86_64-w64-mingw32-gcc"` as the linker.** 
>
> That path would invoke MinGW's GNU ld, which triggers the pre-main `STATUS_ACCESS_VIOLATION` crash described above on any non-trivial Rust project in debug mode.

Adjust the include and library paths according to your MSYS2 installation location if it's different from the default `C:/msys64`.

## Step 6: Build the Project

Open the **MSYS2 MSYS shell** (not PowerShell or CMD) and navigate to the project directory. With all the dependencies installed and configured, you can now build the project:

```bash
cargo build --examples
```

This command will compile the project using the Windows GNU toolchain and build the `zjs` and `test262` examples.

> **Note:** It is strongly recommended to build inside the **MSYS2 MSYS shell**. Running `cargo build` in PowerShell may cause GCC to crash with errors like `gcc: error: spawn failed (exit status 0xc0000005)` due to DLL conflicts or environment incompatibilities. MSYS2 provides a Unix-like environment that the GNU toolchain expects and avoids these issues.

## Troubleshooting

- **`gcc: error: spawn failed (exit status 0xc0000005)` in PowerShell**: This is an access violation caused by running GCC outside the MSYS2 shell. DLL conflicts or incompatible environment variables in PowerShell can crash GCC's process spawning. Switch to the **MSYS2 MSYS shell** to resolve this.
- **Pre-main crash (exit code -1073741819)**: If the process exits immediately on startup with `STATUS_ACCESS_VIOLATION` before reaching any code in `main()`, verify that your `.cargo/config.toml` uses `linker = "rust-lld"` and **not** `"x86_64-w64-mingw32-gcc"`. See Step 5 for details.
- **Linker errors**: Ensure that the MinGW-w64 binaries are properly added to your PATH
- **`cargo` not found in MSYS2 shell**: Set the environment variable `MSYS2_PATH_TYPE=inherit` so MSYS2 inherits the system PATH (including `%USERPROFILE%\.cargo\bin`)
- **`libclang.dll` / `LoadLibraryExW` errors**: Ensure the MSYS2 paths are at the **top** of your PATH list, not at the bottom. The `libloading` crate does not use `LOAD_WITH_ALTERED_SEARCH_PATH`, so DLL dependencies (like `libLLVM-22.dll`) are resolved via the standard search order. If another directory earlier in PATH contains a conflicting DLL, loading will fail.
- **Include path errors**: Verify that the paths in `.cargo/config.toml` point to your actual MSYS2 installation
- **Missing libraries**: You may need to install additional development packages in MSYS2 using `pacman -S <package-name>`

## Notes

- The project specifically requires the GNU toolchain rather than MSVC for Windows builds
- The `bindgen` crate (used by `lre`) dynamically loads `libclang` at build time, which is why `mingw-w64-clang-x86_64-clang` is required alongside GCC
- If you encounter any issues, ensure all toolchains are properly installed and configured in your PATH


