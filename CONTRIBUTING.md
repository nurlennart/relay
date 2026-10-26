# Contributing

## Building

Relay builds with stable Rust (1.88 or newer) on Windows, macOS, and Linux:

```sh
cargo run --release
```

- **Windows:** the MSVC toolchain and the Visual Studio C++ Build Tools.
- **macOS:** the Xcode command line tools (`xcode-select --install`).
- **Linux:** a C compiler (`build-essential` or equivalent). No GUI development packages are needed; windowing, graphics, and file dialogs are loaded at run time.

## Project layout

| Path | What it holds |
| --- | --- |
| `src/main.rs` | Module list and entry point. |
| `src/app/` | The `App` state and frame loop (`mod.rs`) and its screens: `sidebar`, `tabs`, `library`, `sessions`, `editor`, `settings`, `storage`, plus shared `widgets` and the UI tests. |
| `src/platform/` | Everything that differs per OS: data folders, fonts, the ssh binary, and the Windows-only native window code (`windows.rs`); `other.rs` covers macOS and Linux. |
| `src/rdp/` | Remote desktops: the embedded Windows ActiveX control (`activex.rs`) and the FreeRDP command line used elsewhere (`freerdp.rs`). |
| `src/session.rs`, `src/terminal.rs` | Open tabs and the terminal they run ssh or FreeRDP in. |
| `src/model.rs`, `src/database.rs`, `src/state.rs` | Connections, libraries (JSON, SQLite, MSSQL), and saved workspace state. |
| `src/csv_io.rs`, `src/import.rs` | CSV import and export, SSH config and `.rdp` import. |
| `src/secret.rs`, `src/file_dialog.rs`, `src/theme.rs` | Credential store, file pickers, color themes. |
| `packaging/` | The macOS app bundle and Linux desktop entry used by releases. |

Platform-specific code stays in `src/platform` and `src/rdp`; the rest of the app uses their cross-platform interface and `rdp::EMBEDDED` rather than `cfg` attributes.

## Checks

CI runs these on Windows, macOS, and Linux for every pull request:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Tests cover the CSV export schema (including UTF-16), formula-guard round trips, duplicate detection, validation, serialization, atomic saves, corrupt-file preservation, SQLite stale-write rejection, MSSQL configuration, FreeRDP arguments, terminal parsing, keys and focus, and a real pseudo-terminal's input, output, and resize. On Windows, the RDP checks create the installed ActiveX control and verify credential prompting and viewport sizes without contacting a remote host. Live MSSQL access, RDP and SSH authentication, and SMB locking need real infrastructure.

### Rendering checks

The optional `ui-check` feature captures rendered screens without writing profiles:

```sh
RELAY_UI_CHECK="$PWD/target/preview.png" cargo run --features ui-check
```

Add `RELAY_UI_FIXTURE=1` to render the sample CSV, `RELAY_UI_IMPORT=1` for its preview, `RELAY_UI_EDITOR=1` for the editor, `RELAY_UI_DATABASE=1` for database setup, `RELAY_UI_SETTINGS=1` for the settings menu, or `RELAY_UI_TABS=1` for session tabs. `RELAY_UI_THEME=Midnight` (or another theme name) previews a palette without changing the saved preference.

## Icon

`assets/make_icon.py` (Python with Pillow) draws the icon. Run `python -I assets/make_icon.py` to regenerate `assets/icon.ico`, which `build.rs` embeds in the Windows executable, `assets/icon.png`, used for the macOS and Linux packages, and `assets/icon.rgba`, which Relay uses for its window icon.

## Releasing

1. Update `version` in `Cargo.toml` and run `cargo check` so `Cargo.lock` follows.
2. Commit, then tag and push: `git tag v0.3.0 && git push origin v0.3.0`.

The [Release workflow](.github/workflows/release.yml) checks that the tag matches `Cargo.toml`, builds Windows (x64, Arm64), a universal macOS app, and Linux (x86_64, aarch64) archives, and publishes them with SHA-256 checksums and generated release notes. Pull requests that touch packaging build the same archives without publishing them, and the workflow can also be started by hand from the Actions tab.
