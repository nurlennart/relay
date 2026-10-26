//! Remote desktops. On Windows they are embedded in Relay's window with Microsoft's RDP
//! ActiveX control. Elsewhere FreeRDP opens them in its own window, and its prompts show in
//! the session's tab like an SSH terminal.
#[cfg(windows)]
mod activex;
mod freerdp;
#[cfg(not(windows))]
mod unembedded;

#[cfg(windows)]
pub use activex::{RdpClient, SAVES_PASSWORDS, SIGN_IN_HELP, forget_credentials};
pub use freerdp::command as freerdp_command;
#[cfg(not(windows))]
pub use unembedded::{RdpClient, SAVES_PASSWORDS, SIGN_IN_HELP, forget_credentials};

/// Whether remote desktops are drawn inside Relay's windows rather than in their own.
pub const EMBEDDED: bool = cfg!(windows);
