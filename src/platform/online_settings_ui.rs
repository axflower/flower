//! Explicit, local-only configuration. Opening never reads Credential Manager.
use crate::settings::{OnlinePolishSettings, SettingsStore};
use crate::writing::{
    config::{self, ApiProfile},
    PolishStyle,
};
use crate::ChineseScript;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, IsWindowEnabled, SetActiveWindow, SetFocus, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("Flower.OnlineSettings");
const ENABLE: i32 = 101;
const STYLE: i32 = 102;
const SCRIPT: i32 = 103;
const KEY: i32 = 104;
const SAVE_KEY: i32 = 105;
const SAVE: i32 = 108;
const CLOSE: i32 = 109;

#[cfg(test)]
thread_local! {
    // Only explicitly opted-in owned-window fixtures suppress their own success box.
    static SUPPRESS_NOTICES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct State {
    store: SettingsStore,
    saved: Option<OnlinePolishSettings>,
    profile: ApiProfile,
    controls: Vec<(HWND, [i32; 4])>,
    font: HFONT,
    scroll: i32,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
#[cfg(test)]
fn persist(store: &SettingsStore, value: OnlinePolishSettings) -> Result<(), String> {
    let profile = config::load_active(Some(store)).map_err(|_| "请先选择模型 API 连接。")?;
    persist_for(store, &profile, value)
}
fn persist_for(
    store: &SettingsStore,
    profile: &ApiProfile,
    value: OnlinePolishSettings,
) -> Result<(), String> {
    if !config::is_current(Some(store), profile) {
        return Err("当前连接已变化；请关闭并重新打开在线设置。".into());
    }
    let mut latest = store.load().map_err(|_| "读取设置失败；未保存在线参数。")?;
    if latest.model_api.active().ok().and_then(|p| p.scope().ok()) != profile.scope().ok() {
        return Err("当前连接已变化；请关闭并重新打开在线设置。".into());
    }
    latest.online_polish = value;
    store
        .save(&latest)
        .map_err(|_| "保存设置失败；请检查配置目录权限。".into())
}
fn notice(hwnd: HWND, text: &str) {
    #[cfg(test)]
    if SUPPRESS_NOTICES.with(|value| value.get()) {
        return;
    }
    let text = wide(text);
    unsafe {
        MessageBoxW(hwnd, PCWSTR(text.as_ptr()), w!("在线 AI 设置"), MB_OK);
    }
}

/// The caller supplies its existing store; no fallback to real user storage.
pub fn show_online_settings(
    owner: HWND,
    store: SettingsStore,
) -> Result<Option<OnlinePolishSettings>, String> {
    show_online_settings_impl(owner, store, |_| {})
}

fn show_online_settings_impl(
    owner: HWND,
    store: SettingsStore,
    after_show: impl FnOnce(HWND),
) -> Result<Option<OnlinePolishSettings>, String> {
    let loaded = store.load().map_err(|_| "读取设置失败；未打开在线设置。")?;
    let profile = loaded
        .model_api
        .active()
        .map_err(|_| "请先在模型 API 中明确选择当前连接。")?;
    let initial = loaded.online_polish;
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
            saved: None,
            profile,
            controls: Vec::new(),
            font: HFONT::default(),
            scroll: 0,
        });
        let created = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS,
            w!("在线 AI 设置"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_VSCROLL,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            620,
            560,
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
        let mut work = RECT::default();
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut work as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            let _ = SetWindowPos(
                hwnd,
                HWND::default(),
                work.left + 8,
                work.top + 8,
                620.min(work.right - work.left - 16),
                560.min(work.bottom - work.top - 16),
                SWP_NOZORDER,
            );
        }
        let restore_owner = !owner.is_invalid() && IsWindowEnabled(owner).as_bool();
        if restore_owner {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetFocus(GetDlgItem(hwnd, ENABLE).unwrap_or_default());
        after_show(hwnd);
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
            if preprocess_key(hwnd, &message) {
                continue;
            }
            if !IsDialogMessageW(hwnd, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            ensure_focus_visible(hwnd);
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
            None => Ok(state.saved.take()),
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
            (*ptr).scroll = (*ptr).scroll.saturating_add(delta);
            layout(hwnd, &mut *ptr);
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
        if message.wParam.0 == VK_ESCAPE.0 as usize {
            let _ = DestroyWindow(hwnd);
            return true;
        }
        // Enter never grants consent or performs a credential operation.
        message.wParam.0 == VK_RETURN.0 as usize
    }
}

fn read_parameters(hwnd: HWND) -> Result<OnlinePolishSettings, String> {
    let control = |id| unsafe { GetDlgItem(hwnd, id).unwrap_or_default() };
    let selected = |id| unsafe { SendMessageW(control(id), CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 };
    Ok(OnlinePolishSettings {
        enabled: unsafe { SendMessageW(control(ENABLE), BM_GETCHECK, WPARAM(0), LPARAM(0)).0 } == 1,
        style: match selected(STYLE) {
            0 => PolishStyle::Fluent,
            1 => PolishStyle::Formal,
            2 => PolishStyle::Concise,
            _ => return Err("请选择润色风格。".into()),
        },
        script: match selected(SCRIPT) {
            0 => None,
            1 => Some(ChineseScript::Simplified),
            2 => Some(ChineseScript::Traditional),
            _ => return Err("请选择输出字形。".into()),
        },
    })
}

fn initialize(
    hwnd: HWND,
    state: &mut State,
    initial: OnlinePolishSettings,
    instance: HINSTANCE,
) -> Result<(), String> {
    let specs = [
        (200, "STATIC", "在线润色 · 当前模型 API", 0, [24, 16, 550, 62]),
        (201, "STATIC", "润色快捷键（默认 Ctrl+Space）会将主动选中的文本发送给当前模型 API 处理，适用供应商隐私与服务条款，可能计入 API 额度或产生费用。不会自动监听或上传选区。", 0, [24, 84, 550, 60]),
        (ENABLE, "BUTTON", "允许按润色快捷键将所选文本发送至上方显示的当前连接", (BS_AUTOCHECKBOX | BS_MULTILINE) as u32, [24, 152, 550, 52]),
        (202, "STATIC", "润色风格", 0, [24, 220, 160, 24]),
        (STYLE, "COMBOBOX", "", CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0, [24, 248, 240, 120]),
        (203, "STATIC", "输出字形", 0, [304, 220, 240, 24]),
        (SCRIPT, "COMBOBOX", "", CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0, [304, 248, 270, 120]),
        (SAVE_KEY, "BUTTON", "管理模型 API…", 0, [24, 306, 240, 36]),
        (205, "STATIC", "保存密钥或参数不会发送网络请求。\n保存后重新切换到Flower生效。", 0, [24, 354, 550, 48]),
        (SAVE, "BUTTON", "保存参数", 0, [304, 420, 130, 36]),
        (CLOSE, "BUTTON", "关闭", 0, [446, 420, 128, 36]),
    ];
    for (id, class, text, extra, rect) in specs {
        let extra = extra
            | if class == "BUTTON" {
                BS_MULTILINE as u32
            } else {
                0
            };
        let class = wide(class);
        let text = wide(text);
        let child = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(
                    WS_CHILD.0 | WS_VISIBLE.0 | if id < 200 { WS_TABSTOP.0 } else { 0 } | extra,
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
        .map_err(|_| "无法创建在线设置控件。")?;
        state.controls.push((child, rect));
    }
    unsafe {
        SendMessageW(
            GetDlgItem(hwnd, ENABLE).unwrap_or_default(),
            BM_SETCHECK,
            WPARAM(initial.enabled as usize),
            LPARAM(0),
        );
        for (id, labels, selected) in [
            (
                STYLE,
                ["通顺", "正式", "简洁"],
                match initial.style {
                    PolishStyle::Fluent => 0,
                    PolishStyle::Formal => 1,
                    PolishStyle::Concise => 2,
                },
            ),
            (
                SCRIPT,
                ["跟随当前 Flower", "简体", "通用繁体"],
                match initial.script {
                    None => 0,
                    Some(ChineseScript::Simplified) => 1,
                    Some(ChineseScript::Traditional) => 2,
                },
            ),
        ] {
            let control = GetDlgItem(hwnd, id).unwrap_or_default();
            for label in labels {
                let text = wide(label);
                SendMessageW(
                    control,
                    CB_ADDSTRING,
                    WPARAM(0),
                    LPARAM(text.as_ptr() as isize),
                );
            }
            SendMessageW(control, CB_SETCURSEL, WPARAM(selected), LPARAM(0));
        }
    }
    let target = state
        .profile
        .display_target()
        .map_err(|_| "连接配置无效。")?;
    let target = wide(&target);
    unsafe {
        let _ = SetWindowTextW(
            GetDlgItem(hwnd, 200).unwrap_or_default(),
            PCWSTR(target.as_ptr()),
        );
    }
    layout(hwnd, state);
    Ok(())
}

fn layout(hwnd: HWND, state: &mut State) {
    layout_at_dpi(hwnd, state, unsafe { GetDpiForWindow(hwnd) }.max(96));
}

fn layout_at_dpi(hwnd: HWND, state: &mut State, dpi: u32) {
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
            if id >= 200 || !matches!(id, STYLE | SCRIPT | KEY) {
                let mut text = vec![0u16; GetWindowTextLengthW(*control).max(0) as usize + 1];
                let count = GetWindowTextW(*control, &mut text) as usize;
                let inset = if id == ENABLE {
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
                height = height.max(measured.bottom + scale(4));
                row_extra = row_extra.max(height - scale(*h));
            }
            rows.push((*control, [x * width / 598, scale(*y) + shift, cw, height]));
        }
        if !old_font.is_invalid() {
            SelectObject(dc, old_font);
        }
        ReleaseDC(hwnd, dc);
        let content_height = scale(480) + shift + row_extra;
        let max = (content_height - client.bottom).max(0);
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
        for (control, [x, y, w, h]) in rows {
            let _ = MoveWindow(control, x, y - state.scroll, w, h, true);
            if !font.is_invalid() {
                SendMessageW(control, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            }
        }
        if !font.is_invalid() {
            let old = std::mem::replace(&mut state.font, font);
            if !old.is_invalid() {
                let _ = DeleteObject(old);
            }
        }
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
                -((wp.0 >> 16) as u16 as i16 as i32) / 3
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
                (*ptr).scroll += delta;
                layout(hwnd, &mut *ptr);
            }
            return LRESULT(0);
        }
        WM_COMMAND if !ptr.is_null() && (wp.0 >> 16) == BN_CLICKED as usize => {
            let id = (wp.0 & 0xffff) as i32;
            match id {
                CLOSE => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                SAVE_KEY => {
                    let store = unsafe { (*ptr).store.clone() };
                    // No state borrow is held across the nested native message pump.
                    if let Err(error) = super::model_api_ui::show_model_api_settings(hwnd, store) {
                        notice(hwnd, &error);
                    }
                    notice(
                        hwnd,
                        "模型管理已关闭。为避免授权旧目标，请关闭并重新打开在线设置。",
                    );
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                SAVE => {
                    let value = match read_parameters(hwnd) {
                        Ok(value) => value,
                        Err(error) => {
                            notice(hwnd, &error);
                            return LRESULT(0);
                        }
                    };
                    let result = persist_for(
                        unsafe { &(*ptr).store },
                        unsafe { &(*ptr).profile },
                        value.clone(),
                    );
                    match result {
                        Ok(()) => {
                            unsafe {
                                (*ptr).saved = Some(value);
                            }
                            notice(
                                hwnd,
                                "参数已保存，未发送网络请求。\n保存后重新切换到Flower生效。",
                            );
                            let _ = unsafe { DestroyWindow(hwnd) };
                        }
                        Err(error) => notice(hwnd, &error),
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assert_visible(parent: HWND, child: HWND) {
        unsafe {
            let mut rect = RECT::default();
            let mut client = RECT::default();
            GetWindowRect(child, &mut rect).unwrap();
            GetClientRect(parent, &mut client).unwrap();
            let mut origin = POINT {
                x: rect.left,
                y: rect.top,
            };
            let _ = ScreenToClient(parent, &mut origin);
            assert!(
                origin.y >= 0,
                "focused control above viewport: {}",
                origin.y
            );
            assert!(
                origin.y + rect.bottom - rect.top <= client.bottom,
                "focused control below viewport"
            );
        }
    }

    #[test]
    #[ignore = "requires FLOWER_SETTINGS_PREVIEW_DIR pointing to owned session artifacts"]
    fn online_owned_modal_keyboard_scroll_dpi_and_store_fixture() {
        let root = std::path::PathBuf::from(
            std::env::var_os("FLOWER_SETTINGS_PREVIEW_DIR").expect("owned directory required"),
        )
        .join("online-modal");
        std::fs::create_dir(&root).unwrap();
        let store = SettingsStore::new(root.join("settings.json"));
        let mut expected = crate::settings::Settings {
            candidate_page_size: 23,
            ai_correction_enabled: false,
            ..Default::default()
        };
        store.save(&expected).unwrap();
        unsafe {
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Flower owned fixture"),
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
            let mut probe_result = Ok(());
            let result = show_online_settings_impl(owner, store.clone(), |hwnd| {
                probe_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert!(
                        !IsWindowEnabled(owner).as_bool(),
                        "owner disabled during modal"
                    );
                    assert_eq!(
                        read_parameters(hwnd).unwrap(),
                        OnlinePolishSettings::default()
                    );
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
                    // Explicitly exercise actual short-window native dialog traversal.
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
                    for _ in 0..12 {
                        let message = MSG {
                            hwnd: GetFocus(),
                            message: WM_KEYDOWN,
                            wParam: WPARAM(
                                windows::Win32::UI::Input::KeyboardAndMouse::VK_TAB.0 as usize,
                            ),
                            ..Default::default()
                        };
                        assert!(IsDialogMessageW(hwnd, &message).as_bool());
                        ensure_focus_visible(hwnd);
                        assert_visible(hwnd, GetFocus());
                    }
                    for _ in 0..12 {
                        // WM_NEXTDLGCTL(previous=true) is native Shift+Tab navigation,
                        // without modifying global keyboard state.
                        SendMessageW(hwnd, WM_NEXTDLGCTL, WPARAM(1), LPARAM(0));
                        ensure_focus_visible(hwnd);
                        assert_visible(hwnd, GetFocus());
                    }
                    // Render geometry uses real GDI font measurements at 100/150/200%.
                    for dpi in [96, 144, 192] {
                        layout_at_dpi(hwnd, &mut *ptr, dpi);
                        let dc = GetDC(hwnd);
                        let old = SelectObject(dc, (*ptr).font);
                        for (child, _) in &(*ptr).controls {
                            let id = GetDlgCtrlID(*child);
                            if id < 200 && id != ENABLE {
                                continue;
                            }
                            let mut rect = RECT::default();
                            GetClientRect(*child, &mut rect).unwrap();
                            let height = rect.bottom;
                            if id == ENABLE {
                                rect.right -= 24 * dpi as i32 / 96;
                            }
                            let mut text = vec![0u16; GetWindowTextLengthW(*child) as usize + 1];
                            let count = GetWindowTextW(*child, &mut text) as usize;
                            DrawTextW(
                                dc,
                                &mut text[..count],
                                &mut rect,
                                DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                            );
                            assert!(rect.bottom <= height, "text clipped: id={id}, dpi={dpi}");
                        }
                        SelectObject(dc, old);
                        ReleaseDC(hwnd, dc);
                    }
                    layout(hwnd, &mut *ptr);
                    // Same read/persist path used by explicit Save: latest non-online
                    // values must survive even if they changed after the modal opened.
                    let mut latest = store.load().unwrap();
                    latest.theme = crate::settings::Theme::Dark;
                    store.save(&latest).unwrap();
                    SendMessageW(
                        GetDlgItem(hwnd, ENABLE).unwrap(),
                        BM_SETCHECK,
                        WPARAM(1),
                        LPARAM(0),
                    );
                    SendMessageW(
                        GetDlgItem(hwnd, STYLE).unwrap(),
                        CB_SETCURSEL,
                        WPARAM(2),
                        LPARAM(0),
                    );
                    SendMessageW(
                        GetDlgItem(hwnd, SCRIPT).unwrap(),
                        CB_SETCURSEL,
                        WPARAM(2),
                        LPARAM(0),
                    );
                    let parameters = read_parameters(hwnd).unwrap();
                    assert!(parameters.enabled);
                    assert_eq!(parameters.style, PolishStyle::Concise);
                    persist(&store, parameters.clone()).unwrap();
                    latest.online_polish = parameters;
                    assert_eq!(store.load().unwrap(), latest);
                    expected = latest;
                    // Queue real modal-loop Enter messages. None may grant consent,
                    // save credentials, delete credentials, or return Some(saved).
                    SendMessageW(
                        GetDlgItem(hwnd, ENABLE).unwrap(),
                        BM_SETCHECK,
                        WPARAM(0),
                        LPARAM(0),
                    );
                    for id in [ENABLE, SAVE, SAVE_KEY] {
                        PostMessageW(
                            GetDlgItem(hwnd, id).unwrap(),
                            WM_KEYDOWN,
                            WPARAM(VK_RETURN.0 as usize),
                            LPARAM(0),
                        )
                        .unwrap();
                    }
                }));
                // Always terminate this owned modal, including assertion failure.
                PostMessageW(hwnd, WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0)).unwrap();
            });
            let restored = IsWindowEnabled(owner).as_bool();
            DestroyWindow(owner).unwrap();
            probe_result.unwrap();
            assert!(restored, "owner restored after Esc");
            assert_eq!(result.unwrap(), None, "Enter must not save parameters");
            assert_eq!(
                store.load().unwrap(),
                expected,
                "Enter must not overwrite settings"
            );
        }
        // Exercise the actual native Save command and returned saved value.
        // Suppress only this owned fixture's informational success message.
        SUPPRESS_NOTICES.with(|value| value.set(true));
        let save_result =
            show_online_settings_impl(HWND::default(), store.clone(), |hwnd| unsafe {
                SendMessageW(
                    GetDlgItem(hwnd, ENABLE).unwrap(),
                    BM_SETCHECK,
                    WPARAM(0),
                    LPARAM(0),
                );
                SendMessageW(
                    GetDlgItem(hwnd, STYLE).unwrap(),
                    CB_SETCURSEL,
                    WPARAM(1),
                    LPARAM(0),
                );
                PostMessageW(
                    hwnd,
                    WM_COMMAND,
                    WPARAM(SAVE as usize),
                    LPARAM(GetDlgItem(hwnd, SAVE).unwrap().0 as isize),
                )
                .unwrap();
            });
        SUPPRESS_NOTICES.with(|value| value.set(false));
        let saved = save_result
            .unwrap()
            .expect("actual Save command returns parameters");
        assert!(!saved.enabled);
        assert_eq!(saved.style, PolishStyle::Formal);
        expected.online_polish = saved;
        assert_eq!(store.load().unwrap(), expected);
        std::fs::remove_file(store.path()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    #[ignore = "requires FLOWER_SETTINGS_PREVIEW_DIR pointing to owned session artifacts"]
    fn online_parameters_preserve_latest_other_settings_and_reject_bad_store() {
        let root = std::path::PathBuf::from(
            std::env::var_os("FLOWER_SETTINGS_PREVIEW_DIR")
                .expect("owned session directory required"),
        )
        .join("online-settings-store");
        std::fs::create_dir(&root).unwrap();
        let store = SettingsStore::new(root.join("isolated.json"));
        let mut latest = crate::settings::Settings {
            candidate_page_size: 17,
            ai_correction_enabled: false,
            writing_shortcuts: crate::settings::WritingShortcuts {
                draft: crate::settings::WritingShortcut::new(0x41, 5),
                polish: crate::settings::WritingShortcut::new(0x7b, 5),
            },
            ..Default::default()
        };
        store.save(&latest).unwrap();
        let value = OnlinePolishSettings {
            enabled: true,
            style: PolishStyle::Formal,
            script: Some(ChineseScript::Traditional),
        };
        persist(&store, value.clone()).unwrap();
        latest.online_polish = value;
        assert_eq!(store.load().unwrap(), latest);
        let snapshot = config::load_active(Some(&store)).unwrap();
        let mut changed = latest.model_api.clone();
        changed.profiles[0].model = "owned-new-model".into();
        config::persist(&store, &changed).unwrap();
        let before = store.load().unwrap();
        assert!(
            before.online_polish.enabled,
            "changing target preserves the explicit enabled value"
        );
        assert!(persist_for(
            &store,
            &snapshot,
            OnlinePolishSettings {
                enabled: true,
                ..Default::default()
            }
        )
        .is_err());
        assert_eq!(
            store.load().unwrap(),
            before,
            "old displayed target cannot grant consent to a new target"
        );
        std::fs::write(store.path(), b"not valid json").unwrap();
        assert!(persist(&store, OnlinePolishSettings::default()).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), b"not valid json");
        std::fs::remove_file(store.path()).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
