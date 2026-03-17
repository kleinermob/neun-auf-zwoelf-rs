# neun-auf-zwoelf-rs

A drop-in `d3d9.dll` replacement written in Rust that transparently forces **D3D9on12** for any Direct3D 9 application.

## What it does

This DLL intercepts `Direct3DCreate9` and `Direct3DCreate9Ex` and redirects them to `Direct3DCreate9On12` / `Direct3DCreate9On12Ex` with `Enable9On12 = TRUE`. All other `d3d9.dll` exports are forwarded transparently to the real system DLL.

## Requirements

- Windows 10/11 (D3D9on12 is built-in)
- Rust + `cargo`
- MSVC toolchain (`rustup target add i686-pc-windows-msvc x86_64-pc-windows-msvc`)

## Building

```bash
# 32-bit (for 32-bit games)
cargo build --target i686-pc-windows-msvc --release

# 64-bit (for 64-bit games)
cargo build --target x86_64-pc-windows-msvc --release
```

Output:
```
target/i686-pc-windows-msvc/release/d3d9.dll    ← 32-bit
target/x86_64-pc-windows-msvc/release/d3d9.dll  ← 64-bit
```

## Usage

Place the compiled `d3d9.dll` next to the game's `.exe`. The shim will load automatically and redirect D3D9 calls to D3D9on12.

> ⚠️ Use the correct bitness — 32-bit DLL for 32-bit games, 64-bit for 64-bit games.

## How it works

```
Game
 └─ Direct3DCreate9()
      └─ [this shim]
           └─ Direct3DCreate9On12(Enable9On12=TRUE)
                └─ System d3d9.dll
                     └─ D3D12 device + DXGI swap chain
```

All other exports (`D3DPERF_*`, `PSGPError`, `DebugSetMute`, etc.) are resolved lazily from the real system `d3d9.dll` at first call and cached for subsequent calls.

## Project structure

```
.cargo/config.toml   — linker flags for both targets
src/lib.rs           — shim implementation
d3d9.def             — export ordinals (must match system d3d9.dll)
Cargo.toml           — crate config (cdylib)
```

## License

MIT
