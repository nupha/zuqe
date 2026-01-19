# Building on Windows

This document provides instructions for compiling this project on Windows platforms.

## Prerequisites

This project requires the GNU compiler toolchain for building Windows binaries. Follow the steps below to set up your Windows development environment.

## Step 1: Install Rust x86_64-pc-windows-gnu Toolchain

First, install the Rust toolchain with Windows GNU target support:

```bash
rustup target add x86_64-pc-windows-gnu
```

This adds the Windows GNU target to your Rust installation, which is required for building Windows binaries using GCC.

## Step 2: Install MSYS2-w64 Environment

MSYS2 provides a Unix-like build environment for Windows. Install MSYS2-w64 from the official website:

1. Download MSYS2 from https://www.msys2.org/
2. Run the installer and follow the installation instructions
3. After installation, launch the MSYS2 MSYS shell

## Step 3: Install Required Toolchains in MSYS2

Open the MSYS2 MSYS shell and install the following packages:

```bash
# Update package database
pacman -Syu

# Install the Windows GNU target
pacman -S mingw-w64-x86_64-toolchain

# Install mingw-w64-clang-x86_64-toolchain (required for bindgen)
pacman -S mingw-w64-clang-x86_64-toolchain
```

The `mingw-w64-x86_64-toolchain` package includes the `x86_64-pc-windows-gnu` target compiler, while `mingw-w64-clang-x86_64-toolchain` provides the Clang toolchain needed for the bindgen code generator.

## Step 4: Configure PATH Environment Variable

After installation, you need to add the Mingw-w64 binaries to your system PATH. For example, if MSYS2 was installed in dir `C:\msys64\`, then the typical paths are:

- `C:\msys64\mingw64\bin`
- `C:\msys64\clang64\bin`

Add these paths to your system PATH environment variable:

1. Right-click on "This PC" or "My Computer" and select "Properties"
2. Click "Advanced system settings"
3. Click "Environment Variables"
4. Under "System variables", find and edit "Path"
5. Add the Mingw-w64 bin directories to the list
6. Click "OK" to save changes

You may need to restart your command prompt or terminal for the changes to take effect.

## Step 5: Create Cargo Configuration

Create or edit the `.cargo/config.toml` file in your project root directory to configure the compiler and include paths:

```toml
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"
ar = "x86_64-w64-mingw32-ar"

[build]
# Point to your MSYS2 installation
target-dir = "target"

[env]
# Set include paths for Windows GNU environment
CFLAGS_x86_64-pc-windows-gnu = "-IC:/msys64/mingw64/include"
CXXFLAGS_x86_64-pc-windows-gnu = "-IC:/msys64/mingw64/include"
LDFLAGS_x86_64-pc-windows-gnu = "-LC:/msys64/mingw64/lib"
```

Adjust the include and library paths according to your MSYS2 installation location if it's different from the default `C:/msys64`.

## Step 6: Build the Project

With all the dependencies installed and configured, you can now build the project:

```bash
cargo build --example zjs
```

This command will compile the project using the Windows GNU toolchain and build the `zjs` example.

## Troubleshooting

- **Linker errors**: Ensure that the Mingw-w64 binaries are properly added to your PATH
- **Include path errors**: Verify that the paths in `.cargo/config.toml` point to your actual MSYS2 installation
- **Missing libraries**: You may need to install additional development packages in MSYS2 using `pacman -S <package-name>`

## Notes

- The project specifically requires the GNU toolchain rather than MSVC for Windows builds
- The `bindgen` tool requires the Clang toolchain, which is why we install `mingw-w64-clang-x86_64-toolchain`
- If you encounter any issues, ensure all toolchains are properly installed and configured in your PATH
