# Relay

A native Windows remote connection manager written in Rust. A dark, GPU-rendered workspace with a searchable library, group filters, favorites, and tabs for RDP and SSH sessions.

## Run

Open `target\release\relay.exe`, or build with the Rust MSVC toolchain and Visual Studio C++ Build Tools:

```powershell
cargo run --release
```

Requires Windows 10/11 and DirectX 12. RDP uses the installed Windows desktop RDP ActiveX control and system ATL runtime. SSH requires the Windows **OpenSSH Client** optional feature and runs inside a ConPTY terminal rendered by Relay. OpenSSH handles keys, SSH config, agent, host verification, and password prompts.

## Color themes

Open **Settings** at the bottom of the sidebar and use **Appearance** to switch between **Graphite** (mint), **Midnight** (blue), **Amethyst** (violet), and **Ember** (amber), **Daylight** (cool light), and **Linen** (warm light). Changes apply immediately across the workspace and dialogs. Your preference is saved locally in `%LOCALAPPDATA%\Relay\theme.json`, independently of personal or shared connection libraries. Remote desktops and application-specified SSH terminal colors keep their own appearance.

## Connection library

- **New connection** saves a name, host, port, optional username, group, and favorite.
- Click **Connect** or double-click a name to open a tab.
- Sidebar filters and groups narrow the library. Search matches names, addresses, usernames, groups, and protocols.
- The **···** menu edits or deletes a profile. Changes apply to future connections.
- **Connection library** returns to the list while sessions stay open.
- Closing a tab or quitting with active sessions asks for confirmation.
- `Ctrl+K` searches, `Ctrl+N` adds a profile, and `Ctrl+W` closes the selected tab when the manager has focus. SSH receives these keys while its terminal is focused. Click the manager chrome to regain focus.

The personal library is `%LOCALAPPDATA%\Relay\connections.json`. Writes use atomic replacement. Invalid libraries remain untouched and cannot be overwritten through the editor. Profiles contain plaintext hostnames, usernames, groups, and imported metadata; remote passwords are never stored.

## CSV import and export

Open **Settings → Import CSV** and review the preview before committing. The importer supports the supplied `export.csv` schema, including semicolon delimiters, quoted fields, UTF-8, UTF-16, and Windows-1252. A simpler `Name,Host,Protocol,Port,Username,Group,Favorite` schema also works. Blank ports use protocol defaults. Blank destinations fall back to `ConnectionIpAddress`.

The supplied export contains **69 RDP, 2 SSH, and 8 VNC records**. RDP and SSH can be imported; VNC is explicitly listed as unsupported in the preview. Duplicates are skipped using source IDs, or an endpoint/name/group identity when an ID is absent. Existing profiles are not modified by import.

**Settings → Export CSV** writes the whole current library as semicolon-delimited CSV. Known source metadata (description, external ID, image ID, URL, IP/MAC, and custom fields) is retained; credential columns are not imported. Cells starting with spreadsheet formula characters are prefixed with an apostrophe for safety. That prefix remains literal on reimport. Exports replace files atomically after the save dialog confirms an overwrite.

## Shared team libraries

Open **Settings → Manage library**. Switching does not move or merge profiles and does not close sessions. Use CSV to populate an empty shared library. Library selection is session-local; the personal library remains the startup default.

### MSSQL

1. Use an existing SQL Server 2016+ database and run [sql/mssql.sql](sql/mssql.sql) once.
2. Give your team `SELECT` and `UPDATE` permissions on `dbo.RelayLibrary`.
3. Choose **MSSQL server** and enter an ADO.NET connection string.

Windows authentication:

```text
Server=tcp:sql.example.com,1433;Database=Relay;Integrated Security=true;Encrypt=true;TrustServerCertificate=false
```

SQL authentication:

```text
Server=tcp:sql.example.com,1433;Database=Relay;User ID=relay;Password=your-password;Encrypt=true;TrustServerCertificate=false
```

Use an explicit TCP port. Relay requires encrypted transport, uses Windows certificate trust by default, and supports Windows integrated authentication or SQL logins. Database connection strings stay in memory for the session and are not saved to disk. The app does not create the database or alter other tables. A library is one revisioned JSON document in `dbo.RelayLibrary`.

### SQLite file

Choose **SQLite file** and enter a local, mapped-drive, or UNC path such as `\\fileserver\team\relay.sqlite`. A missing database file is created automatically; its parent directory must exist. All users need read/write/create/delete permissions in that directory for database journals.

SQLite uses rollback journaling, full synchronization, a five-second busy timeout, and revision checks. **MSSQL is the preferred team backend.** SQLite on a share depends on reliable filesystem locking; broken locks or interrupted network I/O can still cause corruption. Keep backups and test your actual share. See [SQLite's network filesystem guidance](https://www.sqlite.org/useovernet.html).

### Concurrent edits

Click **Refresh** to load teammates' changes. Shared saves compare the revision read by your client and reject stale updates. Failed changes remain in memory with **Export unsaved changes** available. Export, refresh, and reapply deliberately. Quitting warns if unsaved changes remain. Database operations run off the UI thread; MSSQL operations have a 15-second timeout. If a save times out, refresh before retrying because the server may have committed it.

## Session limits

RDP is hosted directly inside each tab using the Windows desktop RDP control. Relay no longer launches `mstsc.exe` or generates temporary RDP files. Windows credential prompting is enabled and credential saving is disabled; Relay does not receive or store remote passwords. Windows authentication and certificate dialogs can appear outside the tab. Certificate verification and NLA remain enabled; normal security prompts can still appear.

The initial remote desktop resolution uses the actual tab viewport. After login and window resizing, Relay requests updated remote display dimensions, with smart sizing enabled while the size changes or when the server cannot change resolution. A failed display update shows a warning in the tab. The native control and its host window track the viewport together, keeping the session inside the content area. Live server resolution negotiation and mixed-DPI monitor transitions still need testing against your hosts. Closing RDP disconnects without logging out the remote Windows session.

SSH supports terminal colors, scrolling (5,000 lines), resizing, bracketed paste, navigation/function/control keys, and copying the visible screen. Click inside to type. `Ctrl+Shift+C` copies the screen; use the platform paste shortcut to paste. Mouse reporting, text selection, hyperlinks, and complete xterm compatibility are not implemented. Closing SSH may terminate remote foreground work.

## Verification

```powershell
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

Tests cover the supplied CSV, duplicate detection, validation, serialization, atomic saves, corrupt-file preservation, SQLite stale-write rejection, MSSQL configuration, terminal parsing/keys, and real ConPTY input/output/resize. The native RDP check creates the actual installed ActiveX control, verifies credential prompting and both host/control dimensions at two viewport sizes, and never contacts a remote host. Live MSSQL access, RDP/SSH authentication, server display negotiation, and SMB locking require your infrastructure.

An optional `ui-check` feature captures rendered screens without writing profiles:

```powershell
$env:RELAY_UI_CHECK = "$PWD\target\preview.png"
cargo run --features ui-check
```

Use `RELAY_UI_FIXTURE=1` to render the supplied CSV, `RELAY_UI_IMPORT=1` for its preview, `RELAY_UI_EDITOR=1` for the editor, or `RELAY_UI_DATABASE=1` for database setup. Fixtures are excluded from the normal build.

Set `RELAY_UI_THEME=Midnight` (or another theme name) with `ui-check` to preview a palette without changing the saved preference.

Use `RELAY_UI_SETTINGS=1` with `ui-check` to preview the settings menu.

Use `RELAY_UI_TABS=1` with `ui-check` to preview session tabs without opening remote connections.
