//! Microsoft's RDP ActiveX control, hosted through ATL in a child window of Relay.
use crate::{
    model::{Audio, Connection, Display, Keys},
    platform::{self, Window as NativeWindow},
};
use eframe::egui;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use windows::{
    Win32::System::{
        Com::*,
        Ole::{OleInitialize, OleUninitialize},
        Variant::VARIANT,
    },
    core::{BSTR, GUID, HRESULT, IUnknown, Interface, PCWSTR, implement},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::*,
    UI::HiDpi::GetDpiForWindow,
    UI::WindowsAndMessaging::*,
};

/// The window class of the viewport that holds the RDP control, registered once.
fn viewport_class() -> *const u16 {
    static CLASS: OnceLock<Vec<u16>> = OnceLock::new();
    CLASS
        .get_or_init(|| {
            let name: Vec<u16> = "RelayRdpViewport\0".encode_utf16().collect();
            unsafe {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(viewport_proc),
                    hInstance: GetModuleHandleW(std::ptr::null()),
                    hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                    lpszClassName: name.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&class);
            }
            name
        })
        .as_ptr()
}

/// Answers which cursor to show over the remote desktop. The control leaves that question to
/// its parents, and Relay's window would answer with whatever egui showed last, such as split
/// view's resize arrow; the remote computer's pointer changes the cursor from there.
unsafe extern "system" fn viewport_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_SETCURSOR && (lparam & 0xffff) as u32 == HTCLIENT {
            SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_ARROW));
            return 1;
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

struct Apartment;
impl Apartment {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            OleInitialize(None)?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            OleUninitialize();
        }
    }
}
struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

#[allow(non_snake_case)]
mod extended {
    use windows::{
        Win32::System::Variant::VARIANT,
        core::{HRESULT, IUnknown, IUnknown_Vtbl, interface},
    };
    /// Settings that apply before connecting, such as the initial display scale.
    #[interface("302D8188-0052-4807-806A-362B628F9AC5")]
    pub unsafe trait IMsRdpExtendedSettings: IUnknown {
        pub fn put_Property(&self, name: *const u16, value: *const VARIANT) -> HRESULT;
        pub fn get_Property(&self, name: *const u16, value: *mut VARIANT) -> HRESULT;
    }
}
use extended::IMsRdpExtendedSettings;

#[derive(Default)]
struct State {
    connected: bool,
    ended: bool,
    reconnecting: bool,
    full_screen: bool,
    generation: u64,
    error: Option<String>,
}
#[derive(Clone, Copy)]
enum Event {
    Connected,
    Disconnected,
    Login,
    Reconnecting,
    Reconnected,
    EnterFullScreen,
    LeaveFullScreen,
}

#[implement(IDispatch)]
struct Sink {
    event: Event,
    state: Arc<Mutex<State>>,
    ctx: egui::Context,
}
impl IDispatch_Impl for Sink_Impl {
    fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
        Ok(0)
    }
    fn GetTypeInfo(&self, _: u32, _: u32) -> windows::core::Result<ITypeInfo> {
        Err(HRESULT(0x80004001u32 as i32).into())
    }
    fn GetIDsOfNames(
        &self,
        _: *const GUID,
        _: *const PCWSTR,
        _: u32,
        _: u32,
        _: *mut i32,
    ) -> windows::core::Result<()> {
        Err(HRESULT(0x80020006u32 as i32).into())
    }
    fn Invoke(
        &self,
        _: i32,
        _: *const GUID,
        _: u32,
        _: DISPATCH_FLAGS,
        params: *const DISPPARAMS,
        _: *mut VARIANT,
        _: *mut EXCEPINFO,
        _: *mut u32,
    ) -> windows::core::Result<()> {
        let mut state = self.state.lock().unwrap();
        match self.event {
            Event::Connected | Event::Login | Event::Reconnected => {
                state.connected = true;
                state.ended = false;
                state.reconnecting = false;
                state.generation += 1;
                state.error = None;
            }
            Event::Reconnecting => {
                state.connected = false;
                state.reconnecting = true;
            }
            Event::EnterFullScreen => state.full_screen = true,
            Event::LeaveFullScreen => state.full_screen = false,
            Event::Disconnected => {
                state.connected = false;
                state.ended = true;
                state.reconnecting = false;
                state.full_screen = false;
                let args = unsafe {
                    if params.is_null() || (*params).cArgs == 0 || (*params).rgvarg.is_null() {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts((*params).rgvarg, (*params).cArgs as usize)
                    }
                };
                let reason = args.last().and_then(|v| i32::try_from(v).ok()).unwrap_or(0);
                let detail = args
                    .iter()
                    .find_map(|v| BSTR::try_from(v).ok())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                state.error = Some(format!("RDP session ended (reason {reason}). {detail}"));
            }
        }
        drop(state);
        self.ctx.request_repaint();
        Ok(())
    }
}

pub struct RdpClient {
    window: Window,
    client: IDispatch,
    advanced: IDispatch,
    _host: IUnknown,
    callbacks: Vec<(BSTR, IDispatch)>,
    state: Arc<Mutex<State>>,
    parent: HWND,
    display: Display,
    started: bool,
    size: Option<(i32, i32)>,
    /// The size and scale the session connected with.
    initial: Option<((i32, i32), u32)>,
    /// The size and scale the remote display has now.
    current: Option<((i32, i32), u32)>,
    generation: u64,
    failures: u32,
    retry_at: Instant,
    resized: Instant,
    /// Overlay rectangles currently cut out of the viewport.
    holes: Vec<[i32; 4]>,
    display_warning: Option<String>,
    /// Wakes the window when a display update is due.
    ctx: egui::Context,
    _apartment: Apartment,
}

impl RdpClient {
    pub fn new(
        parent: Option<NativeWindow>,
        connection: &Connection,
        admin: bool,
        ctx: egui::Context,
    ) -> Result<Self, String> {
        let parent = parent
            .map(NativeWindow::hwnd)
            .filter(|window| !window.is_null())
            .ok_or("Cannot obtain the native application window.")?;
        let apartment =
            Apartment::new().map_err(|e| format!("Cannot initialize Windows RDP: {e}"))?;
        let module = atl()?;
        type Init = unsafe extern "system" fn() -> i32;
        type Create = unsafe extern "system" fn(
            *const u16,
            HWND,
            *mut c_void,
            *mut *mut c_void,
            *mut *mut c_void,
            *const GUID,
            *mut c_void,
        ) -> HRESULT;
        let (window, host, client) = unsafe {
            let init: Init = std::mem::transmute(
                GetProcAddress(module as _, c"AtlAxWinInit".as_ptr().cast())
                    .ok_or("Windows ATL initialization is unavailable")?,
            );
            if init() == 0 {
                return Err("Windows ATL control hosting could not be initialized.".into());
            }
            let window = Window(CreateWindowExW(
                0,
                viewport_class(),
                std::ptr::null(),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                parent,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ));
            if window.0.is_null() {
                return Err(format!(
                    "Cannot create RDP viewport: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let create: Create = std::mem::transmute(
                GetProcAddress(module as _, c"AtlAxCreateControlEx".as_ptr().cast())
                    .ok_or("Windows ATL control creation is unavailable")?,
            );
            let class: Vec<u16> = "{A0C63C30-F08D-4AB4-907C-34905D770C7D}\0"
                .encode_utf16()
                .collect();
            let mut host = std::ptr::null_mut();
            let mut control = std::ptr::null_mut();
            create(
                class.as_ptr(),
                window.0,
                std::ptr::null_mut(),
                &mut host,
                &mut control,
                &GUID::zeroed(),
                std::ptr::null_mut(),
            )
            .ok()
            .map_err(|e| format!("Cannot create Windows RDP control: {e}"))?;
            let host = IUnknown::from_raw(host);
            let control = IUnknown::from_raw(control);
            let client: IDispatch = control
                .cast()
                .map_err(|e| format!("Windows RDP control interface is unavailable: {e}"))?;
            (window, host, client)
        };
        let advanced = property(&client, "AdvancedSettings9")?;
        let options = &connection.rdp;
        let mut rdp = Self {
            window,
            client,
            advanced,
            _host: host,
            callbacks: vec![],
            state: Arc::new(Mutex::new(State::default())),
            parent,
            display: options.display,
            started: false,
            size: None,
            initial: None,
            current: None,
            generation: 0,
            failures: 0,
            retry_at: Instant::now(),
            resized: Instant::now(),
            holes: vec![],
            display_warning: None,
            ctx: ctx.clone(),
            _apartment: apartment,
        };
        for (name, value) in [
            ("Server", VARIANT::from(connection.host.as_str())),
            ("UserName", VARIANT::from(connection.username.as_str())),
            ("FullScreenTitle", VARIANT::from(connection.name.as_str())),
        ] {
            rdp.set(name, &value)?;
        }
        for (name, value) in [
            ("RDPPort", VARIANT::from(connection.port as i32)),
            ("EnableCredSspSupport", VARIANT::from(true)),
            ("AuthenticationLevel", VARIANT::from(2u32)),
            ("SmartSizing", VARIANT::from(true)),
            ("EnableAutoReconnect", VARIANT::from(true)),
            ("MaxReconnectAttempts", VARIANT::from(20i32)),
            // What mstsc /admin sets; ConnectToServerConsole is documented as unsupported.
            ("ConnectToAdministerServer", VARIANT::from(admin)),
            ("RedirectClipboard", VARIANT::from(options.clipboard)),
            ("RedirectPrinters", VARIANT::from(options.printers)),
            ("RedirectDrives", VARIANT::from(options.drives)),
            ("RedirectSmartCards", VARIANT::from(options.smart_cards)),
            (
                "AudioCaptureRedirectionMode",
                VARIANT::from(options.microphone),
            ),
        ] {
            invoke(&rdp.advanced, name, DISPATCH_PROPERTYPUT, vec![value])?;
        }
        let secured = property(&rdp.client, "SecuredSettings3")?;
        let keys = match options.keys {
            Keys::Local => 0,
            Keys::Remote => 1,
            Keys::FullScreen => 2,
        };
        let audio = match options.audio {
            Audio::Local => 0,
            Audio::Remote => 1,
            Audio::Off => 2,
        };
        for (name, value) in [("KeyboardHookMode", keys), ("AudioRedirectionMode", audio)] {
            invoke(
                &secured,
                name,
                DISPATCH_PROPERTYPUT,
                vec![VARIANT::from(value)],
            )?;
        }
        if !options.gateway.is_empty() {
            let transport = property(&rdp.client, "TransportSettings2")?;
            // Always use the gateway, with the explicit settings below and shared credentials.
            for (name, value) in [
                ("GatewayHostname", VARIANT::from(options.gateway.as_str())),
                ("GatewayUsageMethod", VARIANT::from(1u32)),
                ("GatewayProfileUsageMethod", VARIANT::from(1u32)),
                ("GatewayCredsSource", VARIANT::from(4u32)),
                ("GatewayCredSharing", VARIANT::from(1u32)),
            ] {
                invoke(&transport, name, DISPATCH_PROPERTYPUT, vec![value])?;
            }
        }
        // Windows shows a "Remember me" option and stores credentials in Credential Manager;
        // Relay never receives them.
        for (name, value) in [
            ("AllowPromptingForCredentials", true),
            ("AllowCredentialSaving", true),
            ("PromptForCredentials", true),
        ] {
            nonscriptable(
                &rdp.client,
                name,
                DISPATCH_PROPERTYPUT,
                vec![VARIANT::from(value)],
            )?;
        }
        for (name, event) in [
            ("OnConnected", Event::Connected),
            ("OnDisconnected", Event::Disconnected),
            ("OnLoginComplete", Event::Login),
            ("OnAutoReconnecting", Event::Reconnecting),
            ("OnAutoReconnected", Event::Reconnected),
            ("OnEnterFullScreenMode", Event::EnterFullScreen),
            ("OnLeaveFullScreenMode", Event::LeaveFullScreen),
        ] {
            let callback: IDispatch = Sink {
                event,
                state: rdp.state.clone(),
                ctx: ctx.clone(),
            }
            .into();
            let name = BSTR::from(name);
            invoke(
                &rdp.client,
                "attachEvent",
                DISPATCH_METHOD,
                vec![VARIANT::from(name.clone()), VARIANT::from(callback.clone())],
            )?;
            rdp.callbacks.push((name, callback));
        }
        Ok(rdp)
    }
    fn set(&self, name: &str, value: &VARIANT) -> Result<(), String> {
        invoke(
            &self.client,
            name,
            DISPATCH_PROPERTYPUT,
            vec![value.clone()],
        )
        .map(|_| ())
    }
    /// Moves the remote desktop into another window, such as a detached session window.
    pub fn set_parent(&mut self, parent: NativeWindow) {
        let parent = parent.hwnd();
        if parent != self.parent {
            unsafe {
                SetParent(self.window.0, parent);
            }
            self.parent = parent;
            // Holes were cut for the old window's overlays, such as the menu that moved the
            // session. Forgetting them is not enough: the new window starts whole, or a stale
            // hole stays open over the remote desktop.
            let _ = platform::windows::clip(self.window.0, [0, 0], &[]);
            self.holes.clear();
        }
    }
    /// The control draws itself and wakes the window only for its own events, such as signing
    /// in or ending, so whether its tab is shown changes nothing.
    pub fn set_shown(&self, _shown: bool) {}

    pub fn display_warning(&self) -> Option<&str> {
        self.display_warning.as_deref()
    }
    pub fn ended(&self) -> bool {
        self.state.lock().unwrap().ended
    }
    pub fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }
    pub fn reconnecting(&self) -> bool {
        self.state.lock().unwrap().reconnecting
    }
    pub fn full_screen(&self) -> bool {
        self.state.lock().unwrap().full_screen
    }
    pub fn enter_full_screen(&self) -> Result<(), String> {
        self.set("FullScreen", &VARIANT::from(true))
    }
    pub fn can_full_screen(&self) -> bool {
        true
    }
    /// The control takes the keyboard natively; its shortcuts arrive through a hook instead.
    pub fn focused(&self) -> bool {
        false
    }
    /// The control reports through callbacks, so there is nothing to collect.
    pub fn poll(&mut self) {}
    /// Leaves the rest of `ui` to the control's native window, which `place` positions there.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        ui.available_rect_before_wrap()
    }
    /// Sets the scale the session starts with; later changes go through display updates.
    fn set_scale(&self, scale: u32) -> Result<(), String> {
        let settings: IMsRdpExtendedSettings = self
            .client
            .cast()
            .map_err(|e| format!("RDP extended settings are unavailable: {e}"))?;
        for (name, value) in [
            ("DesktopScaleFactor", scale),
            ("DeviceScaleFactor", device_scale(scale)),
        ] {
            let name = BSTR::from(name);
            unsafe { settings.put_Property(name.as_ptr(), &VARIANT::from(value)) }
                .ok()
                .map_err(|e| format!("Cannot set RDP {name}: {e}"))?;
        }
        Ok(())
    }
    fn layout(&mut self, [x, y, w, h]: [i32; 4]) -> Result<(), String> {
        let (w, h) = (w.max(1), h.max(1));
        platform::windows::position(self.window.0, self.parent, [x, y, w, h])?;
        let size = match self.display {
            Display::Fit => (w.clamp(200, 8192), h.clamp(200, 8192)),
            Display::Fixed { width, height } => (width as i32, height as i32),
        };
        if self.size != Some(size) {
            self.size = Some(size);
            self.resized = Instant::now();
        }
        if !self.started {
            self.set("DesktopWidth", &VARIANT::from(size.0))?;
            self.set("DesktopHeight", &VARIANT::from(size.1))?;
        }
        Ok(())
    }
    pub fn place(&mut self, rect: Option<[i32; 4]>, holes: &[[i32; 4]]) -> Result<(), String> {
        let Some(rect) = rect else {
            // Typing goes to Relay, such as a dialog that covers the desktop, not the hidden control.
            platform::windows::release_focus(self.window.0, self.parent);
            unsafe {
                ShowWindow(self.window.0, SW_HIDE);
            }
            return Ok(());
        };
        let resized = platform::windows::bounds(self.window.0, self.parent)? != rect;
        self.layout(rect)?;
        if holes != self.holes || (resized && !holes.is_empty()) {
            platform::windows::clip(self.window.0, [rect[2], rect[3]], holes)?;
            self.holes = holes.to_vec();
        }
        unsafe {
            ShowWindow(self.window.0, SW_SHOWNA);
        }
        let scale = scale_for(self.parent);
        if !self.started {
            // If the scale cannot be preset, the session starts at 100% and a display update
            // corrects it after login.
            let initial_scale = if self.set_scale(scale).is_ok() {
                scale
            } else {
                100
            };
            invoke(&self.client, "Connect", DISPATCH_METHOD, vec![])?;
            self.started = true;
            self.initial = self.size.map(|size| (size, initial_scale));
        }
        let state = self.state.lock().unwrap();
        let (connected, generation) = (state.connected, state.generation);
        drop(state);
        self.sync_display(scale, connected, generation);
        Ok(())
    }

    /// Asks the session to match the tab's size and scale once it can.
    fn sync_display(&mut self, scale: u32, connected: bool, generation: u64) {
        /// Failed updates retry this often; the session rejects them until login completes.
        const RETRY: Duration = Duration::from_secs(1);
        /// After this many failures in a row, the warning is shown.
        const ATTEMPTS: u32 = 8;
        /// How long the size must stay the same before the session is asked to match it.
        const SETTLE: Duration = Duration::from_millis(250);
        if generation != self.generation {
            // A new connection or reconnection starts from the connect-time settings.
            self.generation = generation;
            self.current = self.initial;
            self.failures = 0;
        }
        let Some(size) = self.size else {
            return;
        };
        let wanted = (size, scale);
        if self.display != Display::Fit || !connected || self.current == Some(wanted) {
            return;
        }
        // Wait for the size to settle, and between retries. Nothing else may redraw the window
        // meanwhile, so wake it when the wait is over.
        let wait = SETTLE
            .saturating_sub(self.resized.elapsed())
            .max(self.retry_at.saturating_duration_since(Instant::now()));
        if !wait.is_zero() {
            self.ctx.request_repaint_after(wait);
            return;
        }
        // Physical size in millimetres at the window's DPI.
        let millimetres = |pixels: i32| (pixels as u32 * 2540 / (scale * 96)).max(10);
        let result = invoke(
            &self.client,
            "UpdateSessionDisplaySettings",
            DISPATCH_METHOD,
            vec![
                VARIANT::from(size.0 as u32),
                VARIANT::from(size.1 as u32),
                VARIANT::from(millimetres(size.0)),
                VARIANT::from(millimetres(size.1)),
                VARIANT::from(0u32),
                VARIANT::from(scale),
                VARIANT::from(device_scale(scale)),
            ],
        );
        match result {
            Ok(_) => {
                self.current = Some(wanted);
                self.failures = 0;
                self.display_warning = None;
            }
            Err(e) => {
                self.failures += 1;
                self.retry_at = Instant::now() + RETRY;
                if self.failures >= ATTEMPTS {
                    // Stop retrying until the size or scale changes again.
                    self.current = Some(wanted);
                    self.failures = 0;
                    self.display_warning = Some(format!(
                        "The remote host could not update its display size: {e}"
                    ));
                } else {
                    self.ctx.request_repaint_after(RETRY);
                }
            }
        }
    }
}
impl Drop for RdpClient {
    fn drop(&mut self) {
        for (name, callback) in &self.callbacks {
            let _ = invoke(
                &self.client,
                "detachEvent",
                DISPATCH_METHOD,
                vec![VARIANT::from(name.clone()), VARIANT::from(callback.clone())],
            );
        }
        let _ = invoke(&self.client, "Disconnect", DISPATCH_METHOD, vec![]);
    }
}

/// The window's display scale in percent, as Windows reports it for that monitor.
fn scale_for(window: HWND) -> u32 {
    match unsafe { GetDpiForWindow(window) } {
        0 => 100,
        dpi => ((dpi * 100 + 48) / 96).clamp(100, 500),
    }
}

/// The remote device scale, which RDP accepts only as 100, 140, or 180 percent.
fn device_scale(scale: u32) -> u32 {
    match scale {
        180.. => 180,
        140.. => 140,
        _ => 100,
    }
}

/// Windows offers to remember credentials in Credential Manager.
pub const SAVES_PASSWORDS: bool = true;

/// The control can connect to a server's administrative session, as mstsc /admin does.
pub const ADMIN_SESSIONS: bool = true;

/// How signing in works, for the connection editor.
pub const SIGN_IN_HELP: &str = "Windows asks for credentials; tick “Remember me” to skip the \
    prompt next time.\nWindows keeps them in Credential Manager. Relay never sees passwords.";

/// The control supports every option.
pub fn unsupported(_options: &crate::model::RdpOptions) -> Vec<&'static str> {
    vec![]
}

/// Removes Windows-saved credentials for an RDP host. Returns whether any existed.
pub fn forget_credentials(host: &str) -> Result<bool, String> {
    use windows_sys::Win32::Security::Credentials::{CRED_TYPE_DOMAIN_PASSWORD, CredDeleteW};
    let target: Vec<u16> = format!("TERMSRV/{host}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_DOMAIN_PASSWORD, 0) } != 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    // ERROR_NOT_FOUND
    if error.raw_os_error() == Some(1168) {
        Ok(false)
    } else {
        Err(error.to_string())
    }
}

fn atl() -> Result<usize, String> {
    // The host window procedures remain in ATL until all controls have been destroyed.
    static MODULE: OnceLock<Result<usize, String>> = OnceLock::new();
    MODULE
        .get_or_init(|| unsafe {
            let name: Vec<u16> = "atl.dll\0".encode_utf16().collect();
            let module = LoadLibraryExW(
                name.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            );
            if module.is_null() {
                Err(format!(
                    "Windows ATL is unavailable: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                Ok(module as usize)
            }
        })
        .clone()
}

fn property(object: &IDispatch, name: &str) -> Result<IDispatch, String> {
    IDispatch::try_from(&invoke(object, name, DISPATCH_PROPERTYGET, vec![])?)
        .map_err(|e| format!("RDP {name} is unavailable: {e}"))
}
fn parameters(args: &mut [VARIANT], flags: DISPATCH_FLAGS, named: &mut i32) -> DISPPARAMS {
    DISPPARAMS {
        rgvarg: args.as_mut_ptr(),
        rgdispidNamedArgs: if flags == DISPATCH_PROPERTYPUT {
            named
        } else {
            std::ptr::null_mut()
        },
        cArgs: args.len() as u32,
        cNamedArgs: if flags == DISPATCH_PROPERTYPUT { 1 } else { 0 },
    }
}
fn invoke(
    object: &IDispatch,
    name: &str,
    flags: DISPATCH_FLAGS,
    mut args: Vec<VARIANT>,
) -> Result<VARIANT, String> {
    unsafe {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut id = 0;
        object
            .GetIDsOfNames(&GUID::zeroed(), &PCWSTR(wide.as_ptr()), 1, 0, &mut id)
            .map_err(|e| format!("RDP does not expose {name}: {e}"))?;
        args.reverse();
        let mut named = -3;
        let params = parameters(&mut args, flags, &mut named);
        let mut result = VARIANT::default();
        object
            .Invoke(
                id,
                &GUID::zeroed(),
                0,
                flags,
                &params,
                Some(&mut result),
                None,
                None,
            )
            .map_err(|e| format!("RDP {name} failed: {e}"))?;
        Ok(result)
    }
}
fn nonscriptable(
    client: &IDispatch,
    name: &str,
    flags: DISPATCH_FLAGS,
    mut args: Vec<VARIANT>,
) -> Result<VARIANT, String> {
    unsafe {
        let iid = GUID::from_u128(0x4f6996d5_d7b1_412c_b0ff_063718566907);
        let mut raw = std::ptr::null_mut();
        client
            .query(&iid, &mut raw)
            .ok()
            .map_err(|e| e.to_string())?;
        let object = IUnknown::from_raw(raw);
        let mut library = None;
        let mut index = 0;
        client
            .GetTypeInfo(0, 0)
            .and_then(|t| t.GetContainingTypeLib(&mut library, &mut index))
            .map_err(|e| e.to_string())?;
        let info = library
            .ok_or("RDP type library is missing")?
            .GetTypeInfoOfGuid(&iid)
            .map_err(|e| e.to_string())?;
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut id = 0;
        info.GetIDsOfNames(&PCWSTR(wide.as_ptr()), 1, &mut id)
            .map_err(|e| format!("RDP authentication property {name} is unavailable: {e}"))?;
        args.reverse();
        let mut named = -3;
        let mut params = parameters(&mut args, flags, &mut named);
        let mut result = VARIANT::default();
        let mut arg_error = 0;
        info.Invoke(
            object.as_raw(),
            id,
            flags,
            &mut params,
            &mut result,
            std::ptr::null_mut(),
            &mut arg_error,
        )
        .map_err(|e| format!("Cannot set RDP authentication property {name}: {e}"))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RdpOptions;

    fn parent() -> Window {
        unsafe {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let parent = Window(CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                1800,
                1000,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ));
            assert!(!parent.0.is_null());
            parent
        }
    }
    fn get(object: &IDispatch, name: &str) -> VARIANT {
        invoke(object, name, DISPATCH_PROPERTYGET, vec![]).unwrap()
    }
    /// Creating RDP controls on several test threads at once intermittently crashes the
    /// process, so tests that create one take turns. Relay creates them on one thread.
    fn one_control_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static CONTROLS: Mutex<()> = Mutex::new(());
        CONTROLS.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn activex_creates_and_tracks_viewport_without_connecting() {
        let _turn = one_control_at_a_time();
        let parent = parent();
        let mut connection = Connection::new();
        connection.host = "localhost".into();
        connection.username = "test-user".into();
        let mut client = RdpClient::new(
            NativeWindow::from_hwnd(parent.0),
            &connection,
            false,
            egui::Context::default(),
        )
        .unwrap();
        unsafe {
            let inplace: windows::Win32::System::Ole::IOleInPlaceObject =
                client.client.cast().unwrap();
            let control_window = inplace.GetWindow().unwrap().0;
            for rect in [[260, 125, 1400, 720], [260, 125, 900, 500]] {
                client.layout(rect).unwrap();
                assert_eq!(
                    platform::windows::bounds(client.window.0, parent.0).unwrap(),
                    rect
                );
                assert_eq!(
                    platform::windows::bounds(control_window, client.window.0).unwrap(),
                    [0, 0, rect[2], rect[3]]
                );
                assert_eq!(
                    i32::try_from(&get(&client.client, "DesktopWidth")).unwrap(),
                    rect[2]
                );
                assert_eq!(
                    i32::try_from(&get(&client.client, "DesktopHeight")).unwrap(),
                    rect[3]
                );
                for name in ["AllowPromptingForCredentials", "AllowCredentialSaving"] {
                    let value =
                        nonscriptable(&client.client, name, DISPATCH_PROPERTYGET, vec![]).unwrap();
                    assert!(bool::try_from(&value).unwrap(), "{name}");
                }
                assert!(!client.started);
            }
        }
        assert!(bool::try_from(&get(&client.advanced, "EnableAutoReconnect")).unwrap());
        assert!(bool::try_from(&get(&client.advanced, "RedirectClipboard")).unwrap());
        assert!(!bool::try_from(&get(&client.advanced, "RedirectDrives")).unwrap());
    }

    #[test]
    fn viewport_moves_to_another_window() {
        let _turn = one_control_at_a_time();
        let (first, second) = (parent(), parent());
        let mut connection = Connection::new();
        connection.host = "localhost".into();
        let mut client = RdpClient::new(
            NativeWindow::from_hwnd(first.0),
            &connection,
            false,
            egui::Context::default(),
        )
        .unwrap();
        client.layout([10, 20, 640, 480]).unwrap();
        client.set_parent(NativeWindow::from_hwnd(second.0).unwrap());
        client.layout([30, 40, 800, 500]).unwrap();
        unsafe {
            assert_eq!(GetParent(client.window.0), second.0);
        }
        assert_eq!(
            platform::windows::bounds(client.window.0, second.0).unwrap(),
            [30, 40, 800, 500]
        );
    }

    #[test]
    fn connection_options_reach_the_control() {
        let _turn = one_control_at_a_time();
        let parent = parent();
        let mut connection = Connection::new();
        connection.host = "desk.example".into();
        connection.rdp = RdpOptions {
            gateway: "gw.example.com".into(),
            clipboard: false,
            drives: true,
            printers: true,
            smart_cards: true,
            microphone: true,
            audio: Audio::Off,
            keys: Keys::Remote,
            display: Display::Fixed {
                width: 1600,
                height: 900,
            },
        };
        let mut client = RdpClient::new(
            NativeWindow::from_hwnd(parent.0),
            &connection,
            false,
            egui::Context::default(),
        )
        .unwrap();
        let flag = |name| bool::try_from(&get(&client.advanced, name)).unwrap();
        assert!(!flag("RedirectClipboard"));
        for name in [
            "RedirectDrives",
            "RedirectPrinters",
            "RedirectSmartCards",
            "AudioCaptureRedirectionMode",
        ] {
            assert!(flag(name), "{name}");
        }
        let secured = property(&client.client, "SecuredSettings3").unwrap();
        assert_eq!(
            i32::try_from(&get(&secured, "KeyboardHookMode")).unwrap(),
            1
        );
        assert_eq!(
            i32::try_from(&get(&secured, "AudioRedirectionMode")).unwrap(),
            2
        );
        let transport = property(&client.client, "TransportSettings2").unwrap();
        assert_eq!(
            BSTR::try_from(&get(&transport, "GatewayHostname"))
                .unwrap()
                .to_string(),
            "gw.example.com"
        );
        assert_eq!(
            u32::try_from(&get(&transport, "GatewayUsageMethod")).unwrap(),
            1
        );
        // A fixed resolution ignores the viewport size.
        client.layout([0, 0, 900, 500]).unwrap();
        assert_eq!(
            i32::try_from(&get(&client.client, "DesktopWidth")).unwrap(),
            1600
        );
        assert_eq!(
            i32::try_from(&get(&client.client, "DesktopHeight")).unwrap(),
            900
        );
        client.set_scale(150).unwrap();
        let settings: IMsRdpExtendedSettings = client.client.cast().unwrap();
        for (name, expected) in [("DesktopScaleFactor", 150), ("DeviceScaleFactor", 140)] {
            let mut value = VARIANT::default();
            let name = BSTR::from(name);
            unsafe { settings.get_Property(name.as_ptr(), &mut value) }
                .ok()
                .unwrap();
            assert_eq!(u32::try_from(&value).unwrap(), expected, "{name}");
        }
    }

    #[test]
    fn moving_to_another_window_closes_holes_cut_for_the_old_ones_overlays() {
        use windows_sys::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, GetWindowRgn};
        let _turn = one_control_at_a_time();
        let (first, second) = (parent(), parent());
        let mut connection = Connection::new();
        connection.host = "localhost".into();
        let mut client = RdpClient::new(
            NativeWindow::from_hwnd(first.0),
            &connection,
            false,
            egui::Context::default(),
        )
        .unwrap();
        // Shown without contacting a host, under a menu that hangs over its top.
        client.started = true;
        client
            .place(Some([0, 0, 400, 300]), &[[20, 0, 200, 36]])
            .unwrap();
        let region = unsafe { CreateRectRgn(0, 0, 0, 0) };
        assert_ne!(
            unsafe { GetWindowRgn(client.window.0, region) },
            0,
            "the hole is cut"
        );
        // The menu's choice moves the session to its own window in the same frame.
        client.set_parent(NativeWindow::from_hwnd(second.0).unwrap());
        assert_eq!(
            unsafe { GetWindowRgn(client.window.0, region) },
            0,
            "a hole stayed open in the new window"
        );
        unsafe { DeleteObject(region) };
    }

    #[test]
    fn the_viewport_shows_the_arrow_instead_of_asking_relay_for_its_cursor() {
        let _turn = one_control_at_a_time();
        let parent = parent();
        let mut connection = Connection::new();
        connection.host = "localhost".into();
        let client = RdpClient::new(
            NativeWindow::from_hwnd(parent.0),
            &connection,
            false,
            egui::Context::default(),
        )
        .unwrap();
        // What Windows sends up from the control as the pointer moves over the desktop.
        let answered = unsafe {
            SendMessageW(
                client.window.0,
                WM_SETCURSOR,
                client.window.0 as WPARAM,
                (HTCLIENT | (WM_MOUSEMOVE << 16)) as LPARAM,
            )
        };
        assert_eq!(answered, 1, "the question went on to Relay's window");
        assert_eq!(unsafe { GetCursor() }, unsafe {
            LoadCursorW(std::ptr::null_mut(), IDC_ARROW)
        });
    }

    #[test]
    fn display_updates_skip_connect_settings_and_retry_quietly() {
        let _turn = one_control_at_a_time();
        let parent = parent();
        let mut connection = Connection::new();
        connection.host = "localhost".into();
        // Waits must wake the window themselves, since nothing else may redraw it.
        let ctx = egui::Context::default();
        let wakes = Arc::new(Mutex::new(vec![]));
        let seen = wakes.clone();
        ctx.set_request_repaint_callback(move |info| seen.lock().unwrap().push(info.delay));
        // egui reports a wake-up only when it is sooner than one already due, so a few frames
        // pass first, as they do in Relay, and clear what is due.
        let frames = || {
            for _ in 0..3 {
                let _ = ctx.run(egui::RawInput::default(), |_| {});
            }
            wakes.lock().unwrap().clear();
        };
        let last_wake = || wakes.lock().unwrap().pop().expect("a scheduled wake-up");
        let mut client = RdpClient::new(
            NativeWindow::from_hwnd(parent.0),
            &connection,
            false,
            ctx.clone(),
        )
        .unwrap();
        client.layout([0, 0, 1000, 600]).unwrap();
        client.resized = Instant::now() - Duration::from_secs(1);
        // Pretend the session connected with this size at 100%, without contacting a host.
        client.started = true;
        client.initial = Some(((1000, 600), 100));
        client.sync_display(100, true, 1);
        assert_eq!(
            client.failures, 0,
            "no update is needed at the connect-time size"
        );
        assert!(client.display_warning.is_none());
        // A new scale needs an update. An unconnected control rejects it, like a session that
        // has not finished logging in; failures retry without a warning at first.
        for attempt in 1..8 {
            client.retry_at = Instant::now();
            frames();
            client.sync_display(150, true, 1);
            assert_eq!(client.failures, attempt);
            assert!(
                client.display_warning.is_none(),
                "warned on attempt {attempt}"
            );
            // The failed attempt itself wakes the window for the next one.
            let wake = last_wake();
            assert!(
                wake > Duration::from_millis(900) && wake <= Duration::from_secs(1),
                "attempt {attempt}: {wake:?}"
            );
        }
        frames();
        client.sync_display(150, true, 1);
        assert_eq!(client.failures, 7, "retries wait for the retry delay");
        let wake = last_wake();
        assert!(
            wake > Duration::ZERO && wake <= Duration::from_secs(1),
            "{wake:?}"
        );
        client.retry_at = Instant::now();
        client.sync_display(150, true, 1);
        assert!(client.display_warning.is_some());
        // A reconnection starts again from the connect-time settings.
        client.sync_display(100, true, 2);
        assert_eq!((client.current, client.failures), (client.initial, 0));
        // A new size waits until it has settled, then wakes the window to send it.
        client.retry_at = Instant::now();
        client.layout([0, 0, 1200, 700]).unwrap();
        frames();
        client.sync_display(100, true, 2);
        assert_eq!(client.failures, 0, "no update while the size settles");
        let wake = last_wake();
        assert!(
            wake > Duration::ZERO && wake <= Duration::from_millis(250),
            "{wake:?}"
        );
    }

    #[test]
    fn scales_map_to_supported_device_factors() {
        assert_eq!(
            [100, 125, 150, 175, 200, 300].map(device_scale),
            [100, 100, 140, 140, 180, 180]
        );
    }

    #[test]
    fn forgetting_missing_credentials_is_not_an_error() {
        assert_eq!(forget_credentials("relay-test.invalid"), Ok(false));
    }

    #[test]
    fn control_events_report_connection_state() {
        let state = Arc::new(Mutex::new(State::default()));
        let ctx = egui::Context::default();
        let fire = |event| unsafe {
            let callback: IDispatch = Sink {
                event,
                state: state.clone(),
                ctx: ctx.clone(),
            }
            .into();
            callback
                .Invoke(
                    0,
                    &GUID::zeroed(),
                    0,
                    DISPATCH_METHOD,
                    &DISPPARAMS::default(),
                    None,
                    None,
                    None,
                )
                .unwrap();
        };
        fire(Event::Connected);
        fire(Event::Login);
        fire(Event::Reconnecting);
        assert!(state.lock().unwrap().reconnecting);
        assert!(!state.lock().unwrap().ended);
        fire(Event::Reconnected);
        assert!(!state.lock().unwrap().reconnecting);
        fire(Event::EnterFullScreen);
        assert!(state.lock().unwrap().full_screen);
        fire(Event::Disconnected);
        let state = state.lock().unwrap();
        assert!(state.ended);
        assert!(!state.connected && !state.full_screen);
        assert_eq!(state.generation, 3);
        assert!(state.error.is_some());
    }
}
