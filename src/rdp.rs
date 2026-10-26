use crate::model::Connection;
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
use windows_sys::Win32::{Foundation::HWND, System::LibraryLoader::*, UI::WindowsAndMessaging::*};

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

#[derive(Default)]
struct State {
    connected: bool,
    ended: bool,
    generation: u64,
    error: Option<String>,
}
#[derive(Clone, Copy)]
enum Event {
    Connected,
    Disconnected,
    Login,
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
            Event::Connected | Event::Login => {
                state.connected = true;
                state.ended = false;
                state.generation += 1;
                state.error = None;
            }
            Event::Disconnected => {
                state.connected = false;
                state.ended = true;
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
    started: bool,
    size: Option<(i32, i32)>,
    applied: Option<((i32, i32), u64)>,
    resized: Instant,
    pub display_warning: Option<String>,
    _apartment: Apartment,
}

impl RdpClient {
    pub fn new(parent: HWND, connection: &Connection, ctx: egui::Context) -> Result<Self, String> {
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
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let window = Window(CreateWindowExW(
                0,
                class.as_ptr(),
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
        let advanced = IDispatch::try_from(&invoke(
            &client,
            "AdvancedSettings9",
            DISPATCH_PROPERTYGET,
            vec![],
        )?)
        .map_err(|e| e.to_string())?;
        let mut rdp = Self {
            window,
            client,
            advanced,
            _host: host,
            callbacks: vec![],
            state: Arc::new(Mutex::new(State::default())),
            parent,
            started: false,
            size: None,
            applied: None,
            resized: Instant::now(),
            display_warning: None,
            _apartment: apartment,
        };
        for (name, value) in [
            ("Server", VARIANT::from(connection.host.as_str())),
            ("UserName", VARIANT::from(connection.username.as_str())),
        ] {
            rdp.set(name, &value)?;
        }
        for (name, value) in [
            ("RDPPort", VARIANT::from(connection.port as i32)),
            ("EnableCredSspSupport", VARIANT::from(true)),
            ("AuthenticationLevel", VARIANT::from(2u32)),
            ("SmartSizing", VARIANT::from(true)),
            ("RedirectClipboard", VARIANT::from(true)),
            ("RedirectPrinters", VARIANT::from(false)),
        ] {
            invoke(&rdp.advanced, name, DISPATCH_PROPERTYPUT, vec![value])?;
        }
        nonscriptable(
            &rdp.client,
            "AllowPromptingForCredentials",
            DISPATCH_PROPERTYPUT,
            vec![VARIANT::from(true)],
        )?;
        nonscriptable(
            &rdp.client,
            "AllowCredentialSaving",
            DISPATCH_PROPERTYPUT,
            vec![VARIANT::from(false)],
        )?;
        nonscriptable(
            &rdp.client,
            "PromptForCredentials",
            DISPATCH_PROPERTYPUT,
            vec![VARIANT::from(true)],
        )?;
        for (name, event) in [
            ("OnConnected", Event::Connected),
            ("OnDisconnected", Event::Disconnected),
            ("OnLoginComplete", Event::Login),
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
    pub fn window(&self) -> HWND {
        self.window.0
    }
    pub fn ended(&self) -> bool {
        self.state.lock().unwrap().ended
    }
    pub fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }
    fn layout(&mut self, [x, y, w, h]: [i32; 4]) -> Result<(), String> {
        let (w, h) = (w.max(1), h.max(1));
        crate::native_host::position(self.window.0, self.parent, [x, y, w, h])?;
        let size = (w.clamp(200, 8192), h.clamp(200, 8192));
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
    pub fn place(&mut self, rect: Option<[i32; 4]>) -> Result<(), String> {
        let Some(rect) = rect else {
            unsafe {
                ShowWindow(self.window.0, SW_HIDE);
            }
            return Ok(());
        };
        self.layout(rect)?;
        unsafe {
            ShowWindow(self.window.0, SW_SHOWNA);
        }
        if !self.started {
            invoke(&self.client, "Connect", DISPATCH_METHOD, vec![])?;
            self.started = true;
        }
        let state = self.state.lock().unwrap();
        let (connected, generation) = (state.connected, state.generation);
        drop(state);
        if let Some(size) = self.size
            && connected
            && self.applied != Some((size, generation))
            && self.resized.elapsed() >= Duration::from_millis(250)
        {
            match invoke(
                &self.client,
                "UpdateSessionDisplaySettings",
                DISPATCH_METHOD,
                vec![
                    VARIANT::from(size.0 as u32),
                    VARIANT::from(size.1 as u32),
                    VARIANT::from((size.0 as u32 * 254 / 960).max(10)),
                    VARIANT::from((size.1 as u32 * 254 / 960).max(10)),
                    VARIANT::from(0u32),
                    VARIANT::from(100u32),
                    VARIANT::from(100u32),
                ],
            ) {
                Ok(_) => {
                    self.applied = Some((size, generation));
                    self.display_warning = None;
                }
                Err(e) => {
                    self.applied = Some((size, generation));
                    self.display_warning = Some(format!(
                        "The remote host could not update its display size: {e}"
                    ));
                }
            }
        }
        Ok(())
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
    #[test]
    fn activex_creates_and_tracks_viewport_without_connecting() {
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
            let mut connection = Connection::new();
            connection.host = "localhost".into();
            connection.username = "test-user".into();
            let mut client =
                RdpClient::new(parent.0, &connection, egui::Context::default()).unwrap();
            let inplace: windows::Win32::System::Ole::IOleInPlaceObject =
                client.client.cast().unwrap();
            let control_window = inplace.GetWindow().unwrap().0;
            let name: Vec<u16> = "UpdateSessionDisplaySettings\0".encode_utf16().collect();
            let mut id = 0;
            client
                .client
                .GetIDsOfNames(&GUID::zeroed(), &PCWSTR(name.as_ptr()), 1, 0, &mut id)
                .unwrap();
            for rect in [[260, 125, 1400, 720], [260, 125, 900, 500]] {
                client.layout(rect).unwrap();
                assert_eq!(
                    crate::native_host::bounds(client.window.0, parent.0).unwrap(),
                    rect
                );
                assert_eq!(
                    crate::native_host::bounds(control_window, client.window.0).unwrap(),
                    [0, 0, rect[2], rect[3]]
                );
                assert_eq!(
                    i32::try_from(
                        &invoke(&client.client, "DesktopWidth", DISPATCH_PROPERTYGET, vec![])
                            .unwrap()
                    )
                    .unwrap(),
                    rect[2]
                );
                assert_eq!(
                    i32::try_from(
                        &invoke(
                            &client.client,
                            "DesktopHeight",
                            DISPATCH_PROPERTYGET,
                            vec![]
                        )
                        .unwrap()
                    )
                    .unwrap(),
                    rect[3]
                );
                assert!(
                    bool::try_from(
                        &nonscriptable(
                            &client.client,
                            "AllowPromptingForCredentials",
                            DISPATCH_PROPERTYGET,
                            vec![]
                        )
                        .unwrap()
                    )
                    .unwrap()
                );
                assert!(!client.started);
            }
        }
    }
    #[test]
    fn control_events_report_connection_state() {
        let state = Arc::new(Mutex::new(State::default()));
        let ctx = egui::Context::default();
        for event in [Event::Connected, Event::Login, Event::Disconnected] {
            let callback: IDispatch = Sink {
                event,
                state: state.clone(),
                ctx: ctx.clone(),
            }
            .into();
            unsafe {
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
            }
        }
        let state = state.lock().unwrap();
        assert!(state.ended);
        assert!(!state.connected);
        assert_eq!(state.generation, 2);
        assert!(state.error.is_some());
    }
}
