# Contributing

## Building

Relay builds with stable Rust (1.94 or newer, which IronRDP requires) on Windows, macOS, and Linux:

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
| `src/app/` | The `App` state and frame loop (`mod.rs`) and its screens: `sidebar`, `tabs`, `library`, `sessions`, `files` (the SFTP browser), `editor`, `settings`, `storage`, plus shared `widgets` and `icons` and the UI tests. |
| `src/platform/` | Everything that differs per OS: data folders, fonts, the ssh binary, and the Windows-only native window code (`windows.rs`); `other.rs` covers macOS and Linux. |
| `src/rdp/` | Remote desktops: the Windows ActiveX control (`activex.rs`) and the IronRDP client used elsewhere (`ironrdp.rs`, with its connection thread, input mapping, frame bookkeeping, and clipboard in `ironrdp/`). Both expose the same `RdpClient` interface. |
| `src/session.rs`, `src/terminal.rs` | Open tabs and the terminal SSH sessions run in. |
| `src/sftp.rs`, `src/askpass.rs` | SFTP over the system's ssh (a worker thread running russh-sftp on ssh's standard input and output, and the transfer queue), and the `SSH_ASKPASS` helper that brings ssh's prompts into the window. |
| `src/model.rs`, `src/database.rs`, `src/state.rs` | Connections, libraries (JSON, SQLite, MSSQL, MySQL), and saved workspace state. |
| `src/csv_io.rs`, `src/import.rs` | CSV import and export, SSH config and `.rdp` import. |
| `src/secret.rs`, `src/file_dialog.rs`, `src/theme.rs` | Credential store, file pickers, color themes. |
| `packaging/` | The macOS app bundle and Linux desktop entry used by releases. |

Platform-specific code stays in `src/platform` and `src/rdp`; the rest of the app uses their cross-platform interface rather than `cfg` attributes. `src/rdp/mod.rs` picks the remote desktop backend per platform; the IronRDP backend has no platform-specific code, so Windows could use it by changing that selection.

## Checks

CI runs these on Windows, macOS, and Linux for every pull request:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Tests cover the CSV export schema (including UTF-16), formula-guard round trips, duplicate detection, validation, serialization, atomic saves, corrupt-file preservation, SQLite stale-write rejection, MSSQL and MySQL configuration (TLS on and off), MSSQL named instances resolved through a stand-in SQL Server Browser, IronRDP input mapping, option and connector configuration, certificate trust, clipboard text, frame and texture bookkeeping, terminal parsing, keys and focus, a real pseudo-terminal's input, output, and resize, the askpass round trip, and SFTP browsing, renaming, moving, deleting, streamed transfers in both directions, overwrite protection and cancellation against OpenSSH's `sftp-server` (skipped where it is not installed, as on Windows). On Windows, the RDP checks create the installed ActiveX control and verify credential prompting and viewport sizes without contacting a remote host. RDP sessions, SSH authentication, and SMB locking need real infrastructure. Live database checks are opt-in; each runs the schema script, then saves, reads, and rejects a stale write, and confirms the session's encryption matches the TLS setting:

```sh
RELAY_TEST_MSSQL='Server=tcp:127.0.0.1,1433;Database=master;User ID=sa;Password=...;TrustServerCertificate=true' \
  cargo test -- --ignored mssql_live
RELAY_TEST_MYSQL='mysql://relay:...@127.0.0.1:3306/relay?danger_accept_invalid_certs=true&danger_skip_domain_validation=true' \
  cargo test -- --ignored mysql_live
```

Set `RELAY_TEST_MSSQL_TLS=0` or `RELAY_TEST_MYSQL_TLS=0` to test without TLS.

The live SFTP check signs in with a password through ssh and Relay's askpass helper, accepting the host key into a temporary `known_hosts`, and transfers a file both ways. Build Relay first, since ssh runs it as the helper:

```sh
cargo build
RELAY_TEST_SFTP=user@127.0.0.1:2222 RELAY_TEST_SFTP_PASSWORD=... cargo test -- --ignored sftp_live
``` Both run against throwaway containers (`mcr.microsoft.com/mssql/server`, `mysql`, `mariadb`); they drop and recreate `RelayLibrary`.

### Rendering checks

The optional `ui-check` feature captures rendered screens without writing profiles:

```sh
RELAY_UI_CHECK="$PWD/target/preview.png" cargo run --features ui-check
```

Add `RELAY_UI_FIXTURE=1` to render the sample CSV, `RELAY_UI_EMPTY=1` for an empty library, `RELAY_UI_IMPORT=1` for its preview, `RELAY_UI_EDITOR=1` for the editor, `RELAY_UI_DATABASE=1` (or `=mysql`, with `RELAY_UI_NO_TLS=1` to show TLS turned off) for database setup, `RELAY_UI_SETTINGS=1` for the settings menu (or `=terminal`, `library` or `snippets` for that tab), `RELAY_UI_TABS=1` for session tabs, or `RELAY_UI_TERMINAL=1 RELAY_UI_LIBRARY=1` for the library with a session open. `RELAY_UI_FILES=1` shows the file browser with transfers (add `RELAY_UI_SPLIT=1` to put it beside a terminal), `RELAY_UI_FILES=queue` a transfer queue long enough to scroll, and `RELAY_UI_FILES=hostkey` or `=password` its sign-in prompts. `RELAY_UI_THEME=Midnight` (or another theme name) previews a palette without changing the saved preference. `RELAY_UI_ZOOM=3` enlarges everything, to inspect icons and other small details. `RELAY_UI_FILTER=favorites` (or `recent`) opens that view, `RELAY_UI_NO_FAVORITES=1` clears the sample favorites, and `RELAY_UI_ENDED=1` shows the terminal session as ended. With `RELAY_UI_SETTINGS=library`, `RELAY_UI_LOCATION=sqlite`, `mssql`, `mssql-forgotten` or `mysql` shows that kind of library as open. `RELAY_UI_HOVER=x,y` moves the pointer there to show hover states, and `RELAY_UI_CONTEXT=x,y` right-clicks there first to open a context menu.

## Icon

`assets/make_icon.py` (Python with Pillow) draws the icon. Run `python -I assets/make_icon.py` to regenerate `assets/icon.ico`, which `build.rs` embeds in the Windows executable, `assets/icon.png`, used for the macOS and Linux packages, and `assets/icon.rgba`, which Relay uses for its window icon.

## Releasing

1. Update `version` in `Cargo.toml` and run `cargo check` so `Cargo.lock` follows.
2. Commit, then tag and push: `git tag v0.3.0 && git push origin v0.3.0`.

The [Release workflow](.github/workflows/release.yml) checks that the tag matches `Cargo.toml`, builds Windows (x64, Arm64), a universal macOS app, and Linux (x86_64, aarch64) archives, and publishes them with SHA-256 checksums and generated release notes. Pull requests that touch packaging build the same archives without publishing them, and the workflow can also be started by hand from the Actions tab.
