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
- Click **Connect**, double-click a row, or select it and press `Enter` to open a tab.
- Sidebar filters and groups narrow the library. Search matches names, addresses, usernames, groups, and protocols.
- Keyboard: `Ctrl+K` searches; `↑`/`↓` move the selection and `Enter` connects (from the search box or the list); `Esc` clears the search and closes dialogs; `Enter` saves the connection editor. `Ctrl+N` adds a profile, `Ctrl+W` closes the selected tab, `Ctrl+1`–`Ctrl+9` jump to tabs, and `Ctrl+B` toggles the sidebar. `Ctrl+Tab`/`Ctrl+Shift+Tab` cycle tabs, even from an SSH terminal; the terminal receives the other shortcuts while focused. Click the manager chrome to regain focus.
- Below 1000 px wide the sidebar collapses to icons, and narrow tables hide the group and then the protocol column. The window can shrink to 720×560.
- Session tabs show a status dot: accent while open, red once the session has ended.
- **Connection library** returns to the list while sessions stay open.
- Closing a tab or quitting with active sessions asks for confirmation.
- The **···** menu edits, duplicates, copies the address of, or deletes a profile. Changes apply to future connections. The editor suggests existing groups.

The personal library is `%LOCALAPPDATA%\Relay\connections.json`. Writes use atomic replacement. Invalid libraries remain untouched and cannot be overwritten through the editor. Profiles contain plaintext hostnames, usernames, groups, and imported metadata; remote passwords are never stored.

## CSV import and export

Open **Settings → Import CSV** and review the preview before committing. The importer supports the full export schema (see [tests/fixtures/export-sample.csv](tests/fixtures/export-sample.csv)), including semicolon delimiters, quoted fields, UTF-8, UTF-16, and Windows-1252. A simpler `Name,Host,Protocol,Port,Username,Group,Favorite` schema also works. Blank ports use protocol defaults. Blank destinations fall back to `ConnectionIpAddress`.

RDP and SSH records can be imported; VNC and other protocols are listed as unsupported in the preview. Duplicates are skipped using source IDs, or an endpoint/name/group identity when an ID is absent. Existing profiles are not modified by import.

**Settings → Export CSV** writes the whole current library as semicolon-delimited CSV. Known source metadata (description, external ID, image ID, URL, IP/MAC, and custom fields) is retained; credential columns are not imported. Cells starting with spreadsheet formula characters (`= + - @`, tab, carriage return) are prefixed with an apostrophe for safety. Relay removes that prefix on reimport, so exported values round-trip exactly. Exports replace files atomically after the save dialog confirms an overwrite.

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

Use an explicit TCP port. Relay requires encrypted transport, uses Windows certificate trust by default, and supports Windows integrated authentication or SQL logins. By default the connection string stays in memory for the session and is not saved to disk. Tick **Remember on this computer** to store it in `%LOCALAPPDATA%\Relay\mssql.bin`, encrypted with Windows DPAPI for your user account; it is saved only after the library opens, and opening without the option deletes it. The app does not create the database or alter other tables. A library is one revisioned JSON document in `dbo.RelayLibrary`.

### SQLite file

Choose **SQLite file** and enter a local, mapped-drive, or UNC path such as `\\fileserver\team\relay.sqlite`. A missing database file is created automatically; its parent directory must exist. All users need read/write/create/delete permissions in that directory for database journals.

SQLite uses rollback journaling, full synchronization, a five-second busy timeout, and revision checks. **MSSQL is the preferred team backend.** SQLite on a share depends on reliable filesystem locking; broken locks or interrupted network I/O can still cause corruption. Keep backups and test your actual share. See [SQLite's network filesystem guidance](https://www.sqlite.org/useovernet.html).

### Concurrent edits

Click **Refresh** to load teammates' changes. Shared saves compare the revision read by your client and reject stale updates. Failed changes remain in memory with **Export unsaved changes** available. Export, refresh, and reapply deliberately. Quitting warns if unsaved changes remain. Database operations run off the UI thread; MSSQL operations have a 15-second timeout. If a save times out, refresh before retrying because the server may have committed it.

## Session limits

RDP is hosted directly inside each tab using the Windows desktop RDP control. Relay does not launch `mstsc.exe` or generate RDP files. Windows asks for credentials and offers **Remember me**, which stores them in Windows Credential Manager (`TERMSRV/<host>`), exactly like Remote Desktop Connection; Relay never receives or stores passwords. **··· → Forget saved password** removes them. Windows authentication and certificate dialogs can appear outside the tab. Certificate verification and NLA remain enabled.

By default the remote resolution follows the tab, in physical pixels, and the remote display scale matches the monitor Relay is on (for example 150% on a high-DPI laptop), so text keeps its usual size. After login, resizing, or moving to a monitor with a different scale, Relay updates the remote display; smart sizing covers the moment in between and servers that cannot change resolution. A failed display update shows a warning in the tab. Closing RDP disconnects without logging out the remote Windows session.

Sessions reconnect automatically after network drops or sleep (up to 20 attempts, shown as *Reconnecting…*). If a session ends, **Reconnect** reopens it in the same tab with the connection's current settings. **Full screen** hands the session to the Windows full-screen mode with its connection bar; `Ctrl+Alt+Break` toggles it.

While a remote desktop has the keyboard, it receives normal shortcuts. Relay reserves three: `Ctrl+Alt+PageUp`/`Ctrl+Alt+PageDown` switch tabs and `Ctrl+Alt+Home` returns the keyboard to Relay. These need Relay in the foreground and do not apply in full screen or when Windows key combinations are set to *On the remote computer*.

### Remote desktop options

Each RDP connection has **Remote desktop options** in its editor: an optional **RD Gateway** host (used with the same credentials), which local resources to use on the remote computer (clipboard, local drives, printers, smart cards, microphone), where remote audio plays, where Windows key combinations such as `Alt+Tab` apply (default: remote only in full screen), and the resolution (fit to tab, or a fixed size scaled to the tab). Options are saved with the connection in personal and shared libraries; CSV export does not include them.

SSH supports terminal colors, scrolling (5,000 lines), resizing, bracketed paste, navigation/function/control/Alt keys, and copying the visible screen. Click inside to type; Tab, Escape, and the arrow keys stay in the terminal. `Ctrl+C` sends an interrupt, `Ctrl+Shift+C` copies the screen, and `Ctrl+V` or `Shift+Insert` pastes. Mouse reporting, text selection, hyperlinks, and complete xterm compatibility are not implemented. Closing SSH may terminate remote foreground work.

## Verification

```powershell
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

Tests cover a sample CSV in the export schema (including UTF-16), formula-guard round trips, duplicate detection, validation, serialization, atomic saves, corrupt-file preservation, SQLite stale-write rejection, MSSQL configuration, terminal parsing/keys and focus, and real ConPTY input/output/resize. The native RDP check creates the actual installed ActiveX control, verifies credential prompting and both host/control dimensions at two viewport sizes, and never contacts a remote host. Live MSSQL access, RDP/SSH authentication, server display negotiation, and SMB locking require your infrastructure.

An optional `ui-check` feature captures rendered screens without writing profiles:

```powershell
$env:RELAY_UI_CHECK = "$PWD\target\preview.png"
cargo run --features ui-check
```

Use `RELAY_UI_FIXTURE=1` to render the sample CSV, `RELAY_UI_IMPORT=1` for its preview, `RELAY_UI_EDITOR=1` for the editor, or `RELAY_UI_DATABASE=1` for database setup. Fixtures are excluded from the normal build.

Set `RELAY_UI_THEME=Midnight` (or another theme name) with `ui-check` to preview a palette without changing the saved preference.

Use `RELAY_UI_SETTINGS=1` with `ui-check` to preview the settings menu.

Use `RELAY_UI_TABS=1` with `ui-check` to preview session tabs without opening remote connections.
