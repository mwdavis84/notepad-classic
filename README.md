# Notepad Classic

A deliberately small, fast Windows text editor written in Rust against the raw
Win32 API. The editor is the standard Windows `EDIT` control; there is no GUI
framework, background service, telemetry, network access, updater, or plugin
system.

## Build

Install Rust 1.85 or newer with the MSVC toolchain and the Visual Studio Build
Tools with the **Desktop development with C++** workload, then run on Windows.
Rust 1.85 is the minimum because the project uses Edition 2024.

```powershell
cargo build --release
```

The standalone executable is written to
`target\release\notepad-classic.exe`.

The executable includes a native Windows icon resource with 16, 20, 24, 32,
40, 48, 64, 128, and 256 pixel variants, so the app icon is used in Explorer,
the taskbar, Alt+Tab, and the window caption without needing a sidecar file.

The release profile uses size optimization, whole-program LTO, one codegen unit,
symbol stripping, and aborting panics. These settings trade a slower build for a
smaller executable and avoid unwinding machinery without adding runtime work.

The initial editor font is Lucida Console Regular 10 point, matching the classic
Windows Notepad default. It can be changed for the current run with
**Format > Font**; version 1 deliberately remembers no settings between runs.

## Text formats

New documents are saved as UTF-8 without a BOM. Existing UTF-8, UTF-8 BOM, and
UTF-16 LE BOM files retain their encoding when saved. Other input is decoded
with the active Windows ANSI code page and saved as UTF-8.
