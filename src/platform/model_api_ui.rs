//! Explicit, local-only configuration. Opening never reads Credential Manager.
use crate::settings::SettingsStore;
use crate::writing::{
    config::{self, ApiProfile, ApiProtocol, ModelApiSettings},
    credentials,
};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{SetScrollInfo, EM_SETLIMITTEXT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, SetActiveWindow, SetFocus, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("Flower.ModelApiSettings");
const PROFILE: i32 = 101;
const NEW: i32 = 102;
const DELETE: i32 = 103;
const NAME: i32 = 104;
const PROTOCOL: i32 = 105;
const URL: i32 = 106;
const MODEL: i32 = 107;
const ADVANCED: i32 = 108;
const SAVE: i32 = 109;
const KEY: i32 = 110;
const FETCH: i32 = 111;
const DELETE_KEY: i32 = 112;
const CLOSE: i32 = 113;
const MANUAL: i32 = 114;
const STATUS: i32 = 207;
const CATALOG_STATUS: i32 = 208;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Private operation boundary: fixtures supply memory-only implementations.
trait Services: Send + Sync {
    fn load(&self, store: &SettingsStore) -> Result<ModelApiSettings, String> {
        store
            .load()
            .map(|s| s.model_api)
            .map_err(|_| "读取模型设置失败。".into())
    }
    fn persist(&self, store: &SettingsStore, value: &ModelApiSettings) -> Result<(), String> {
        config::persist(store, value)
    }
    fn key(&self, p: &ApiProfile) -> Result<Option<credentials::SecretKey>, String>;
    fn save(&self, p: &ApiProfile, key: &credentials::SecretKey) -> Result<(), String>;
    fn models(
        &self,
        p: &ApiProfile,
        key: &credentials::SecretKey,
        cancel: &AtomicBool,
    ) -> Result<Vec<String>, String>;
}
struct NativeServices;
impl Services for NativeServices {
    fn key(&self, p: &ApiProfile) -> Result<Option<credentials::SecretKey>, String> {
        credentials::load_key_for(p).map_err(|_| "读取密钥失败。".into())
    }
    fn save(&self, p: &ApiProfile, key: &credentials::SecretKey) -> Result<(), String> {
        credentials::save_key_for(p, key).map_err(|_| "保存密钥失败，连接未应用。".into())
    }
    fn models(
        &self,
        p: &ApiProfile,
        key: &credentials::SecretKey,
        cancel: &AtomicBool,
    ) -> Result<Vec<String>, String> {
        crate::writing::transport::fetch_models_for(p, key, cancel)
            .map_err(|_| "无法获取标准模型目录；请在高级设置手动填写供应商提供的模型 ID。".into())
    }
}
struct CatalogJob {
    target: ApiProfile,
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<Vec<String>, String>>>,
}
impl Drop for CatalogJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn catalog_credential(s: &ModelApiSettings, draft: &ApiProfile) -> Result<ApiProfile, String> {
    let draft = draft
        .connection_normalized()
        .map_err(|_| "请检查连接名称、HTTPS 基址和协议。")?;
    s.profiles
        .iter()
        .filter_map(|p| p.normalized().ok())
        .find(|p| p.id == draft.id && p.base_url == draft.base_url && p.protocol == draft.protocol)
        .ok_or_else(|| "此目标没有匹配的已保存连接，请输入新密钥。".into())
}

fn save_and_use(
    store: &SettingsStore,
    svc: &dyn Services,
    p: &ApiProfile,
    input: &str,
) -> Result<ApiProfile, String> {
    let p = p
        .normalized()
        .map_err(|_| "请选择模型，或在高级设置明确填写有效模型 ID；并检查 HTTPS 基址。")?;
    if !input.is_empty() {
        let key = credentials::SecretKey::new(input).map_err(|_| "请输入有效密钥。")?;
        svc.save(&p, &key)?;
    } else if svc.key(&p)?.is_none() {
        return Err("此目标尚无密钥，请输入新密钥后保存并使用。".into());
    }
    let result = (|| {
        let mut latest = svc.load(store)?;
        upsert(&mut latest, p.clone(), true)?;
        svc.persist(store, &latest)?;
        let actual = svc
            .load(store)?
            .active()
            .map_err(|_| "没有实际当前连接。")?;
        if actual.scope().ok() != p.scope().ok() {
            return Err("实际当前连接已变化。".into());
        }
        Ok(p)
    })();
    result.map_err(|_: String| {
        if input.is_empty() {
            "配置未确认应用；请重新检查实际当前连接。".into()
        } else {
            "密钥已保存，但配置未确认应用；未回滚并发设置，请重新检查实际当前连接。".into()
        }
    })
}
#[cfg(test)]
thread_local! {
    // Only explicitly opted-in owned-window fixtures suppress their own success box.
    static SUPPRESS_NOTICES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct State {
    store: SettingsStore,
    selected: ApiProfile,
    controls: Vec<(HWND, [i32; 4])>,
    font: HFONT,
    scroll: i32,
    advanced: bool,
    services: Arc<dyn Services>,
    catalog: Option<CatalogJob>,
    retired: Vec<CatalogJob>,
    background: HBRUSH,
    card: HBRUSH,
    extra_fonts: Vec<HFONT>,
    owner: HWND,
    updating: bool,
    rows: Vec<(HWND, [i32; 4])>,
    cards: Vec<RECT>,
    max_scroll: i32,
    wheel_remainder: i32,
    #[cfg(test)]
    layout_count: usize,
    #[cfg(test)]
    redraw_count: std::cell::Cell<usize>,
    #[cfg(test)]
    font_creations: usize,
    #[cfg(test)]
    text_measurements: usize,
    #[cfg(test)]
    pump_probe: Option<PumpProbe>,
}
#[cfg(test)]
type PumpProbe = Box<dyn FnMut(HWND, &MSG)>;
impl Drop for State {
    fn drop(&mut self) {
        self.catalog.take();
        self.retired.clear();
        unsafe {
            let _ = DeleteObject(self.background);
            let _ = DeleteObject(self.card);
            for font in self.extra_fonts.drain(..) {
                let _ = DeleteObject(font);
            }
        }
    }
}
fn label(hwnd: HWND, id: i32, value: &str) {
    let b = wide(value);
    unsafe {
        let _ = SetWindowTextW(GetDlgItem(hwnd, id).unwrap_or_default(), PCWSTR(b.as_ptr()));
    }
}
fn invalidate_catalog(hwnd: HWND, state: &mut State) {
    let updating = std::mem::replace(&mut state.updating, true);
    if let Some(job) = state.catalog.take() {
        job.cancel.store(true, Ordering::Relaxed);
        state.retired.push(job);
    }
    unsafe {
        SendMessageW(
            GetDlgItem(hwnd, MODEL).unwrap_or_default(),
            CB_RESETCONTENT,
            WPARAM(0),
            LPARAM(0),
        );
    }
    label(hwnd, MANUAL, "");
    label(
        hwnd,
        CATALOG_STATUS,
        "连接已变更，请重新获取模型列表；目录成功不代表调用可用。",
    );
    state.updating = updating;
}
fn key_changed(hwnd: HWND, state: &mut State) {
    // Remove the old-key job from the only consumable slot. Even a successful
    // result for the same connection can now only be joined and discarded.
    if let Some(job) = state.catalog.take() {
        job.cancel.store(true, Ordering::Relaxed);
        state.retired.push(job);
    }
    label(
        hwnd,
        CATALOG_STATUS,
        "密钥已变更；目录尚未经新密钥核验，已保留当前模型。",
    );
}
fn start_catalog(hwnd: HWND, state: &mut State) -> Result<(), String> {
    let draft = edit_profile(hwnd, &state.selected)?
        .connection_normalized()
        .map_err(|_| "请检查连接名称、HTTPS 基址和协议。")?;
    let mut input = text(hwnd, KEY);
    let typed = if input.is_empty() {
        None
    } else {
        Some(credentials::SecretKey::new(&input).map_err(|_| "请输入有效密钥。"))
    };
    unsafe {
        input.as_bytes_mut().fill(0);
    }
    let typed = typed.transpose()?;
    invalidate_catalog(hwnd, state);
    let svc = state.services.clone();
    let store = state.store.clone();
    let target = draft.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let thread = std::thread::spawn(move || {
        let key = match typed {
            Some(key) => key,
            None => {
                let saved = catalog_credential(&svc.load(&store)?, &draft)?;
                svc.key(&saved)?.ok_or("此目标没有密钥，请输入新密钥。")?
            }
        };
        if flag.load(Ordering::Relaxed) {
            return Err("已取消获取模型。".into());
        }
        svc.models(&draft, &key, &flag)
    });
    state.catalog = Some(CatalogJob {
        target,
        cancel,
        thread: Some(thread),
    });
    label(
        hwnd,
        CATALOG_STATUS,
        "正在获取目录…仅请求模型列表，不发送生成内容。",
    );
    Ok(())
}
fn poll_catalog(hwnd: HWND, state: &mut State) {
    state
        .retired
        .retain(|job| !job.thread.as_ref().is_some_and(|t| t.is_finished()));
    if !state
        .catalog
        .as_ref()
        .is_some_and(|job| job.thread.as_ref().is_some_and(|t| t.is_finished()))
    {
        return;
    }
    let mut job = state.catalog.take().unwrap();
    let result = job
        .thread
        .take()
        .unwrap()
        .join()
        .unwrap_or_else(|_| Err("获取模型目录失败。".into()));
    let current = edit_profile(hwnd, &state.selected)
        .and_then(|p| p.connection_normalized().map_err(|_| "连接无效。".into()));
    // Model selection is not part of a directory identity.
    let same = current.is_ok_and(|p| {
        p.id == job.target.id
            && p.name == job.target.name
            && p.base_url == job.target.base_url
            && p.protocol == job.target.protocol
    });
    if !same || job.cancel.load(Ordering::Relaxed) {
        return;
    }
    match result {
        Ok(models) => {
            let old = text(hwnd, MANUAL);
            unsafe {
                let combo = GetDlgItem(hwnd, MODEL).unwrap_or_default();
                SendMessageW(combo, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
                for model in &models {
                    let b = wide(model);
                    SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(b.as_ptr() as isize));
                }
                SendMessageW(
                    combo,
                    CB_SETCURSEL,
                    WPARAM(models.iter().position(|m| *m == old).unwrap_or(usize::MAX)),
                    LPARAM(0),
                );
            }
            label(
                hwnd,
                CATALOG_STATUS,
                if models.is_empty() {
                    "目录没有可选模型，请在高级设置手动填写供应商提供的模型 ID。"
                } else {
                    "已获取目录，请明确选择模型；尚未进行实际调用验证。"
                },
            );
        }
        Err(error) => label(hwnd, CATALOG_STATUS, &error),
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn notice(hwnd: HWND, text: &str) {
    #[cfg(test)]
    if SUPPRESS_NOTICES.with(|value| value.get()) {
        return;
    }
    let text = wide(text);
    unsafe {
        MessageBoxW(hwnd, PCWSTR(text.as_ptr()), w!("模型 API 设置"), MB_OK);
    }
}

fn default_window_rect(work: RECT, dpi: u32) -> RECT {
    let scale = |n: i32| ((n as i64 * dpi.max(96) as i64 + 48) / 96) as i32;
    let width = scale(820).min((work.right - work.left).max(1));
    let height = scale(900).min((work.bottom - work.top).max(1));
    let left = work.left + ((work.right - work.left - width) / 2).max(0);
    let top = work.top + ((work.bottom - work.top - height) / 2).max(0);
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

/// The caller supplies its existing store; no fallback to real user storage.
pub fn show_model_api_settings(owner: HWND, store: SettingsStore) -> Result<(), String> {
    show_model_api_settings_impl(owner, store, |_| {})
}
fn show_model_api_settings_impl(
    owner: HWND,
    store: SettingsStore,
    after_show: impl FnOnce(HWND),
) -> Result<(), String> {
    let initial = store.load().map_err(|_| "读取模型配置失败。")?.model_api;
    initial.validate().map_err(|_| "模型配置无效。")?;
    unsafe {
        let instance =
            HINSTANCE::from(GetModuleHandleW(PCWSTR::null()).map_err(|_| "无法获取窗口模块。")?);
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: CLASS,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err("无法注册在线设置窗口。".into());
        }
        let mut state = Box::new(State {
            store,
            selected: initial
                .profiles
                .first()
                .cloned()
                .unwrap_or_else(new_profile),
            controls: Vec::new(),
            font: HFONT::default(),
            scroll: 0,
            advanced: false,
            services: Arc::new(NativeServices),
            catalog: None,
            retired: Vec::new(),
            background: CreateSolidBrush(COLORREF(0x00f8f7f4)),
            card: CreateSolidBrush(COLORREF(0x00ffffff)),
            extra_fonts: Vec::new(),
            owner,
            updating: false,
            rows: Vec::new(),
            cards: Vec::new(),
            max_scroll: 0,
            wheel_remainder: 0,
            #[cfg(test)]
            layout_count: 0,
            #[cfg(test)]
            redraw_count: std::cell::Cell::new(0),
            #[cfg(test)]
            font_creations: 0,
            #[cfg(test)]
            text_measurements: 0,
            #[cfg(test)]
            pump_probe: None,
        });
        let created = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS,
            w!("模型 API 设置"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_VSCROLL | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            820,
            900,
            owner,
            HMENU::default(),
            instance,
            Some((&mut *state as *mut State).cast()),
        );
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(_) => {
                let _ = UnregisterClassW(CLASS, instance);
                return Err("无法创建在线设置窗口。".into());
            }
        };
        let initialized = initialize(hwnd, &mut state, initial, instance);
        if let Err(error) = initialized {
            let _ = DestroyWindow(hwnd);
            if !state.font.is_invalid() {
                let _ = DeleteObject(state.font);
            }
            let _ = UnregisterClassW(CLASS, instance);
            return Err(error);
        }
        // Keep all controls reachable even on a short work area.
        let target = if owner.is_invalid() { hwnd } else { owner };
        let monitor = MonitorFromWindow(target, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let mut dx = 96;
            let mut dy = 96;
            let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(
                monitor,
                windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
                &mut dx,
                &mut dy,
            );
            let bounds = default_window_rect(info.rcWork, dx);
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                SWP_NOZORDER,
            );
        }
        let restore_owner = !owner.is_invalid() && IsWindowEnabled(owner).as_bool();
        if restore_owner {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        SetTimer(hwnd, 1, 30, None);
        let _ = SetFocus(GetDlgItem(hwnd, PROFILE).unwrap_or_default());
        after_show(hwnd);
        let mut last_focus = GetFocus();
        let mut message = MSG::default();
        let mut error = None;
        while IsWindow(hwnd).as_bool() {
            // Leave a host quit message queued; never post WM_QUIT ourselves.
            if !PeekMessageW(&mut message, HWND::default(), 0, 0, PM_NOREMOVE).as_bool() {
                let _ = WaitMessage();
                continue;
            }
            if message.message == WM_QUIT {
                error = Some("宿主正在退出，在线设置已关闭。".to_owned());
                break;
            }
            let result = GetMessageW(&mut message, HWND::default(), 0, 0);
            if result.0 <= 0 {
                error = Some("读取在线设置消息失败。".to_owned());
                break;
            }
            if !preprocess_key(hwnd, &message) && !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            // Manual scrolling deliberately leaves the focused child offscreen.
            // Do not undo it when a wheel, scrollbar, timer or paint is dispatched.
            // Keyboard navigation (including reverse Tab) or an actual focus
            // transition makes the new/current keyboard target visible again.
            let owned = message.hwnd == hwnd || IsChild(hwnd, message.hwnd).as_bool();
            let keyboard_or_layout = owned
                && matches!(
                    message.message,
                    WM_KEYDOWN | WM_SYSKEYDOWN | WM_NEXTDLGCTL | WM_SIZE | WM_DPICHANGED
                );
            if IsWindow(hwnd).as_bool() && (GetFocus() != last_focus || keyboard_or_layout) {
                ensure_focus_visible(hwnd);
            }
            last_focus = GetFocus();
            #[cfg(test)]
            if let Some(mut probe) = state.pump_probe.take() {
                probe(hwnd, &message);
                state.pump_probe = Some(probe);
            }
        }
        if IsWindow(hwnd).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        if restore_owner && IsWindow(owner).as_bool() {
            let _ = EnableWindow(owner, true);
            let _ = SetActiveWindow(owner);
        }
        if !state.font.is_invalid() {
            let _ = DeleteObject(state.font);
        }
        let _ = UnregisterClassW(CLASS, instance);
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn ensure_focus_visible(hwnd: HWND) {
    unsafe {
        let focused = GetFocus();
        if !IsChild(hwnd, focused).as_bool() {
            return;
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if ptr.is_null() {
            return;
        }
        let mut rect = RECT::default();
        let mut client = RECT::default();
        if GetWindowRect(focused, &mut rect).is_err() || GetClientRect(hwnd, &mut client).is_err() {
            return;
        }
        let mut origin = POINT {
            x: rect.left,
            y: rect.top,
        };
        let _ = ScreenToClient(hwnd, &mut origin);
        let bottom = origin.y + rect.bottom - rect.top;
        let delta = if origin.y < 8 {
            origin.y - 8
        } else if bottom > client.bottom - 8 {
            bottom - client.bottom + 8
        } else {
            0
        };
        if delta != 0 {
            scroll_to(hwnd, &mut *ptr, (*ptr).scroll.saturating_add(delta));
        }
    }
}

fn preprocess_key(hwnd: HWND, message: &MSG) -> bool {
    unsafe {
        if message.message != WM_KEYDOWN
            || !(message.hwnd == hwnd || IsChild(hwnd, message.hwnd).as_bool())
        {
            return false;
        }
        if matches!(GetDlgCtrlID(message.hwnd), PROFILE | PROTOCOL | MODEL)
            && SendMessageW(message.hwnd, CB_GETDROPPEDSTATE, WPARAM(0), LPARAM(0)).0 != 0
            && matches!(message.wParam.0, n if n == VK_ESCAPE.0 as usize || n == VK_RETURN.0 as usize)
        {
            SendMessageW(
                message.hwnd,
                message.message,
                message.wParam,
                message.lParam,
            );
            return true;
        }
        if message.wParam.0 == VK_ESCAPE.0 as usize {
            let _ = DestroyWindow(hwnd);
            return true;
        }
        // Enter never grants consent or performs a credential operation.
        message.wParam.0 == VK_RETURN.0 as usize
    }
}

fn layout(hwnd: HWND, state: &mut State) {
    layout_at_dpi(hwnd, state, unsafe { GetDpiForWindow(hwnd) }.max(96));
}

fn scroll_to(hwnd: HWND, state: &mut State, position: i32) {
    let position = position.clamp(0, state.max_scroll);
    if position == state.scroll {
        return;
    }
    state.scroll = position;
    position_rows(hwnd, state);
}

fn position_rows(hwnd: HWND, state: &State) {
    unsafe {
        let mut deferred = false;
        if let Ok(mut batch) = BeginDeferWindowPos(state.rows.len() as i32) {
            let mut valid = true;
            for &(control, [x, y, w, h]) in &state.rows {
                match DeferWindowPos(
                    batch,
                    control,
                    HWND::default(),
                    x,
                    y - state.scroll,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS,
                ) {
                    Ok(next) => batch = next,
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid {
                deferred = EndDeferWindowPos(batch).is_ok();
            }
        }
        if !deferred {
            for &(control, [x, y, w, h]) in &state.rows {
                let _ = SetWindowPos(
                    control,
                    HWND::default(),
                    x,
                    y - state.scroll,
                    w,
                    h,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS,
                );
            }
        }
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_POS,
            nPos: state.scroll,
            ..Default::default()
        };
        SetScrollInfo(hwnd, SB_VERT, &info, false);
        #[cfg(test)]
        state.redraw_count.set(state.redraw_count.get() + 1);
        let _ = RedrawWindow(
            hwnd,
            None,
            HRGN::default(),
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
    }
}

fn layout_at_dpi(hwnd: HWND, state: &mut State, dpi: u32) {
    #[cfg(test)]
    {
        state.layout_count += 1;
    }
    unsafe {
        let scale = |n: i32| ((n as i64 * dpi as i64 + 48) / 96) as i32;
        let font = CreateFontW(
            -scale(14),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            w!("Microsoft YaHei UI"),
        );
        let tier_font = |size| {
            CreateFontW(
                -scale(size),
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                CLEARTYPE_QUALITY.0 as u32,
                DEFAULT_PITCH.0 as u32,
                w!("Microsoft YaHei UI"),
            )
        };
        let title_font = tier_font(20);
        let hint_font = tier_font(12);
        #[cfg(test)]
        {
            state.font_creations += 3;
        }
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let width = client.right.max(1);
        let dc = GetDC(hwnd);
        let old_font = if !font.is_invalid() {
            SelectObject(dc, font)
        } else {
            HGDIOBJ::default()
        };
        let mut rows = Vec::new();
        let mut boundaries = Vec::new();
        let mut shift = 0;
        let mut last_y = -1;
        let mut row_extra = 0;
        let mut controls = state.controls.clone();
        controls.sort_by_key(|(_, rect)| rect[1]);
        for (control, [x, y, w, h]) in &controls {
            if *y != last_y {
                shift += row_extra;
                row_extra = 0;
                last_y = *y;
            }
            let cw = w * width / 598;
            let id = GetDlgCtrlID(*control);
            let mut height = scale(*h);
            if id >= 200 || !matches!(id, PROFILE | PROTOCOL | NAME | URL | MODEL | MANUAL | KEY) {
                let mut text = vec![0u16; GetWindowTextLengthW(*control).max(0) as usize + 1];
                let count = GetWindowTextW(*control, &mut text) as usize;
                let inset = if id == ADVANCED {
                    scale(24)
                } else if id < 200 {
                    scale(16)
                } else {
                    0
                };
                let mut measured = RECT {
                    right: (cw - inset).max(1),
                    ..Default::default()
                };
                DrawTextW(
                    dc,
                    &mut text[..count],
                    &mut measured,
                    DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                );
                #[cfg(test)]
                {
                    state.text_measurements += 1;
                }
                height = height.max(measured.bottom + scale(4));
                row_extra = row_extra.max(height - scale(*h));
            }
            rows.push((*control, [x * width / 598, scale(*y) + shift, cw, height]));
            boundaries.push((*y, scale(*y) + shift, height));
        }
        if !old_font.is_invalid() {
            SelectObject(dc, old_font);
        }
        ReleaseDC(hwnd, dc);
        let content_height = scale(if state.advanced { 1070 } else { 808 }) + shift + row_extra;
        let max = (content_height - client.bottom).max(0);
        state.max_scroll = max;
        state.cards = [(44, 350), (360, 542), (552, 690)]
            .into_iter()
            .map(|(top, bottom)| {
                let offset = boundaries
                    .iter()
                    .find(|(y, _, _)| *y >= top)
                    .map_or(0, |(y, actual, _)| actual - scale(*y));
                let end = boundaries
                    .iter()
                    .filter(|(y, _, _)| *y >= top && *y < bottom)
                    .map(|(_, actual, height)| actual + height + scale(8))
                    .max()
                    .unwrap_or(scale(bottom) + offset);
                RECT {
                    left: width * 12 / 598,
                    right: width * 586 / 598,
                    top: scale(top) + offset,
                    bottom: end.max(scale(bottom) + offset),
                }
            })
            .collect();
        state.scroll = state.scroll.clamp(0, max);
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: content_height - 1,
            nPage: client.bottom.max(0) as u32,
            nPos: state.scroll,
            ..Default::default()
        };
        SetScrollInfo(hwnd, SB_VERT, &info, true);
        // Scale horizontally as well: a narrow work area never clips right-side controls.
        for &(control, _) in &rows {
            let id = GetDlgCtrlID(control);
            let advanced = matches!(id, PROTOCOL | MANUAL | DELETE_KEY | 212..=215);
            let _ = ShowWindow(
                control,
                if advanced && !state.advanced {
                    SW_HIDE
                } else {
                    SW_SHOW
                },
            );
            if !font.is_invalid() {
                let selected_font = if id == 200 {
                    title_font
                } else if matches!(id, 205 | CATALOG_STATUS | 215) {
                    hint_font
                } else {
                    font
                };
                SendMessageW(
                    control,
                    WM_SETFONT,
                    WPARAM(selected_font.0 as usize),
                    LPARAM(0),
                );
            }
        }
        state.rows = rows;
        for old in state.extra_fonts.drain(..) {
            let _ = DeleteObject(old);
        }
        state.extra_fonts = vec![title_font, hint_font];
        if !font.is_invalid() {
            let old = std::mem::replace(&mut state.font, font);
            if !old.is_invalid() {
                let _ = DeleteObject(old);
            }
        }
        // Moving children alone leaves the old white-card pixels behind.
        position_rows(hwnd, state);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Never unwind through the Win32 ABI.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        handle(hwnd, msg, wp, lp)
    }))
    .unwrap_or(LRESULT(0))
}

unsafe fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    match msg {
        WM_DRAWITEM if !ptr.is_null() && wp.0 == SAVE as usize => unsafe {
            let item = &*(lp.0 as *const windows::Win32::UI::Controls::DRAWITEMSTRUCT);
            let brush = CreateSolidBrush(COLORREF(0x00696b0f));
            FillRect(item.hDC, &item.rcItem, brush);
            let _ = DeleteObject(brush);
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(item.hDC, COLORREF(0x00ffffff));
            let mut b = wide("保存并使用");
            let mut r = item.rcItem;
            DrawTextW(
                item.hDC,
                &mut b[..5],
                &mut r,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if item.itemState.0 & 0x10 != 0 {
                let _ = DrawFocusRect(item.hDC, &item.rcItem);
            }
            return LRESULT(1);
        },
        WM_TIMER if !ptr.is_null() => {
            if unsafe { !(*ptr).owner.is_invalid() && !IsWindow((*ptr).owner).as_bool() } {
                let _ = unsafe { DestroyWindow(hwnd) };
                return LRESULT(0);
            }
            poll_catalog(hwnd, unsafe { &mut *ptr });
            return LRESULT(0);
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX if !ptr.is_null() => unsafe {
            let dc = HDC(wp.0 as *mut _);
            SetTextColor(dc, COLORREF(0x003a2b17));
            SetBkColor(dc, COLORREF(0x00ffffff));
            return LRESULT((*ptr).card.0 as isize);
        },
        WM_ERASEBKGND if !ptr.is_null() => unsafe {
            let mut r = RECT::default();
            let _ = GetClientRect(hwnd, &mut r);
            FillRect(HDC(wp.0 as *mut _), &r, (*ptr).background);
            // White cards follow the same scroll coordinate system as children.
            for bounds in &(*ptr).cards {
                let mut card = *bounds;
                card.top -= (*ptr).scroll;
                card.bottom -= (*ptr).scroll;
                FillRect(HDC(wp.0 as *mut _), &card, (*ptr).card);
            }
            return LRESULT(1);
        },
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        },
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            return LRESULT(0);
        }
        WM_SIZE | WM_DPICHANGED if !ptr.is_null() => {
            if msg == WM_DPICHANGED && lp.0 != 0 {
                let r = unsafe { &*(lp.0 as *const RECT) };
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        HWND::default(),
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER,
                    )
                };
            }
            layout(hwnd, unsafe { &mut *ptr });
            return LRESULT(0);
        }
        WM_VSCROLL | WM_MOUSEWHEEL if !ptr.is_null() => {
            let delta = if msg == WM_MOUSEWHEEL {
                for id in [PROFILE, PROTOCOL, MODEL] {
                    let combo = unsafe { GetDlgItem(hwnd, id).unwrap_or_default() };
                    if unsafe { SendMessageW(combo, CB_GETDROPPEDSTATE, WPARAM(0), LPARAM(0)).0 }
                        != 0
                    {
                        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
                    }
                }
                let state = unsafe { &mut *ptr };
                let numerator = state.wheel_remainder - ((wp.0 >> 16) as u16 as i16 as i32) * 40;
                state.wheel_remainder = numerator % 120;
                numerator / 120
            } else {
                match wp.0 & 0xffff {
                    0 => -24,
                    1 => 24,
                    2 => -160,
                    3 => 160,
                    4 | 5 => {
                        let mut info = SCROLLINFO {
                            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                            fMask: SIF_TRACKPOS,
                            ..Default::default()
                        };
                        let _ = unsafe { GetScrollInfo(hwnd, SB_VERT, &mut info) };
                        info.nTrackPos - unsafe { (*ptr).scroll }
                    }
                    6 => -i32::MAX / 2,
                    7 => i32::MAX / 2,
                    _ => 0,
                }
            };
            unsafe {
                scroll_to(hwnd, &mut *ptr, (*ptr).scroll.saturating_add(delta));
            }
            return LRESULT(0);
        }
        WM_COMMAND if !ptr.is_null() => {
            if unsafe { (*ptr).updating } {
                return LRESULT(0);
            }
            let id = (wp.0 & 0xffff) as i32;
            let notification = wp.0 >> 16;
            if (notification == EN_CHANGE as usize && id == MANUAL)
                || (notification == CBN_SELCHANGE as usize && id == MODEL)
            {
                // Editing a model is also an explicit draft revision: a pending
                // directory must not later overwrite the user's choice.
                if let Some(job) = unsafe { (*ptr).catalog.take() } {
                    job.cancel.store(true, Ordering::Relaxed);
                    unsafe {
                        (*ptr).retired.push(job);
                    }
                }
            }
            if notification == CBN_SELCHANGE as usize && id == MODEL {
                label(hwnd, MANUAL, "");
            }
            if notification == EN_CHANGE as usize && id == KEY {
                key_changed(hwnd, unsafe { &mut *ptr });
            } else if (notification == EN_CHANGE as usize && matches!(id, NAME | URL))
                || (notification == CBN_SELCHANGE as usize && id == PROTOCOL)
            {
                invalidate_catalog(hwnd, unsafe { &mut *ptr });
            }
            if (wp.0 >> 16) == CBN_SELCHANGE as usize && id == PROFILE {
                let store = unsafe { (*ptr).store.clone() };
                if let Ok(latest) = store.load() {
                    let index = unsafe {
                        SendMessageW(
                            GetDlgItem(hwnd, PROFILE).unwrap_or_default(),
                            CB_GETCURSEL,
                            WPARAM(0),
                            LPARAM(0),
                        )
                        .0
                    } as usize;
                    if let Some(p) = latest.model_api.profiles.get(index) {
                        unsafe {
                            (*ptr).selected = p.clone();
                            refresh(hwnd, &mut *ptr, &latest.model_api);
                        }
                    }
                }
            } else if (wp.0 >> 16) == BN_CLICKED as usize {
                if id == CLOSE {
                    let _ = unsafe { DestroyWindow(hwnd) };
                    return LRESULT(0);
                }
                if let Err(e) = command(hwnd, ptr, id) {
                    if window_owns_state(hwnd, ptr) {
                        notice(hwnd, &e);
                    }
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn new_profile() -> ApiProfile {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    ApiProfile {
        id: format!(
            "api-{t:x}-{:x}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ),
        name: "新连接".into(),
        protocol: ApiProtocol::ChatCompletions,
        base_url: "https://api.example.com/v1".into(),
        model: String::new(),
    }
}
fn text(hwnd: HWND, id: i32) -> String {
    unsafe {
        let c = GetDlgItem(hwnd, id).unwrap_or_default();
        let mut b = vec![0; GetWindowTextLengthW(c).max(0) as usize + 1];
        let n = GetWindowTextW(c, &mut b) as usize;
        let s = String::from_utf16_lossy(&b[..n]);
        b.fill(0);
        s
    }
}
fn edit_profile(hwnd: HWND, p: &ApiProfile) -> Result<ApiProfile, String> {
    let n = unsafe {
        SendMessageW(
            GetDlgItem(hwnd, PROTOCOL).unwrap_or_default(),
            CB_GETCURSEL,
            WPARAM(0),
            LPARAM(0),
        )
        .0
    };
    Ok(ApiProfile {
        id: p.id.clone(),
        name: text(hwnd, NAME),
        protocol: match n {
            0 => ApiProtocol::ChatCompletions,
            1 => ApiProtocol::Responses,
            _ => return Err("请选择协议。".into()),
        },
        base_url: text(hwnd, URL),
        model: {
            let selected = text(hwnd, MODEL);
            if !text(hwnd, MANUAL).is_empty() {
                text(hwnd, MANUAL)
            } else {
                selected
            }
        },
    })
}
// Exact equality: unsaved equivalent edits and concurrent edits both fail closed.
fn credential_profile(s: &ModelApiSettings, p: &ApiProfile) -> Result<ApiProfile, String> {
    s.profiles
        .iter()
        .find(|saved| *saved == p)
        .ok_or_else(|| "字段与已保存连接不一致，请先保存连接。".to_owned())?
        .normalized()
        .map_err(|_| "连接无效，请先保存连接。".into())
}
fn upsert(s: &mut ModelApiSettings, p: ApiProfile, active: bool) -> Result<(), String> {
    let p = p
        .normalized()
        .map_err(|_| "请检查名称、HTTPS API 基址和模型 ID；基址不能是最终请求路径。")?;
    if let Some(old) = s.profiles.iter_mut().find(|old| old.id == p.id) {
        *old = p.clone();
    } else {
        if s.profiles.len() >= 16 {
            return Err("最多保存 16 组连接。".into());
        }
        s.profiles.push(p.clone());
    }
    if active {
        s.active_id = Some(p.id);
    } else if s.active_id.as_ref() == Some(&p.id) {
        s.active_id = None;
    }
    s.validate().map_err(|_| "连接配置无效。".into())
}
fn remove(s: &mut ModelApiSettings, id: &str) {
    s.profiles.retain(|p| p.id != id);
    if s.active_id.as_deref() == Some(id) {
        s.active_id = None;
    }
}
fn refresh(hwnd: HWND, state: &mut State, s: &ModelApiSettings) {
    state.updating = true;
    invalidate_catalog(hwnd, state);
    unsafe {
        let c = GetDlgItem(hwnd, PROFILE).unwrap_or_default();
        SendMessageW(c, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for p in &s.profiles {
            let label = wide(&format!(
                "{}{}",
                p.name,
                if s.active_id.as_ref() == Some(&p.id) {
                    "（当前）"
                } else {
                    ""
                }
            ));
            SendMessageW(c, CB_ADDSTRING, WPARAM(0), LPARAM(label.as_ptr() as isize));
        }
        SendMessageW(
            c,
            CB_SETCURSEL,
            WPARAM(
                s.profiles
                    .iter()
                    .position(|p| p.id == state.selected.id)
                    .unwrap_or(usize::MAX),
            ),
            LPARAM(0),
        );
        for (id, value) in [
            (NAME, &state.selected.name),
            (URL, &state.selected.base_url),
        ] {
            let b = wide(value);
            let _ = SetWindowTextW(GetDlgItem(hwnd, id).unwrap_or_default(), PCWSTR(b.as_ptr()));
        }
        SendMessageW(
            GetDlgItem(hwnd, PROTOCOL).unwrap_or_default(),
            CB_SETCURSEL,
            WPARAM(usize::from(
                state.selected.protocol == ApiProtocol::Responses,
            )),
            LPARAM(0),
        );
        let _ = SetWindowTextW(GetDlgItem(hwnd, KEY).unwrap_or_default(), w!(""));
        let combo = GetDlgItem(hwnd, MODEL).unwrap_or_default();
        if !state.selected.model.is_empty() {
            let b = wide(&state.selected.model);
            SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(b.as_ptr() as isize));
            SendMessageW(combo, CB_SETCURSEL, WPARAM(0), LPARAM(0));
        }
    }
    label(hwnd, MANUAL, &state.selected.model);
    label(
        hwnd,
        ADVANCED,
        if state.selected.protocol == ApiProtocol::Responses {
            "高级设置 ▸ 此连接保留 Responses 协议"
        } else {
            "高级设置 ▸ 默认 Chat Completions（推荐输入法）"
        },
    );
    label(
        hwnd,
        STATUS,
        &s.active()
            .map(|p| {
                format!(
                    "实际当前：{} / {}\n未调用验证；保存不代表生成成功。",
                    p.name, p.model
                )
            })
            .unwrap_or_else(|_| "实际当前：未选择连接\n未调用验证。".into()),
    );
    state.updating = false;
}
fn initialize(
    hwnd: HWND,
    state: &mut State,
    initial: ModelApiSettings,
    instance: HINSTANCE,
) -> Result<(), String> {
    let specs=[
    (200,"STATIC","模型服务设置",0,[24,10,550,28]),
    (201,"STATIC","服务连接",0,[24,54,550,24]),
    (PROFILE,"COMBOBOX","",CBS_DROPDOWNLIST as u32|WS_VSCROLL.0,[24,84,330,160]),
    (NEW,"BUTTON","新建",0,[366,84,88,32]),(DELETE,"BUTTON","删除连接…",0,[464,84,110,32]),
    (202,"STATIC","连接名称",0,[24,124,550,22]),(NAME,"EDIT","",ES_AUTOHSCROLL as u32|WS_BORDER.0,[24,150,550,30]),
    (203,"STATIC","HTTPS API 基址（不自动补 /v1）",0,[24,188,550,22]),(URL,"EDIT","",ES_AUTOHSCROLL as u32|WS_BORDER.0,[24,214,550,30]),
    (204,"STATIC","新密钥（留空沿用此目标已保存的密钥）",0,[24,252,550,22]),
    (KEY,"EDIT","",ES_PASSWORD as u32|ES_AUTOHSCROLL as u32|WS_BORDER.0,[24,278,550,30]),
    (205,"STATIC","密钥仅存本机凭据管理器，不读取或显示旧值。",0,[24,316,550,24]),
    (209,"STATIC","选择模型",0,[24,370,330,24]),
    (FETCH,"BUTTON","获取模型列表",0,[410,366,164,34]),
    (MODEL,"COMBOBOX","",CBS_DROPDOWNLIST as u32|WS_VSCROLL.0,[24,414,550,180]),
    (CATALOG_STATUS,"STATIC","请主动获取模型列表，不会自动选择付费模型。",0,[24,458,550,60]),
    (210,"STATIC","实际应用",0,[24,562,550,24]),
    (STATUS,"STATIC","未调用验证",0,[24,594,550,64]),
    (SAVE,"BUTTON","保存并使用",BS_OWNERDRAW as u32,[24,704,200,40]),
    (CLOSE,"BUTTON","关闭",0,[446,704,128,40]),
    (ADVANCED,"BUTTON","高级设置 ▸ 默认 Chat Completions（推荐输入法）",0,[24,756,550,36]),
    (212,"STATIC","协议（旧 Responses 不会自动迁移，需明确改选）",0,[24,808,550,24]),
    (PROTOCOL,"COMBOBOX","",CBS_DROPDOWNLIST as u32|WS_VSCROLL.0,[24,838,550,120]),
    (213,"STATIC","仅目录不可用时：手动模型 ID（填写即优先使用）",0,[24,880,550,24]),
    (MANUAL,"EDIT","",ES_AUTOHSCROLL as u32|WS_BORDER.0,[24,908,550,30]),
    (DELETE_KEY,"BUTTON","删除已保存密钥…",0,[24,954,220,36]),
    (215,"STATIC","DeepSeek 常规接口请选择 Chat。智谱 Coding Plan 限适用工具，Flower 不伪装工具或保证套餐可用。",0,[24,1000,550,48])];
    for (id, class, label, extra, rect) in specs {
        let button = class == "BUTTON";
        let class = wide(class);
        let label = wide(label);
        let c = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(label.as_ptr()),
                WINDOW_STYLE(
                    WS_CHILD.0
                        | WS_VISIBLE.0
                        | if id < 200 { WS_TABSTOP.0 } else { 0 }
                        | extra
                        | if button { BS_MULTILINE as u32 } else { 0 },
                ),
                0,
                0,
                0,
                0,
                hwnd,
                HMENU(id as *mut _),
                instance,
                None,
            )
        }
        .map_err(|_| "无法创建模型 API 控件。")?;
        state.controls.push((c, rect));
    }
    unsafe {
        for label in ["Chat Completions", "Responses"] {
            let b = wide(label);
            SendMessageW(
                GetDlgItem(hwnd, PROTOCOL).unwrap_or_default(),
                CB_ADDSTRING,
                WPARAM(0),
                LPARAM(b.as_ptr() as isize),
            );
        }
        for (id, n) in [(NAME, 80), (URL, 1024), (MANUAL, 160), (KEY, 2048)] {
            SendMessageW(
                GetDlgItem(hwnd, id).unwrap_or_default(),
                EM_SETLIMITTEXT,
                WPARAM(n),
                LPARAM(0),
            );
        }
    }
    refresh(hwnd, state, &initial);
    layout(hwnd, state);
    Ok(())
}
fn confirmed(hwnd: HWND, label: &str) -> bool {
    let b = wide(label);
    unsafe {
        MessageBoxW(
            hwnd,
            PCWSTR(b.as_ptr()),
            w!("确认删除"),
            MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING,
        ) == IDYES
    }
}
fn window_owns_state(hwnd: HWND, ptr: *mut State) -> bool {
    !ptr.is_null()
        && unsafe { IsWindow(hwnd) }.as_bool()
        && unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } == ptr as isize
}

/// Freeze all three identities before the confirmation's nested message pump.
/// Never hold a state reference across it, or dereference a destroyed window's
/// state. The injected sink keeps regression fixtures away from real credentials.
fn delete_key_after_confirmation(
    hwnd: HWND,
    ptr: *mut State,
    confirm: impl FnOnce() -> bool,
    delete: impl FnOnce(&ApiProfile) -> Result<(), String>,
) -> Result<bool, String> {
    if !window_owns_state(hwnd, ptr) {
        return Ok(false);
    }
    let store = unsafe { (*ptr).store.clone() };
    let selected = unsafe { (*ptr).selected.clone() };
    let edited = edit_profile(hwnd, &selected)?;
    let latest = store.load().map_err(|_| "读取模型设置失败。")?;
    let frozen = credential_profile(&latest.model_api, &edited)?;
    if !confirm() || !window_owns_state(hwnd, ptr) {
        return Ok(false);
    }
    let current_selection = unsafe { (*ptr).selected.clone() };
    if current_selection != selected || edit_profile(hwnd, &current_selection)? != edited {
        return Err("确认期间连接或字段已变化，已取消删除；请重新确认目标。".into());
    }
    let latest = store.load().map_err(|_| "读取模型设置失败。")?;
    let revalidated = credential_profile(&latest.model_api, &edited)?;
    if revalidated != frozen || !window_owns_state(hwnd, ptr) {
        return Err("确认期间已保存连接发生变化，已取消删除。".into());
    }
    // The sink receives exactly the frozen profile, not a newly substituted
    // destination that happened to pass a different qualification check.
    delete(&frozen)?;
    Ok(true)
}

fn command(hwnd: HWND, ptr: *mut State, id: i32) -> Result<(), String> {
    let store = unsafe { (*ptr).store.clone() };
    let selected = unsafe { (*ptr).selected.clone() };
    let mut latest = store.load().map_err(|_| "读取模型设置失败。")?.model_api;
    match id {
        ADVANCED => unsafe {
            (*ptr).advanced = !(*ptr).advanced;
            label(
                hwnd,
                ADVANCED,
                if (*ptr).advanced {
                    "高级设置 ▾ 收起"
                } else {
                    "高级设置 ▸ 展开（协议 / 手动模型 / 删除密钥）"
                },
            );
            layout(hwnd, &mut *ptr);
            ensure_focus_visible(hwnd);
        },
        FETCH => start_catalog(hwnd, unsafe { &mut *ptr })?,
        NEW => unsafe {
            (*ptr).selected = new_profile();
            refresh(hwnd, &mut *ptr, &latest);
        },
        SAVE => {
            let p = edit_profile(hwnd, &selected)?;
            let svc = unsafe { (*ptr).services.clone() };
            let mut input = text(hwnd, KEY);
            let saved = save_and_use(&store, &*svc, &p, &input);
            unsafe {
                input.as_bytes_mut().fill(0);
            }
            let p = saved?;
            latest = svc.load(&store)?;
            unsafe {
                (*ptr).selected = p;
                refresh(hwnd, &mut *ptr, &latest);
            }
            notice(
                hwnd,
                "已保存并应用，未调用验证。请重新切换 Flower，使润色使用新的模型配置。",
            );
        }
        DELETE => {
            if !latest.profiles.iter().any(|p| p.id == selected.id) {
                return Err("请先选择已保存连接。".into());
            }
            if !confirmed(hwnd, "删除此连接？不会删除密钥，也不自动选择其它当前连接。")
            {
                return Ok(());
            }
            latest = store.load().map_err(|_| "读取模型设置失败。")?.model_api;
            remove(&mut latest, &selected.id);
            config::persist(&store, &latest)?;
            unsafe {
                (*ptr).selected = new_profile();
                refresh(hwnd, &mut *ptr, &latest);
            }
        }
        DELETE_KEY => {
            let deleted = delete_key_after_confirmation(
                hwnd,
                ptr,
                || confirmed(hwnd, "删除此已保存连接的密钥？不删除连接。"),
                |profile| credentials::delete_key_for(profile).map_err(|_| "删除密钥失败。".into()),
            )?;
            if deleted && window_owns_state(hwnd, ptr) {
                notice(hwnd, "密钥已删除，未联网。");
            }
        }
        _ => {}
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Fake {
        settings: Mutex<ModelApiSettings>,
        keys: Mutex<Vec<String>>,
        reads: Mutex<Vec<ApiProfile>>,
        calls: Mutex<Vec<ApiProfile>>,
        saved_keys: Mutex<Vec<(ApiProfile, String)>>,
        fail_key: AtomicBool,
        fail_config: AtomicBool,
        block: AtomicBool,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                settings: Mutex::new(ModelApiSettings::default()),
                keys: Mutex::new(Vec::new()),
                reads: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                saved_keys: Mutex::new(Vec::new()),
                fail_key: AtomicBool::new(false),
                fail_config: AtomicBool::new(false),
                block: AtomicBool::new(false),
            }
        }
    }
    impl Services for Fake {
        fn load(&self, _: &SettingsStore) -> Result<ModelApiSettings, String> {
            Ok(self.settings.lock().unwrap().clone())
        }
        fn persist(&self, _: &SettingsStore, s: &ModelApiSettings) -> Result<(), String> {
            if self.fail_config.load(Ordering::Relaxed) {
                return Err("fake config failure".into());
            }
            *self.settings.lock().unwrap() = s.clone();
            Ok(())
        }
        fn key(&self, p: &ApiProfile) -> Result<Option<credentials::SecretKey>, String> {
            self.reads.lock().unwrap().push(p.clone());
            Ok(self
                .keys
                .lock()
                .unwrap()
                .contains(&p.credential_target().unwrap())
                .then(|| credentials::SecretKey::new("fake-not-a-secret").unwrap()))
        }
        fn save(&self, p: &ApiProfile, key: &credentials::SecretKey) -> Result<(), String> {
            if self.fail_key.load(Ordering::Relaxed) {
                return Err("fake key failure".into());
            }
            self.keys
                .lock()
                .unwrap()
                .push(p.credential_target().unwrap());
            self.saved_keys
                .lock()
                .unwrap()
                .push((p.clone(), key.expose().to_owned()));
            Ok(())
        }
        fn models(
            &self,
            p: &ApiProfile,
            _: &credentials::SecretKey,
            cancel: &AtomicBool,
        ) -> Result<Vec<String>, String> {
            self.calls.lock().unwrap().push(p.clone());
            while self.block.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(vec!["sample-chat".into(), "sample-reasoner".into()])
        }
    }
    #[test]
    fn model_api_save_and_use_credentials_before_activation() {
        let svc = Fake::new();
        let store = SettingsStore::new(std::path::PathBuf::from("unused-fake-store"));
        let mut p = new_profile();
        assert!(save_and_use(&store, &svc, &p, "fake").is_err());
        assert!(svc.keys.lock().unwrap().is_empty());
        p.model = "explicit-model".into();
        svc.fail_key.store(true, Ordering::Relaxed);
        assert!(save_and_use(&store, &svc, &p, "fake").is_err());
        assert_eq!(
            svc.settings.lock().unwrap().active_id.as_deref(),
            Some("legacy-bigmodel")
        );
        svc.fail_key.store(false, Ordering::Relaxed);
        svc.fail_config.store(true, Ordering::Relaxed);
        assert!(save_and_use(&store, &svc, &p, "fake")
            .unwrap_err()
            .contains("密钥已保存"));
        assert_eq!(
            svc.settings.lock().unwrap().active_id.as_deref(),
            Some("legacy-bigmodel")
        );
        svc.fail_config.store(false, Ordering::Relaxed);
        let frozen = save_and_use(&store, &svc, &p, "").unwrap();
        let next = svc.load(&store).unwrap().active().unwrap();
        assert_eq!(next.scope().unwrap(), frozen.scope().unwrap());
        assert!(svc.key(&next).unwrap().is_some());
        p.base_url = "https://other.example/api".into();
        assert!(save_and_use(&store, &svc, &p, "").is_err());
        assert_eq!(svc.load(&store).unwrap().active().unwrap(), next);
        assert!(
            svc.calls.lock().unwrap().is_empty(),
            "saving must not request catalog or generation"
        );
    }
    #[test]
    fn model_api_catalog_key_qualification_is_connection_exact() {
        let s = ModelApiSettings::default();
        let saved = s.profiles[0].clone();
        let mut draft = saved.clone();
        draft.model.clear();
        assert!(draft.normalized().is_err());
        assert_eq!(catalog_credential(&s, &draft).unwrap(), saved);
        draft.base_url.push('/');
        assert_eq!(catalog_credential(&s, &draft).unwrap(), saved);
        for field in 0..3 {
            let mut changed = draft.clone();
            match field {
                0 => changed.id = "other-id".into(),
                1 => changed.base_url = "https://other.example/v1".into(),
                _ => changed.protocol = ApiProtocol::Responses,
            }
            assert!(catalog_credential(&s, &changed).is_err());
        }
        assert_eq!(new_profile().protocol, ApiProtocol::ChatCompletions);
    }
    #[test]
    #[ignore = "owned window and synthetic keys with injected services only"]
    fn model_api_owned_key_rotation_preserves_models_and_discards_late_catalog() {
        let root = owned_root("model-key-rotation");
        let store = SettingsStore::new(root.join("settings.json"));
        let initial = crate::settings::Settings::default();
        store.save(&initial).unwrap();
        for manual in [false, true] {
            let fake = Arc::new(Fake::new());
            fake.keys
                .lock()
                .unwrap()
                .push(initial.model_api.profiles[0].credential_target().unwrap());
            let mut probe = Ok(());
            show_model_api_settings_impl(HWND::default(), store.clone(), |hwnd| {
                probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                    (*ptr).services = fake.clone();
                    start_catalog(hwnd, &mut *ptr).unwrap();
                    while !(*ptr)
                        .catalog
                        .as_ref()
                        .unwrap()
                        .thread
                        .as_ref()
                        .unwrap()
                        .is_finished()
                    {
                        std::thread::yield_now();
                    }
                    poll_catalog(hwnd, &mut *ptr);
                    let combo = GetDlgItem(hwnd, MODEL).unwrap();
                    SendMessageW(combo, CB_SETCURSEL, WPARAM(0), LPARAM(0));
                    SendMessageW(
                        hwnd,
                        WM_COMMAND,
                        WPARAM(MODEL as usize | ((CBN_SELCHANGE as usize) << 16)),
                        LPARAM(combo.0 as isize),
                    );
                    let expected_model = if manual {
                        command(hwnd, ptr, ADVANCED).unwrap();
                        label(hwnd, MANUAL, "explicit-manual-model");
                        "explicit-manual-model"
                    } else {
                        "sample-chat"
                    };
                    let frozen = edit_profile(hwnd, &(*ptr).selected).unwrap();
                    assert_eq!(frozen.model, expected_model);
                    // Force a late successful response despite cancellation, with
                    // exactly the same connection identity. It must never revive.
                    let (release, gate) = std::sync::mpsc::channel();
                    let cancel = Arc::new(AtomicBool::new(false));
                    (*ptr).catalog = Some(CatalogJob {
                        target: frozen.connection_normalized().unwrap(),
                        cancel: cancel.clone(),
                        thread: Some(std::thread::spawn(move || {
                            gate.recv().unwrap();
                            Ok(vec!["late-old-key-model".into()])
                        })),
                    });
                    let calls = fake.calls.lock().unwrap().len();
                    label(hwnd, KEY, "new-synthetic-key");
                    assert!(cancel.load(Ordering::Relaxed));
                    assert!((*ptr).catalog.is_none());
                    assert_eq!(text(hwnd, MODEL), "sample-chat");
                    assert_eq!(text(hwnd, MANUAL), if manual { expected_model } else { "" });
                    assert_eq!(edit_profile(hwnd, &(*ptr).selected).unwrap(), frozen);
                    assert!(text(hwnd, CATALOG_STATUS).contains("目录尚未经新密钥核验"));
                    release.send(()).unwrap();
                    for job in &mut (*ptr).retired {
                        assert_eq!(
                            job.thread.take().unwrap().join().unwrap().unwrap(),
                            vec!["late-old-key-model"]
                        );
                    }
                    poll_catalog(hwnd, &mut *ptr);
                    assert_eq!(text(hwnd, MODEL), "sample-chat");
                    assert_eq!(SendMessageW(combo, CB_GETCOUNT, WPARAM(0), LPARAM(0)).0, 2);
                    assert_eq!(edit_profile(hwnd, &(*ptr).selected).unwrap(), frozen);
                    assert_eq!(
                        fake.calls.lock().unwrap().len(),
                        calls,
                        "key edits must not automatically request a catalog"
                    );
                    SUPPRESS_NOTICES.with(|value| value.set(true));
                    let applied = command(hwnd, ptr, SAVE);
                    SUPPRESS_NOTICES.with(|value| value.set(false));
                    applied.unwrap();
                    let actual = fake.load(&store).unwrap().active().unwrap();
                    assert_eq!(actual, frozen);
                    assert_eq!(
                        fake.saved_keys.lock().unwrap().as_slice(),
                        &[(frozen, "new-synthetic-key".to_owned())]
                    );
                    assert!(text(hwnd, STATUS).contains(expected_model));
                }));
                if unsafe { IsWindow(hwnd) }.as_bool() {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
            })
            .unwrap();
            probe.unwrap();
        }
        assert_eq!(
            store.load().unwrap(),
            initial,
            "only injected fake configuration was applied"
        );
        std::fs::remove_file(store.path()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    #[ignore = "owned windows and fake services only"]
    fn model_api_owned_catalog_cancel_and_protocol_preservation() {
        let root = owned_root("model-catalog-fake");
        let store = SettingsStore::new(root.join("settings.json"));
        let mut settings = crate::settings::Settings::default();
        settings.model_api.profiles[0].protocol = ApiProtocol::Responses;
        store.save(&settings).unwrap();
        let fake = Arc::new(Fake::new());
        *fake.settings.lock().unwrap() = settings.model_api.clone();
        fake.keys
            .lock()
            .unwrap()
            .push(settings.model_api.profiles[0].credential_target().unwrap());
        fake.block.store(true, Ordering::Relaxed);
        let mut probe = Ok(());
        show_model_api_settings_impl(HWND::default(), store.clone(), |hwnd| {
            probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                (*ptr).services = fake.clone();
                assert_eq!(
                    edit_profile(hwnd, &(*ptr).selected).unwrap().protocol,
                    ApiProtocol::Responses
                );
                assert!(text(hwnd, KEY).is_empty());
                command(hwnd, ptr, ADVANCED).unwrap();
                label(hwnd, MANUAL, "explicit-manual-model");
                command(hwnd, ptr, ADVANCED).unwrap();
                assert_eq!(
                    edit_profile(hwnd, &(*ptr).selected).unwrap().model,
                    "explicit-manual-model",
                    "collapsing advanced settings must not discard an explicit model"
                );
                start_catalog(hwnd, &mut *ptr).unwrap();
                // A URL change cancels without joining/blocking the UI thread.
                label(hwnd, URL, "https://other.example/v1");
                assert!((*ptr).catalog.is_none());
                assert!(text(hwnd, MODEL).is_empty());
                fake.block.store(false, Ordering::Relaxed);
                for job in &mut (*ptr).retired {
                    let _ = job.thread.take().unwrap().join();
                }
                poll_catalog(hwnd, &mut *ptr);
                assert!(text(hwnd, MODEL).is_empty());
                // No saved credential is eligible for the new origin.
                let reads = fake.reads.lock().unwrap().len();
                start_catalog(hwnd, &mut *ptr).unwrap();
                let job = (*ptr).catalog.as_mut().unwrap();
                while !job.thread.as_ref().unwrap().is_finished() {
                    std::thread::yield_now();
                }
                poll_catalog(hwnd, &mut *ptr);
                assert_eq!(fake.reads.lock().unwrap().len(), reads);
                assert!(text(hwnd, CATALOG_STATUS).contains("新密钥"));
                label(hwnd, KEY, "fake-key");
                start_catalog(hwnd, &mut *ptr).unwrap();
                while !(*ptr)
                    .catalog
                    .as_ref()
                    .unwrap()
                    .thread
                    .as_ref()
                    .unwrap()
                    .is_finished()
                {
                    std::thread::yield_now();
                }
                poll_catalog(hwnd, &mut *ptr);
                assert_eq!(
                    SendMessageW(
                        GetDlgItem(hwnd, MODEL).unwrap(),
                        CB_GETCOUNT,
                        WPARAM(0),
                        LPARAM(0)
                    )
                    .0,
                    2
                );
                assert!(
                    text(hwnd, MODEL).is_empty(),
                    "never auto-select first model"
                );
                fake.block.store(true, Ordering::Relaxed);
                start_catalog(hwnd, &mut *ptr).unwrap();
                DestroyWindow(hwnd).unwrap();
            }));
            if unsafe { IsWindow(hwnd) }.as_bool() {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        })
        .unwrap();
        probe.unwrap();
        assert_eq!(store.load().unwrap(), settings, "catalog must not persist");
        std::fs::remove_file(store.path()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    #[ignore = "owned modal and isolated Store; injected delete sink only"]
    fn model_api_owned_delete_confirmation_revalidates_frozen_target() {
        let root = owned_root("model-delete-confirmation");
        let store = SettingsStore::new(root.join("settings.json"));
        // 0 unchanged; 1 selected ID; 2 same-ID endpoint plus editor;
        // 3 closed window; 4 detached ownership; 5 saved-only edit; 6 cancel.
        for scenario in 0..7 {
            store.save(&crate::settings::Settings::default()).unwrap();
            let mut probe = Ok(());
            let result = show_model_api_settings_impl(HWND::default(), store.clone(), |hwnd| {
                probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                    let frozen = (*ptr).selected.clone();
                    let calls = std::cell::RefCell::new(Vec::<ApiProfile>::new());
                    let deleted = delete_key_after_confirmation(
                        hwnd,
                        ptr,
                        || {
                            match scenario {
                                1 => {
                                    // Same visible fields, different selected identity.
                                    (*ptr).selected.id = "other-selected-profile".into();
                                }
                                2 | 5 => {
                                    let mut latest = store.load().unwrap();
                                    latest.model_api.profiles[0].base_url =
                                        "https://changed.example/v1".into();
                                    store.save(&latest).unwrap();
                                    if scenario == 2 {
                                        let url = wide(&latest.model_api.profiles[0].base_url);
                                        SetWindowTextW(
                                            GetDlgItem(hwnd, URL).unwrap(),
                                            PCWSTR(url.as_ptr()),
                                        )
                                        .unwrap();
                                    }
                                }
                                3 => {
                                    DestroyWindow(hwnd).unwrap();
                                }
                                4 => {
                                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                                }
                                _ => {}
                            }
                            scenario != 6
                        },
                        |target| {
                            calls.borrow_mut().push(target.clone());
                            Ok(())
                        },
                    );
                    if scenario == 4 {
                        // Restore only this fixture's still-live state for teardown.
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
                    }
                    if scenario == 0 {
                        assert!(deleted.unwrap());
                        assert_eq!(&*calls.borrow(), std::slice::from_ref(&frozen));
                        assert_eq!(
                            calls.borrow()[0].credential_target().unwrap(),
                            frozen.credential_target().unwrap()
                        );
                    } else {
                        assert!(!matches!(deleted, Ok(true)));
                        assert!(
                            calls.borrow().is_empty(),
                            "scenario {scenario} deleted a key"
                        );
                    }
                }));
                if unsafe { IsWindow(hwnd) }.as_bool() {
                    unsafe {
                        DestroyWindow(hwnd).unwrap();
                    }
                }
            });
            result.unwrap();
            probe.unwrap();
        }
        std::fs::remove_file(store.path()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn model_api_crud_selection_and_credential_eligibility() {
        let mut s = ModelApiSettings::default();
        let mut p = new_profile();
        p.model = "owned-model".into();
        upsert(&mut s, p.clone(), false).unwrap();
        assert_eq!(s.active_id.as_deref(), Some("legacy-bigmodel"));
        assert_eq!(credential_profile(&s, &p).unwrap(), p);
        for field in 0..5 {
            let mut edited = p.clone();
            match field {
                0 => edited.name.push('x'),
                1 => edited.protocol = ApiProtocol::Responses,
                2 => edited.base_url.push_str("/new"),
                3 => edited.model.push('x'),
                _ => edited.id.push('x'),
            }
            assert!(credential_profile(&s, &edited).is_err());
        }
        p.protocol = ApiProtocol::Responses;
        upsert(&mut s, p.clone(), true).unwrap();
        assert_eq!(s.active().unwrap(), p);
        remove(&mut s, &p.id);
        assert!(s.active_id.is_none());
        assert!(s.active().is_err());
        assert!(credential_profile(&s, &p).is_err());
        upsert(&mut s, p.clone(), true).unwrap();
        upsert(&mut s, p, false).unwrap();
        assert!(s.active_id.is_none());
        while s.profiles.len() < 16 {
            let mut p = new_profile();
            p.model = "m".into();
            upsert(&mut s, p, false).unwrap();
        }
        let mut p = new_profile();
        p.model = "m".into();
        assert!(upsert(&mut s, p, false).is_err());
        assert!(credentials::SecretKey::new("").is_err());
    }
    fn owned_root(name: &str) -> std::path::PathBuf {
        let root = std::path::PathBuf::from(
            std::env::var_os("FLOWER_SETTINGS_PREVIEW_DIR")
                .expect("explicit owned directory required"),
        )
        .join(name);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn model_api_default_window_sizes() {
        for dpi in [96, 144, 192] {
            let work = RECT {
                left: -400,
                top: 80,
                right: 3600,
                bottom: 3080,
            };
            let r = default_window_rect(work, dpi);
            assert_eq!(r.right - r.left, 820 * dpi as i32 / 96);
            assert_eq!(r.bottom - r.top, 900 * dpi as i32 / 96);
            let small = RECT {
                left: 100,
                top: 40,
                right: 460,
                bottom: 280,
            };
            let r = default_window_rect(small, dpi);
            assert_eq!((r.left, r.top, r.right, r.bottom), (100, 40, 460, 280));
        }
    }

    #[test]
    #[ignore = "owned production modal pump; FLOWER_SETTINGS_PREVIEW_DIR required"]
    fn model_api_owned_pump_manual_scroll_and_tab() {
        use std::cell::RefCell;
        use std::rc::Rc;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetKeyboardState, SetKeyboardState, VK_SHIFT, VK_TAB,
        };
        struct RestoreKeyboard([u8; 256]);
        impl Drop for RestoreKeyboard {
            fn drop(&mut self) {
                // Only this UI thread's state; no global input injection.
                let _ = unsafe { SetKeyboardState(&self.0) };
            }
        }
        const CHECK: u32 = WM_APP + 731;
        let root = owned_root("pump-scroll");
        let store = SettingsStore::new(root.join("settings.json"));
        store.save(&Default::default()).unwrap();
        for focus_id in [NAME, PROFILE] {
            let mut keys = [0; 256];
            unsafe {
                GetKeyboardState(&mut keys).unwrap();
            }
            let _restore = RestoreKeyboard(keys);
            let mut isolated_keys = [0; 256];
            unsafe {
                SetKeyboardState(&isolated_keys).unwrap();
            }
            let outcome = Rc::new(RefCell::new(Ok(())));
            let completed = Rc::new(std::cell::Cell::new(false));
            let report = outcome.clone();
            let finished = completed.clone();
            show_model_api_settings_impl(HWND::default(), store.clone(), move |hwnd| unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                (*ptr).services = Arc::new(Fake::new());
                SetWindowPos(hwnd, HWND::default(), 0, 0, 390, 260,
                    SWP_NOMOVE | SWP_NOZORDER).unwrap();
                let initial_focus = GetDlgItem(hwnd, focus_id).unwrap();
                let _ = SetFocus(initial_focus);
                scroll_to(hwnd, &mut *ptr, 0);
                let mut navigation = 0;
                let mut expected_scroll = 80;
                let mut saw_timer = false;
                let mut saw_paint = false;
                let mut timer_checks = 0;
                let mut paint_checks = 0;
                (*ptr).pump_probe = Some(Box::new(move |hwnd, message| {
                    if finished.get() || report.borrow().is_err() { return; }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                        if message.message == WM_TIMER { saw_timer = true; }
                        if message.message == WM_PAINT { saw_paint = true; }
                        if message.message == CHECK {
                            assert_eq!(GetFocus(), initial_focus);
                            assert_eq!((*ptr).scroll, expected_scroll,
                                "manual scroll snapped back with focused control {focus_id}");
                            assert!(saw_timer && saw_paint, "queued timer/paint were not pumped");
                            timer_checks += usize::from(saw_timer);
                            paint_checks += usize::from(saw_paint);
                            if message.wParam.0 == 1 {
                                expected_scroll = ((*ptr).scroll + 160).min((*ptr).max_scroll);
                                PostMessageW(hwnd, WM_VSCROLL, WPARAM(3), LPARAM(0)).unwrap();
                                PostMessageW(hwnd, WM_TIMER, WPARAM(1), LPARAM(0)).unwrap();
                                PostMessageW(hwnd, WM_PAINT, WPARAM(0), LPARAM(0)).unwrap();
                                PostMessageW(hwnd, CHECK, WPARAM(2), LPARAM(0)).unwrap();
                                saw_timer = false;
                                saw_paint = false;
                            } else {
                                PostMessageW(initial_focus, WM_KEYDOWN,
                                    WPARAM(VK_TAB.0 as usize), LPARAM(0)).unwrap();
                            }
                        } else if message.message == WM_KEYDOWN
                            && message.wParam.0 == VK_TAB.0 as usize {
                            assert_focus_visible(hwnd);
                            navigation += 1;
                            if navigation == 1 {
                                assert_ne!(GetFocus(), initial_focus, "Tab did not navigate");
                                // Per-thread modifier state lets the real dialog route
                                // process Shift+Tab without touching global keyboard input.
                                isolated_keys[VK_SHIFT.0 as usize] = 0x80;
                                SetKeyboardState(&isolated_keys).unwrap();
                                // Make the reverse target offscreen, without changing focus.
                                scroll_to(hwnd, &mut *ptr, (*ptr).max_scroll);
                                PostMessageW(GetFocus(), WM_KEYDOWN,
                                    WPARAM(VK_TAB.0 as usize), LPARAM(0)).unwrap();
                            } else {
                                assert_eq!(GetFocus(), initial_focus, "Shift+Tab did not reverse");
                                isolated_keys[VK_SHIFT.0 as usize] = 0;
                                SetKeyboardState(&isolated_keys).unwrap();
                                finished.set(true);
                                println!("focus {focus_id}: posted wheel+vscroll retained through {timer_checks} timer/{paint_checks} paint checks; Tab and Shift+Tab visible");
                                PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap();
                            }
                        }
                    }));
                    if let Err(error) = result {
                        *report.borrow_mut() = Err(error);
                        PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap();
                    }
                }));
                PostMessageW(hwnd, WM_MOUSEWHEEL,
                    WPARAM((-240i16 as u16 as usize) << 16), LPARAM(0)).unwrap();
                PostMessageW(hwnd, WM_TIMER, WPARAM(1), LPARAM(0)).unwrap();
                PostMessageW(hwnd, WM_PAINT, WPARAM(0), LPARAM(0)).unwrap();
                PostMessageW(hwnd, CHECK, WPARAM(1), LPARAM(0)).unwrap();
            }).unwrap();
            if let Err(error) = std::mem::replace(&mut *outcome.borrow_mut(), Ok(())) {
                std::panic::resume_unwind(error);
            }
            assert!(completed.get(), "production pump fixture did not finish");
        }
        assert_eq!(store.load().unwrap(), crate::settings::Settings::default());
        std::fs::remove_file(store.path()).unwrap();
    }

    #[test]
    #[ignore = "owned HWND scroll fixture; FLOWER_SETTINGS_PREVIEW_DIR required"]
    fn model_api_owned_scroll_snapshot() {
        let root = owned_root("scroll-snapshot");
        let store = SettingsStore::new(root.join("settings.json"));
        store.save(&Default::default()).unwrap();
        let mut probe = Ok(());
        show_model_api_settings_impl(HWND::default(), store.clone(), |hwnd| {
            probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                (*ptr).services = Arc::new(Fake::new());
                let _ = SetFocus(hwnd);
                let _ = UpdateWindow(hwnd);
                write_preview_bitmap(hwnd, &root.join("default.bmp"));
                SetWindowPos(hwnd, HWND::default(), 0, 0, 390, 260,
                    SWP_NOMOVE | SWP_NOZORDER).unwrap();
                scroll_to(hwnd, &mut *ptr, 0);
                let _ = UpdateWindow(hwnd);
                write_preview_bitmap(hwnd, &root.join("before.bmp"));
                let layouts = (*ptr).layout_count;
                let measured = ((*ptr).font_creations, (*ptr).text_measurements);
                let fonts = ((*ptr).font, (*ptr).extra_fonts.clone());
                let wheel = |delta: i16| {
                    SendMessageW(hwnd, WM_MOUSEWHEEL,
                        WPARAM((delta as u16 as usize) << 16), LPARAM(0));
                };
                let redraws = (*ptr).redraw_count.get();
                for _ in 0..100 { wheel(120); }
                assert_eq!((*ptr).redraw_count.get(), redraws);
                for _ in 0..120 { wheel(-1); }
                assert_eq!((*ptr).scroll, 40);
                for _ in 0..120 { wheel(1); }
                assert_eq!((*ptr).scroll, 0);
                for _ in 0..10 {
                    for _ in 0..8 { wheel(-15); }
                    for _ in 0..8 { wheel(15); }
                }
                assert_eq!((*ptr).scroll, 0);
                assert_eq!((*ptr).layout_count, layouts);
                assert_eq!(((*ptr).font_creations, (*ptr).text_measurements), measured);
                assert_eq!(((*ptr).font, (*ptr).extra_fonts.clone()), fonts);
                let _ = UpdateWindow(hwnd);
                let roundtrip = client_rgb(hwnd);
                write_preview_bitmap(hwnd, &root.join("roundtrip.bmp"));
                let _ = RedrawWindow(hwnd, None, HRGN::default(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW);
                write_preview_bitmap(hwnd, &root.join("full.bmp"));
                assert_eq!(roundtrip, client_rgb(hwnd), "client RGB changed after full redraw");
                assert_eq!(std::fs::read(root.join("roundtrip.bmp")).unwrap(),
                    std::fs::read(root.join("full.bmp")).unwrap());
                command(hwnd, ptr, ADVANCED).unwrap();
                scroll_to(hwnd, &mut *ptr, (*ptr).max_scroll);
                let bottom_redraws = (*ptr).redraw_count.get();
                for _ in 0..100 { wheel(-120); }
                assert_eq!((*ptr).redraw_count.get(), bottom_redraws);
                let advanced_counts = ((*ptr).layout_count, (*ptr).font_creations, (*ptr).text_measurements);
                let _ = UpdateWindow(hwnd);
                let advanced_before = client_rgb(hwnd);
                for _ in 0..5 {
                    for _ in 0..8 { wheel(15); }
                    for _ in 0..8 { wheel(-15); }
                }
                let _ = UpdateWindow(hwnd);
                let advanced_after = client_rgb(hwnd);
                assert_eq!(advanced_before, advanced_after);
                let _ = RedrawWindow(hwnd, None, HRGN::default(),
                    RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW);
                assert_eq!(advanced_after, client_rgb(hwnd));
                assert_eq!(((*ptr).layout_count, (*ptr).font_creations, (*ptr).text_measurements), advanced_counts);
                write_preview_bitmap(hwnd, &root.join("advanced-small.bmp"));
                println!("normal+advanced wheel: layout rebuilds=0, font creations=0, text measurements=0; top+bottom boundary redraws=0; roundtrip/full client RGB identical");
            }));
            unsafe { PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap(); }
        }).unwrap();
        probe.unwrap();
        std::fs::remove_file(store.path()).unwrap();
    }
    fn client_rgb(hwnd: HWND) -> Vec<u8> {
        unsafe {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect).unwrap();
            let source = GetDC(hwnd);
            let dc = CreateCompatibleDC(source);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: rect.right,
                    biHeight: -rect.bottom,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bitmap);
            BitBlt(dc, 0, 0, rect.right, rect.bottom, source, 0, 0, SRCCOPY).unwrap();
            let _ = GdiFlush();
            let bytes = std::slice::from_raw_parts(
                bits.cast::<u8>(),
                (rect.right * rect.bottom * 4) as usize,
            );
            let rgb = bytes
                .chunks_exact(4)
                .flat_map(|p| p[..3].iter().copied())
                .collect();
            SelectObject(dc, old);
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(dc);
            ReleaseDC(hwnd, source);
            rgb
        }
    }
    #[test]
    #[ignore = "requires owned FLOWER_SETTINGS_PREVIEW_DIR"]
    fn model_api_isolated_scope_and_merge() {
        let root = owned_root("model-scope");
        let store = SettingsStore::new(root.join("settings.json"));
        let initial = crate::settings::Settings {
            online_polish: crate::settings::OnlinePolishSettings {
                enabled: true,
                ..Default::default()
            },
            writing_send_consent: Some(crate::writing::consent::SCOPE.into()),
            ..Default::default()
        };
        store.save(&initial).unwrap();
        let mut models = initial.model_api.clone();
        models.profiles[0].name = "rename only".into();
        config::persist(&store, &models).unwrap();
        assert!(store.load().unwrap().online_polish.enabled);
        models.profiles[0].model = "changed-scope".into();
        config::persist(&store, &models).unwrap();
        let actual = store.load().unwrap();
        assert!(actual.online_polish.enabled);
        assert!(actual.writing_send_consent.is_none());
        assert_eq!(actual.model_api, models);
        std::fs::remove_file(store.path()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    fn assert_focus_visible(hwnd: HWND) {
        unsafe {
            let mut r = RECT::default();
            let mut c = RECT::default();
            GetWindowRect(GetFocus(), &mut r).unwrap();
            GetClientRect(hwnd, &mut c).unwrap();
            let mut p = POINT {
                x: r.left,
                y: r.top,
            };
            let _ = ScreenToClient(hwnd, &mut p);
            assert!(
                p.y >= 0 && p.y + r.bottom - r.top <= c.bottom,
                "focused control clipped"
            );
        }
    }
    #[test]
    #[ignore = "owned native modal fixture; requires FLOWER_SETTINGS_PREVIEW_DIR"]
    fn model_api_owned_small_keyboard_and_host_quit() {
        let root = owned_root("model-modal");
        let store = SettingsStore::new(root.join("settings.json"));
        store.save(&Default::default()).unwrap();
        unsafe {
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("owned model fixture"),
                WS_OVERLAPPED,
                0,
                0,
                300,
                200,
                HWND::default(),
                HMENU::default(),
                HINSTANCE::from(GetModuleHandleW(PCWSTR::null()).unwrap()),
                None,
            )
            .unwrap();
            let mut probe = Ok(());
            let result = show_model_api_settings_impl(owner, store.clone(), |hwnd| {
                probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert!(!IsWindowEnabled(owner).as_bool());
                    SetWindowPos(
                        hwnd,
                        HWND::default(),
                        0,
                        0,
                        390,
                        260,
                        SWP_NOMOVE | SWP_NOZORDER,
                    )
                    .unwrap();
                    let mut visited = std::collections::HashSet::new();
                    for _ in 0..32 {
                        let m = MSG {
                            hwnd: GetFocus(),
                            message: WM_KEYDOWN,
                            wParam: WPARAM(
                                windows::Win32::UI::Input::KeyboardAndMouse::VK_TAB.0 as usize,
                            ),
                            ..Default::default()
                        };
                        assert!(IsDialogMessageW(hwnd, &m).as_bool());
                        ensure_focus_visible(hwnd);
                        assert_focus_visible(hwnd);
                        visited.insert(GetDlgCtrlID(GetFocus()));
                    }
                    for id in [
                        PROFILE, NEW, DELETE, NAME, URL, MODEL, ADVANCED, SAVE, KEY, FETCH, CLOSE,
                    ] {
                        assert!(visited.contains(&id), "missing control {id}");
                    }
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                    command(hwnd, ptr, ADVANCED).unwrap();
                    for id in [PROTOCOL, MANUAL, DELETE_KEY] {
                        assert!(IsWindowVisible(GetDlgItem(hwnd, id).unwrap()).as_bool());
                        let _ = SetFocus(GetDlgItem(hwnd, id).unwrap());
                        ensure_focus_visible(hwnd);
                        assert_focus_visible(hwnd);
                    }
                    for key in [VK_RETURN, VK_ESCAPE] {
                        let combo = GetDlgItem(hwnd, MODEL).unwrap();
                        SendMessageW(combo, CB_SHOWDROPDOWN, WPARAM(1), LPARAM(0));
                        let m = MSG {
                            hwnd: combo,
                            message: WM_KEYDOWN,
                            wParam: WPARAM(key.0 as usize),
                            ..Default::default()
                        };
                        assert!(preprocess_key(hwnd, &m));
                        assert!(
                            IsWindow(hwnd).as_bool(),
                            "dropdown Esc must not close modal"
                        );
                    }
                    for _ in 0..16 {
                        SendMessageW(hwnd, WM_NEXTDLGCTL, WPARAM(1), LPARAM(0));
                        ensure_focus_visible(hwnd);
                        assert_focus_visible(hwnd);
                    }
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                    assert!(credential_profile(
                        &store.load().unwrap().model_api,
                        &edit_profile(hwnd, &(*ptr).selected).unwrap()
                    )
                    .is_ok());
                    let s = wide("https://different.example/v1");
                    SetWindowTextW(GetDlgItem(hwnd, URL).unwrap(), PCWSTR(s.as_ptr())).unwrap();
                    assert!(credential_profile(
                        &store.load().unwrap().model_api,
                        &edit_profile(hwnd, &(*ptr).selected).unwrap()
                    )
                    .is_err());
                    for id in [SAVE, FETCH, DELETE_KEY] {
                        let msg = MSG {
                            hwnd: GetDlgItem(hwnd, id).unwrap(),
                            message: WM_KEYDOWN,
                            wParam: WPARAM(VK_RETURN.0 as usize),
                            ..Default::default()
                        };
                        assert!(preprocess_key(hwnd, &msg));
                    }
                }));
                PostMessageW(hwnd, WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0)).unwrap();
            });
            assert!(IsWindowEnabled(owner).as_bool());
            assert!(result.is_ok());
            probe.unwrap();
            let quit = show_model_api_settings_impl(owner, store.clone(), |_| PostQuitMessage(73));
            assert!(quit.is_err());
            assert!(IsWindowEnabled(owner).as_bool());
            let mut msg = MSG::default();
            assert!(PeekMessageW(&mut msg, HWND::default(), WM_QUIT, WM_QUIT, PM_REMOVE).as_bool());
            assert_eq!(msg.wParam.0, 73);
            DestroyWindow(owner).unwrap();
        }
        assert_eq!(store.load().unwrap(), crate::settings::Settings::default());
        std::fs::remove_file(store.path()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    #[ignore = "explicit owned model manager screenshots; FLOWER_SETTINGS_PREVIEW_DIR required"]
    fn model_api_owned_manager_preview() {
        let root = owned_root("model-manager-preview");
        let store = SettingsStore::new(root.join("settings.json"));
        let profile = ApiProfile {
            id: "owned-responses-preview".into(),
            name: "自定义连接示例".into(),
            protocol: ApiProtocol::Responses,
            base_url: "https://api.example.com/v1".into(),
            model: "example-text-model".into(),
        };
        let initial = crate::settings::Settings {
            model_api: ModelApiSettings {
                profiles: vec![profile.clone()],
                active_id: Some(profile.id.clone()),
            },
            ..Default::default()
        };
        store.save(&initial).unwrap();
        // The owned worker creates, runs and destroys its own window. Join before
        // returning; this is not the real GUI entry or a detached UI thread.
        let worker = std::thread::spawn(move || {
            let mut probe = Ok(());
            let mut captured = HWND::default();
            let result = show_model_api_settings_impl(HWND::default(), store.clone(), |hwnd| {
                captured = hwnd;
                probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                    SetWindowPos(
                        hwnd,
                        HWND::default(),
                        0,
                        0,
                        740,
                        940,
                        SWP_NOMOVE | SWP_NOZORDER,
                    )
                    .unwrap();
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                    let fake = Arc::new(Fake::new());
                    *fake.settings.lock().unwrap() = store.load().unwrap().model_api;
                    fake.keys
                        .lock()
                        .unwrap()
                        .push((*ptr).selected.credential_target().unwrap());
                    (*ptr).services = fake;
                    start_catalog(hwnd, &mut *ptr).unwrap();
                    while !(*ptr)
                        .catalog
                        .as_ref()
                        .unwrap()
                        .thread
                        .as_ref()
                        .unwrap()
                        .is_finished()
                    {
                        std::thread::yield_now();
                    }
                    poll_catalog(hwnd, &mut *ptr);
                    SendMessageW(
                        GetDlgItem(hwnd, MODEL).unwrap(),
                        CB_SETCURSEL,
                        WPARAM(0),
                        LPARAM(0),
                    );
                    label(hwnd, MANUAL, "");
                    assert!(text(hwnd, KEY).is_empty());
                    (*ptr).scroll = 0;
                    layout(hwnd, &mut *ptr);
                    let _ = UpdateWindow(hwnd);
                    write_preview_bitmap(hwnd, &root.join("model-manager-connection.bmp"));
                    command(hwnd, ptr, ADVANCED).unwrap();
                    SetWindowPos(
                        hwnd,
                        HWND::default(),
                        0,
                        0,
                        480,
                        430,
                        SWP_NOMOVE | SWP_NOZORDER,
                    )
                    .unwrap();
                    (*ptr).scroll = i32::MAX / 2;
                    layout(hwnd, &mut *ptr);
                    let _ = SetFocus(GetDlgItem(hwnd, MANUAL).unwrap());
                    ensure_focus_visible(hwnd);
                    let _ = UpdateWindow(hwnd);
                    write_preview_bitmap(hwnd, &root.join("model-manager-key-and-notice.bmp"));
                }));
                // Always close, including after a screenshot assertion failure.
                unsafe {
                    PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap();
                }
            });
            assert!(!unsafe { IsWindow(captured) }.as_bool());
            result.unwrap();
            probe.unwrap();
            assert_eq!(store.load().unwrap(), initial);
            std::fs::remove_file(store.path()).unwrap();
        });
        worker.join().unwrap();
    }
    fn write_preview_bitmap(hwnd: HWND, path: &std::path::Path) {
        use windows::Win32::Graphics::Gdi::*;
        #[link(name = "user32")]
        extern "system" {
            fn PrintWindow(
                hwnd: *mut std::ffi::c_void,
                dc: *mut std::ffi::c_void,
                flags: u32,
            ) -> i32;
        }
        unsafe {
            let mut rect = RECT::default();
            windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect).unwrap();
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            let dc = CreateCompatibleDC(HDC::default());
            assert!(!dc.is_invalid());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, HGDIOBJ::from(bitmap));
            let rendered = PrintWindow(hwnd.0, dc.0, 1); // PW_CLIENTONLY
            let byte_count = (width * height * 4) as usize;
            let mut bytes = Vec::with_capacity(54 + byte_count);
            bytes.extend_from_slice(b"BM");
            bytes.extend_from_slice(&((54 + byte_count) as u32).to_le_bytes());
            bytes.extend_from_slice(&[0; 4]);
            bytes.extend_from_slice(&54u32.to_le_bytes());
            bytes.extend_from_slice(std::slice::from_raw_parts(
                (&info.bmiHeader as *const BITMAPINFOHEADER).cast::<u8>(),
                40,
            ));
            bytes.extend_from_slice(std::slice::from_raw_parts(bits.cast::<u8>(), byte_count));
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ::from(bitmap));
            let _ = DeleteDC(dc);
            assert_ne!(rendered, 0, "PrintWindow failed");
            std::fs::write(path, bytes).unwrap();
        }
    }
}
