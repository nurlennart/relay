# Relay

A remote connection manager for Windows, macOS, and Linux, written in Rust. A dark, GPU-rendered workspace with a searchable library, group filters, favorites, and tabs for RDP and SSH sessions.

## Install

Download the archive for your platform from the [latest release](https://github.com/nurlennart/relay/releases/latest).

| Platform | Download | Notes |
| --- | --- | --- |
| Windows 10/11 (x64, Arm64) | `relay-<version>-windows-*.zip` | Unzip and run `relay.exe`. SSH uses the **OpenSSH Client** optional feature. |
| macOS 11+ (Apple silicon and Intel) | `relay-<version>-macos-universal.zip` | Unzip and move **Relay.app** to Applications. The app is not notarized: the first time, right-click it and choose **Open**. |
| Linux (x86_64, aarch64) | `relay-<version>-linux-*.tar.gz` | Extract and run `./install.sh` to install into `~/.local`. |

SSH uses the system's OpenSSH client everywhere, so your SSH config, keys, and agent work as usual.

### How remote desktops open

- **Windows:** the remote desktop is embedded in the tab with the Windows RDP control, the same one Remote Desktop Connection uses.
- **macOS and Linux:** Relay's built-in RDP client, based on [IronRDP](https://github.com/Devolutions/IronRDP), draws the remote desktop in the tab. Nothing else needs to be installed.

To build from source, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Color themes

Open **Settings** at the bottom of the sidebar and use **Appearance** to switch between **Graphite** (mint), **Midnight** (blue), **Amethyst** (violet), and **Ember** (amber), **Daylight** (cool light), and **Linen** (warm light). Changes apply immediately across the workspace and dialogs. Your preference is saved locally in `theme.json` in Relay's [data folder](#where-relay-keeps-data), independently of personal or shared connection libraries. Remote desktops and application-specified SSH terminal colors keep their own appearance.

## Connection library

- **New connection** saves a name, host, port, optional username, group, and favorite.
- Click **Connect**, double-click a row, or select it and press `Enter` to open a tab.
- Sidebar filters and groups narrow the library. Search matches names, addresses, usernames, groups, and protocols.
- **Quick connect:** type a host in search and press `Enter` when nothing in the library matches, or `Alt+Enter` to connect directly even when something does. Search accepts `host`, `host:port`, `user@host`, `[IPv6]:port`, and an `ssh`/`rdp` prefix or `ssh://`/`rdp://`. Without a prefix, `user@host` and port 22 mean SSH and anything else RDP. An unsaved session offers **Save to library**.
- Keyboard (use `Cmd` instead of `Ctrl` on macOS): `Ctrl+K` searches; `↑`/`↓` move the selection and `Enter` connects (from the search box or the list); `Esc` clears the search and closes dialogs; `Enter` saves the connection editor. `Ctrl+N` adds a profile, `Ctrl+W` closes the selected tab, `Ctrl+1`–`Ctrl+9` jump to tabs, and `Ctrl+B` toggles the sidebar. `Ctrl+Tab`/`Ctrl+Shift+Tab` cycle tabs, even from an SSH terminal; the terminal receives the other shortcuts while focused. Click the manager chrome to regain focus.
- Below 1000 px wide the sidebar collapses to icons, and narrow tables hide the group and then the protocol column. The window can shrink to 720×560.
- Session tabs show a status dot: accent while open, red once the session has ended. Drag a tab to reorder it, or use `Ctrl+Shift+PageUp`/`Ctrl+Shift+PageDown`.
- **Recent** lists the connections you opened in the current library, newest first.
- Relay remembers the window size, the sidebar choice, the last library, recent connections, snippets and open tabs in `state.json`. A shared SQLite library reopens at startup, and an MSSQL library reopens when its connection string is remembered. No connection strings are written to this file. Open tabs come back at startup; each connects when it is first selected, starting with the tab that was selected, so a restart does not open every login prompt at once.
- **Connection library** returns to the list while sessions stay open.
- Closing a tab or quitting with active sessions asks for confirmation.
- The **···** menu edits, duplicates, copies the address of, or deletes a profile. Changes apply to future connections. The editor suggests existing groups.
- **Split view:** right-click a tab and choose **Open beside current tab**, or press `Ctrl+\` to pair the current tab with its neighbour. Drag the divider to resize; selecting the right-hand tab swaps the sides. **Move to new window** in the same menu detaches a session into its own window; its tab brings the window forward, and **Back to tabs** or closing the window returns it.

### Where Relay keeps data

| Platform | Folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%\Relay` |
| macOS | `~/Library/Application Support/Relay` |
| Linux | `$XDG_DATA_HOME/relay` (usually `~/.local/share/relay`) |

The personal library is `connections.json` in that folder. Writes use atomic replacement. Invalid libraries remain untouched and cannot be overwritten through the editor. Profiles contain plaintext hostnames, usernames, groups, and imported metadata; remote passwords are never stored.

## Import and export

**Settings → Import SSH config** reads `~/.ssh/config` (`%USERPROFILE%\.ssh\config` on Windows). Each concrete `Host` alias becomes an SSH profile with its `HostName`, `User`, `Port`, `ProxyJump`, `IdentityFile`, and local, remote and dynamic forwards. As in OpenSSH, an alias collects options from every matching block, including wildcard and negated patterns, and the first value wins. `Match` blocks and `Include` are listed as skipped.

**Settings → Import .rdp files** accepts one or more Remote Desktop files (UTF-16 or UTF-8) and maps the address, user, RD Gateway, redirections, audio, keyboard and resolution settings.

Every import shows a review before anything is saved, skips duplicates, and lists skipped entries with reasons.

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

Use an explicit TCP port. Relay requires encrypted transport and trusts the system's certificate store. SQL logins work everywhere; Windows integrated authentication works on Windows. By default the connection string stays in memory for the session and is not saved to disk. Tick **Remember on this computer** to keep it in the system credential store (Windows Credential Manager, the macOS Keychain, or the Secret Service on Linux, such as GNOME Keyring or KWallet); it is saved only after the library opens, and opening without the option removes it. The app does not create the database or alter other tables. A library is one revisioned JSON document in `dbo.RelayLibrary`.

### SQLite file

Choose **SQLite file** and enter a local, mapped-drive, or UNC path such as `\\fileserver\team\relay.sqlite`. A missing database file is created automatically; its parent directory must exist. All users need read/write/create/delete permissions in that directory for database journals.

SQLite uses rollback journaling, full synchronization, a five-second busy timeout, and revision checks. **MSSQL is the preferred team backend.** SQLite on a share depends on reliable filesystem locking; broken locks or interrupted network I/O can still cause corruption. Keep backups and test your actual share. See [SQLite's network filesystem guidance](https://www.sqlite.org/useovernet.html).

### Concurrent edits

Click **Refresh** to load teammates' changes. Shared saves compare the revision read by your client and reject stale updates. Failed changes remain in memory with **Export unsaved changes** available. Export, refresh, and reapply deliberately. Quitting warns if unsaved changes remain. Database operations run off the UI thread; MSSQL operations have a 15-second timeout. If a save times out, refresh before retrying because the server may have committed it.

## Sessions

### Remote desktops on Windows

RDP is hosted directly inside each tab using the Windows desktop RDP control. Relay does not launch `mstsc.exe` or generate RDP files. Windows asks for credentials and offers **Remember me**, which stores them in Windows Credential Manager (`TERMSRV/<host>`), exactly like Remote Desktop Connection; Relay never receives or stores passwords. **··· → Forget saved password** removes them. Windows authentication and certificate dialogs can appear outside the tab. Certificate verification and NLA remain enabled.

By default the remote resolution follows the tab, in physical pixels, and the remote display scale matches the monitor Relay is on (for example 150% on a high-DPI laptop), so text keeps its usual size. After login, resizing, or moving to a monitor with a different scale, Relay updates the remote display; smart sizing covers the moment in between and servers that cannot change resolution. A failed display update shows a warning in the tab. Closing RDP disconnects without logging out the remote Windows session.

Sessions reconnect automatically after network drops or sleep (up to 20 attempts, shown as *Reconnecting…*). If a session ends, **Reconnect** reopens it in the same tab with the connection's current settings. **Full screen** hands the session to the Windows full-screen mode with its connection bar; `Ctrl+Alt+Break` toggles it.

While a remote desktop has the keyboard, it receives normal shortcuts. Relay reserves three: `Ctrl+Alt+PageUp`/`Ctrl+Alt+PageDown` switch tabs and `Ctrl+Alt+Home` returns the keyboard to Relay. These need Relay in the foreground and do not apply in full screen or when Windows key combinations are set to *On the remote computer*.

### Remote desktops on macOS and Linux

The remote desktop is drawn inside the tab by Relay's built-in client. The tab asks for the user name (`DOMAIN\user` or `user@domain`) and password each time it connects; the password stays in memory only while the session is open and is never saved. Sign-in uses NLA (CredSSP with NTLM) when the server supports it, and TLS otherwise. A certificate your system does not trust, such as a self-signed one, is shown with its SHA-256 fingerprint for you to accept; Relay then trusts it for that host until it quits.

Click the desktop to give it the keyboard; click elsewhere in Relay to take it back. Keys are sent by their position, so the remote computer's keyboard layout applies, and on macOS `Cmd` acts as `Ctrl`. `Ctrl+Alt+PageUp`/`PageDown` (or `Ctrl+Tab`) still switch tabs. **Fit to tab** changes the remote resolution as the tab is resized (through the display control channel; servers without it are scaled instead, with a warning), and **Fixed** scales a fixed resolution to the tab. Clipboard text is shared in both directions. Sessions reconnect after network drops (up to 5 attempts); when one ends, **Reconnect** asks for the password again.

Some options are not available here and are listed in the connection editor when selected: local drives, printers, smart cards, microphone, remote audio (sound is turned off), and sending system key combinations such as `Alt+Tab` to the remote computer. There is no full-screen mode and no Kerberos sign-in.

### Remote desktop options

Each RDP connection has **Remote desktop options** in its editor: an optional **RD Gateway** host (used with the same credentials), which local resources to use on the remote computer (clipboard, local drives, printers, smart cards, microphone), where remote audio plays, where Windows key combinations such as `Alt+Tab` apply (default: remote only in full screen), and the resolution (fit to tab, or a fixed size scaled to the tab). Options are saved with the connection in personal and shared libraries; CSV export does not include them.

### SSH

SSH runs in a dark terminal panel that keeps its own colors in every app theme, using the system's monospace font (Cascadia Mono or Consolas on Windows, SF Mono or Menlo on macOS, DejaVu Sans Mono or Noto Sans Mono on Linux). It supports 256 and true colors, bold, dim, underline and inverse text, scrolling through 5,000 lines with a scrollbar, resizing, bracketed paste, and navigation, function, control and Alt keys. The terminal is focused when its tab opens; Tab, Escape and the arrow keys stay in it. `Ctrl+C` sends an interrupt. Drag to select text; `Ctrl+Shift+C` (or `Cmd+C` on macOS) or a right-click copies the selection (or the whole screen when nothing is selected), and a right-click without a selection, `Ctrl+V` or `Shift+Insert` pastes. Mouse reporting, hyperlinks and complete xterm compatibility are not implemented. Closing SSH may terminate remote foreground work.

SSH profiles have **SSH options**: a jump host (`ssh -J`), an identity file (`ssh -i`), and port forwards written as `L8080:localhost:80`, `R9000:localhost:9000` or `D1080`, separated by commas. Relay passes `ServerAliveInterval=15`, so a dropped connection is noticed within about 45 seconds. When ssh exits with 255 after the session had been up, Relay reconnects in the same tab after 1, 2, 4, 8, 16, then 30 seconds, up to ten times; **Reconnect now** and **Stop** are shown meanwhile. Wrong passwords, unknown hosts and `exit` are not retried.

**Snippets** are saved commands, managed in **Settings → Snippets**. In an SSH tab, `Ctrl+Shift+P` or **Snippets** opens a palette: type to filter, `↑`/`↓` and `Enter` send, `Esc` closes. A snippet types its command and can press Enter afterwards; multi-line snippets use bracketed paste.
