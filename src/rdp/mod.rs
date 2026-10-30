//! Remote desktops, drawn inside Relay's windows. On Windows they use Microsoft's RDP ActiveX
//! control, the one Remote Desktop Connection uses; elsewhere IronRDP renders them into the
//! tab. Both backends offer the same `RdpClient` interface and constants, so a platform can
//! switch by changing the selection below.
#[cfg(windows)]
mod activex;
#[cfg(not(windows))]
mod ironrdp;

#[cfg(windows)]
pub use activex::{
    ADMIN_SESSIONS, FULL_SCREEN_HELP, FULL_SCREEN_IN_WINDOW, RdpClient, SAVES_PASSWORDS,
    SIGN_IN_HELP, forget_credentials, unsupported,
};
#[cfg(not(windows))]
pub use ironrdp::{
    ADMIN_SESSIONS, FULL_SCREEN_HELP, FULL_SCREEN_IN_WINDOW, RdpClient, SAVES_PASSWORDS,
    SIGN_IN_HELP, forget_credentials, unsupported,
};
