//! 纯 Rust/Win32 的 Flower 设置窗口。
//!
//! 此模块只由独立的 `flower-settings` 可执行文件使用。窗口拥有自己的
//! 消息循环与初始词典读取线程，不影响 DLL 中 TSF 服务的宿主消息循环。

use std::cell::RefCell;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::JoinHandle;

use windows::core::{Error as WindowsError, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, GetSysColorBrush, SetBkMode, SetTextColor, UpdateWindow,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_BTNFACE, DEFAULT_CHARSET, DEFAULT_PITCH,
    FF_DONTCARE, FW_NORMAL, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetDlgCtrlID, GetMessageW,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, IsWindow,
    LoadCursorW, MessageBoxW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW,
    SendMessageW, SetDlgItemTextW, SetWindowLongPtrW, ShowWindow, TranslateMessage,
    UnregisterClassW, BN_CLICKED, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_ERR, CB_GETDROPPEDSTATE,
    CB_SETCURSEL, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_NUMBER,
    GWLP_USERDATA, HMENU, IDCANCEL, IDC_ARROW, IDOK, IDYES, MB_ICONERROR, MB_OK, MB_YESNO, MSG,
    SW_SHOWNORMAL, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED,
    WM_KEYDOWN, WM_NCCREATE, WM_NCDESTROY, WM_SYSCOLORCHANGE, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};

use crate::settings::{
    parse_candidate_page_size_input, Settings, SettingsStore, Theme, WritingShortcut,
    WritingShortcuts, MAX_CANDIDATE_PAGE_SIZE, MIN_CANDIDATE_PAGE_SIZE,
};
use crate::user_dictionary::{DictionaryStore, UserDictionary};

const DICTIONARY_TIMER: usize = 0x4644;
type DictionaryResult = Result<UserDictionary, String>;

struct InitialDictionaryLoad {
    cancelled: Arc<AtomicBool>,
    result: mpsc::Receiver<DictionaryResult>,
    worker: Option<JoinHandle<()>>,
}

impl InitialDictionaryLoad {
    fn start(load: impl FnOnce() -> DictionaryResult + Send + 'static) -> Result<Self, String> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = cancelled.clone();
        let (sender, result) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("settings-dictionary".into())
            .spawn(move || {
                if token.load(Ordering::Acquire) {
                    return;
                }
                let result = catch_unwind(AssertUnwindSafe(load))
                    .unwrap_or_else(|_| Err("词典读取线程失败".into()));
                if !token.load(Ordering::Acquire) {
                    let _ = sender.send(result);
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            cancelled,
            result,
            worker: Some(worker),
        })
    }
}

impl Drop for InitialDictionaryLoad {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // Store reads cannot be interrupted mid-I/O. Join before releasing the
        // window-owned state; no worker ever holds an HWND or UI pointer.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

const WINDOW_CLASS_NAME: PCWSTR = windows::core::w!("Flower.SettingsWindow");
const FONT_FACE: PCWSTR = windows::core::w!("Microsoft YaHei UI");
const BASE_DPI: u32 = 96;
const CLIENT_WIDTH: i32 = 760;
const CLIENT_HEIGHT: i32 = 480;
const BACKGROUND: COLORREF = COLORREF(0x00f8f7f4);
const WHITE: COLORREF = COLORREF(0x00ffffff);
const TEXT: COLORREF = COLORREF(0x003a2b17);
const MUTED: COLORREF = COLORREF(0x00756652);
const ACCENT: COLORREF = COLORREF(0x00696b0f);
const BORDER: COLORREF = COLORREF(0x00e9e5dc);
const DANGER: COLORREF = COLORREF(0x003636a6);
const CARDS: [[i32; 4]; 1] = [[28, 72, 704, 304]];
const CATEGORY_ID: i32 = 400;
const DRAFT_SHORTCUT_ID: i32 = 410;
const RESET_SHORTCUTS_ID: i32 = 412;

fn select_shortcuts(c: SettingsControls, shortcuts: WritingShortcuts) {
    for (control, value) in c
        .shortcuts
        .into_iter()
        .zip([shortcuts.draft, shortcuts.polish])
    {
        let text = wide_null(&value.label());
        unsafe {
            SetWindowLongPtrW(
                control,
                GWLP_USERDATA,
                value.key as isize | ((value.modifiers as isize) << 16),
            );
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                control,
                PCWSTR(text.as_ptr()),
            );
        }
    }
}

fn category_controls(c: SettingsControls) -> [Vec<HWND>; 4] {
    [
        vec![
            c.headings[2],
            c.labels[0],
            c.labels[1],
            c.labels[2],
            c.labels[3],
            c.page_edit,
            c.theme_combo,
            c.script_combo,
        ],
        vec![
            c.headings[3],
            c.online_ai,
            c.online_ai_hint,
            c.online_settings,
            c.model_api_settings,
            c.ai_correction,
            c.ai_scope_compatibility,
            c.ai_correction_hint,
            c.restore_send_confirmation,
        ],
        vec![
            c.shortcut_labels[0],
            c.shortcut_labels[1],
            c.shortcuts[0],
            c.shortcuts[1],
            c.reset_shortcuts,
            c.shortcut_hint,
        ],
        vec![
            c.headings[4],
            c.headings[5],
            c.learning,
            c.learning_hint,
            c.clear_learning,
            c.domain_combo,
            c.domain_enabled,
            c.dictionary_status,
        ],
    ]
}

fn show_category(state: &RefCell<SettingsUiState>, category: usize) {
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOW};
    state.borrow_mut().controls.category = category;
    let c = state.borrow().controls;
    for (index, controls) in category_controls(c).into_iter().enumerate() {
        for control in controls {
            unsafe {
                let _ = ShowWindow(control, if index == category { SW_SHOW } else { SW_HIDE });
            }
        }
    }
    // Selection is painted separately; keep accessible captions stable.
    for (index, caption) in ["常规", "AI功能", "快捷键", "词库与学习"]
        .into_iter()
        .enumerate()
    {
        let text = wide_null(caption);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                c.categories[index],
                PCWSTR(text.as_ptr()),
            );
        }
    }
    unsafe {
        let parent = windows::Win32::UI::WindowsAndMessaging::GetParent(c.categories[0]);
        if let Ok(parent) = parent {
            let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
                parent,
                None,
                None,
                windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                    | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN
                    | windows::Win32::Graphics::Gdi::RDW_UPDATENOW,
            );
        }
    }
}

fn paint_settings(hwnd: HWND) {
    use windows::Win32::Graphics::Gdi::*;
    // All selected objects are stock objects; restore the DC before EndPaint.
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        paint_canvas(hwnd, dc, &paint.rcPaint);
        let _ = EndPaint(hwnd, &paint);
    }
}

fn paint_canvas(hwnd: HWND, dc: HDC, rect: &RECT) {
    use windows::Win32::Graphics::Gdi::*;
    unsafe {
        let saved = SaveDC(dc);
        let brush = GetStockObject(DC_BRUSH);
        SelectObject(dc, brush);
        SelectObject(dc, GetStockObject(DC_PEN));
        SetDCBrushColor(dc, BACKGROUND);
        FillRect(dc, rect, windows::Win32::Graphics::Gdi::HBRUSH(brush.0));
        SetDCBrushColor(dc, WHITE);
        SetDCPenColor(dc, BORDER);
        let dpi = GetDpiForWindow(hwnd).max(BASE_DPI);
        if let Some(state) = settings_state_pointer(hwnd).as_ref() {
            let c = state.borrow().controls;
            let _ = SetViewportOrgEx(dc, -c.scroll_x, -c.scroll_y, None);
        }
        for [x, y, width, height] in CARDS {
            let _ = RoundRect(
                dc,
                scaled(x, dpi),
                scaled(y, dpi),
                scaled(x + width, dpi),
                scaled(y + height, dpi),
                scaled(22, dpi),
                scaled(22, dpi),
            );
        }
        let _ = RestoreDC(dc, saved);
    }
}

fn paint_button(item: &windows::Win32::UI::Controls::DRAWITEMSTRUCT) {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::Controls::{ODS_DISABLED, ODS_FOCUS, ODS_SELECTED};
    unsafe {
        let saved = SaveDC(item.hDC);
        SelectObject(item.hDC, GetStockObject(DC_BRUSH));
        SelectObject(item.hDC, GetStockObject(DC_PEN));
        let active_category = if (CATEGORY_ID as u32..CATEGORY_ID as u32 + 4).contains(&item.CtlID)
        {
            windows::Win32::UI::WindowsAndMessaging::GetParent(item.hwndItem)
                .ok()
                .and_then(|parent| settings_state_pointer(parent).as_ref())
                .map(|state| {
                    state.borrow().controls.category == (item.CtlID as i32 - CATEGORY_ID) as usize
                })
                .unwrap_or(false)
        } else {
            false
        };
        let primary = item.CtlID == SAVE_BUTTON_ID as u32 || active_category;
        let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
        let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
        let fill = if disabled {
            BORDER
        } else if primary {
            if pressed {
                COLORREF(0x0050520b)
            } else {
                ACCENT
            }
        } else {
            COLORREF(0x00f2f4f4)
        };
        SetDCBrushColor(item.hDC, fill);
        SetDCPenColor(item.hDC, if primary { fill } else { BORDER });
        let r = item.rcItem;
        let dpi = GetDpiForWindow(item.hwndItem).max(BASE_DPI);
        let _ = RoundRect(
            item.hDC,
            r.left,
            r.top,
            r.right,
            r.bottom,
            scaled(12, dpi),
            scaled(12, dpi),
        );
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(
            item.hDC,
            if disabled {
                MUTED
            } else if primary {
                WHITE
            } else if item.CtlID == CLEAR_LEARNING_ID as u32 {
                DANGER
            } else {
                TEXT
            },
        );
        let font = SendMessageW(
            item.hwndItem,
            windows::Win32::UI::WindowsAndMessaging::WM_GETFONT,
            WPARAM(0),
            LPARAM(0),
        );
        if font.0 != 0 {
            SelectObject(item.hDC, HGDIOBJ(font.0 as *mut c_void));
        }
        let mut text = [0u16; 128];
        let length = GetWindowTextW(item.hwndItem, &mut text) as usize;
        let mut text_rect = r;
        if pressed {
            text_rect.top += 1;
        }
        DrawTextW(
            item.hDC,
            &mut text[..length],
            &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        if item.itemState.0 & ODS_FOCUS.0 != 0 {
            let inset = scaled(4, dpi);
            let focus = RECT {
                left: r.left + inset,
                top: r.top + inset,
                right: r.right - inset,
                bottom: r.bottom - inset,
            };
            let _ = DrawFocusRect(item.hDC, &focus);
        }
        let _ = RestoreDC(item.hDC, saved);
    }
}

const PAGE_LABEL_ID: i32 = 100;
const PAGE_EDIT_ID: i32 = 101;
const PAGE_HINT_ID: i32 = 102;
const THEME_LABEL_ID: i32 = 103;
const THEME_COMBO_ID: i32 = 104;
const NOTICE_ID: i32 = 105;
const LEARNING_ID: i32 = 106;
const CLEAR_LEARNING_ID: i32 = 107;
const DOMAIN_COMBO_ID: i32 = 108;
const DOMAIN_ENABLED_ID: i32 = 109;
const DICTIONARY_STATUS_ID: i32 = 110;
const AI_CORRECTION_ID: i32 = 111;
const AI_CORRECTION_HINT_ID: i32 = 112;
const LEARNING_HINT_ID: i32 = 113;
const AI_SCOPE_COMPATIBILITY_ID: i32 = 114;
const SCRIPT_LABEL_ID: i32 = 119;
const SCRIPT_COMBO_ID: i32 = 120;
const SAVE_BUTTON_ID: i32 = IDOK.0;
const CANCEL_BUTTON_ID: i32 = IDCANCEL.0;

/// Runs the settings window on the current thread and returns after it closes.
pub fn run_settings_app() -> Result<(), String> {
    initialize_standard_controls()?;
    let store = match SettingsStore::for_current_user() {
        Ok(store) => store,
        Err(error) => {
            let message = format!("无法确定当前用户的设置路径：{error}");
            message_box(
                HWND::default(),
                &message,
                "Flower 设置",
                MB_OK | MB_ICONERROR,
            );
            return Err(message);
        }
    };
    let settings = match store.load() {
        Ok(settings) => settings,
        Err(error) => {
            let message = format!(
                "无法读取设置文件：{error}\n\n路径：{}\n\n是否使用默认设置继续？\n\
                 选择“否”将退出，并且不会覆盖现有文件。",
                store.path().display()
            );
            if message_box(
                HWND::default(),
                &message,
                "Flower 设置",
                MB_YESNO | MB_ICONERROR,
            ) != IDYES
            {
                return Ok(());
            }
            Settings::default()
        }
    };

    // SAFETY: The executable module handle is used only to register and create
    // this process's settings window. The cursor is the shared system arrow.
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map(HINSTANCE::from)
        .map_err(|error| format!("无法获取设置程序模块：{error}"))?;
    let cursor = unsafe { LoadCursorW(HINSTANCE::default(), IDC_ARROW) }
        .map_err(|error| format!("无法加载窗口光标：{error}"))?;
    let background = unsafe { GetSysColorBrush(COLOR_BTNFACE) };
    let window_class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(settings_window_proc),
        hInstance: instance,
        hCursor: cursor,
        hbrBackground: background,
        lpszClassName: WINDOW_CLASS_NAME,
        ..Default::default()
    };

    // SAFETY: `window_class` is fully initialized and points only to static
    // names and this module's window procedure.
    if unsafe { RegisterClassExW(&window_class) } == 0 {
        return Err(format!(
            "无法注册设置窗口类：{}",
            WindowsError::from_win32()
        ));
    }

    let dictionary_store = match DictionaryStore::for_current_user() {
        Ok(store) => store,
        Err(error) => {
            let _ = unsafe { UnregisterClassW(WINDOW_CLASS_NAME, instance) };
            return Err(format!("无法确定当前用户词典路径：{error}"));
        }
    };
    let state = Box::new(RefCell::new(SettingsUiState {
        store,
        dictionary_store,
        settings,
        controls: SettingsControls::default(),
        fonts: Vec::new(),
        saving: false,
        creation_error: None,
        domain_names: Vec::new(),
        dictionary_ready: false,
        initial_dictionary: None,
        initial_started: false,
        #[cfg(test)]
        initial_loader: None,
    }));
    let state_pointer = state_pointer(state.as_ref());
    let dpi = unsafe { GetDpiForSystem() }.max(BASE_DPI);
    let style = WS_OVERLAPPED
        | WS_CAPTION
        | WS_SYSMENU
        | WS_MINIMIZEBOX
        | windows::Win32::UI::WindowsAndMessaging::WS_SIZEBOX
        | windows::Win32::UI::WindowsAndMessaging::WS_HSCROLL
        | WS_VSCROLL;
    let mut window_rect = RECT {
        left: 0,
        top: 0,
        right: scaled(CLIENT_WIDTH, dpi),
        bottom: scaled(CLIENT_HEIGHT, dpi),
    };
    let adjusted = unsafe {
        AdjustWindowRectExForDpi(&mut window_rect, style, FALSE, WS_EX_CONTROLPARENT, dpi)
    };
    if let Err(error) = adjusted {
        drop(state);
        let _ = unsafe { UnregisterClassW(WINDOW_CLASS_NAME, instance) };
        return Err(format!("无法计算设置窗口尺寸：{error}"));
    }
    let mut work_area = RECT::default();
    if unsafe {
        windows::Win32::UI::WindowsAndMessaging::SystemParametersInfoW(
            windows::Win32::UI::WindowsAndMessaging::SPI_GETWORKAREA,
            0,
            Some((&mut work_area as *mut RECT).cast()),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok()
    {
        window_rect.right = window_rect.left
            + (window_rect.right - window_rect.left)
                .min((work_area.right - work_area.left - 24).max(240));
        window_rect.bottom = window_rect.top
            + (window_rect.bottom - window_rect.top)
                .min((work_area.bottom - work_area.top - 24).max(200));
    }

    // SAFETY: The class is registered, the create parameter points to `state`
    // for the duration of the message loop, and the window starts hidden.
    let window_title = wide_null("Flower 设置");
    let created = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            WINDOW_CLASS_NAME,
            PCWSTR(window_title.as_ptr()),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            window_rect.right - window_rect.left,
            window_rect.bottom - window_rect.top,
            HWND::default(),
            HMENU::default(),
            instance,
            Some(state_pointer),
        )
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(error) => {
            let detail = state
                .try_borrow_mut()
                .ok()
                .and_then(|mut state| state.creation_error.take())
                .unwrap_or_else(|| format!("{error}"));
            delete_all_fonts(state.as_ref());
            drop(state);
            let _ = unsafe { UnregisterClassW(WINDOW_CLASS_NAME, instance) };
            return Err(format!("无法创建设置窗口：{detail}"));
        }
    };

    unsafe {
        // ShowWindow reports the previous visibility state, not success.
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = UpdateWindow(hwnd);
    }
    let page_edit = state
        .try_borrow()
        .ok()
        .map(|state| state.controls.page_edit)
        .unwrap_or_default();
    if !page_edit.is_invalid() {
        let _ = unsafe { SetFocus(page_edit) };
    }

    let mut message = MSG::default();
    let loop_result = loop {
        // SAFETY: `message` is writable storage for the thread message queue.
        let result = unsafe { GetMessageW(&mut message, HWND::default(), 0, 0) };
        if result.0 == -1 {
            break Err("读取设置窗口消息失败".to_owned());
        }
        if result.0 == 0 {
            break Ok(());
        }

        // Owner-draw buttons do not advertise DLGC_DEFPUSHBUTTON. Route
        // Enter/Esc before IsDialogMessage can synthesize the default IDOK.
        if preprocess_settings_key(hwnd, state.as_ref(), &message) {
            ensure_settings_focus_visible(hwnd, state.as_ref());
            continue;
        }
        // SAFETY: Both the dialog and message belong to this thread.
        // Tab/Shift+Tab, Space and all remaining dialog keys stay native.
        if unsafe { IsDialogMessageW(hwnd, &message) }.as_bool() {
            ensure_settings_focus_visible(hwnd, state.as_ref());
            continue;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        ensure_settings_focus_visible(hwnd, state.as_ref());
    };

    let cleanup = cleanup_window(hwnd, state.as_ref());
    let creation_error = state
        .try_borrow_mut()
        .ok()
        .and_then(|mut state| state.creation_error.take());

    if matches!(
        cleanup,
        WindowCleanup::DetachFailed | WindowCleanup::UnattachedLiveWindow
    ) {
        // The window may still reference this state. Leaking the state and
        // its GDI handles is safer than freeing memory a WndProc can reach.
        let message = match cleanup {
            WindowCleanup::DetachFailed => "无法分离设置窗口状态，已停止释放设置状态".to_owned(),
            WindowCleanup::UnattachedLiveWindow => {
                "设置窗口句柄不再属于当前状态，未销毁可能是其他窗口的句柄".to_owned()
            }
            _ => unreachable!(),
        };
        let unregister = unsafe { UnregisterClassW(WINDOW_CLASS_NAME, instance) };
        std::mem::forget(state);
        return Err(match unregister {
            Ok(()) => message,
            Err(error) => format!("{message}；注销窗口类也失败：{error}"),
        });
    }

    if matches!(
        cleanup,
        WindowCleanup::Destroyed | WindowCleanup::AlreadyGone
    ) {
        delete_all_fonts(state.as_ref());
    }

    drop(state);
    let unregister = unsafe { UnregisterClassW(WINDOW_CLASS_NAME, instance) };
    match (creation_error, unregister, cleanup, loop_result) {
        (Some(error), _, _, _) => Err(error),
        (None, Err(error), _, _) => Err(format!("无法注销设置窗口类：{error}")),
        (None, Ok(()), WindowCleanup::DestroyedFailed, _) => {
            Err("设置窗口状态已分离，但销毁窗口失败".to_owned())
        }
        (None, Ok(()), _, result) => result,
    }
}

fn initialize_standard_controls() -> Result<(), String> {
    use windows::Win32::UI::Controls::{
        InitCommonControlsEx, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX,
    };
    let controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    // Called only by the standalone settings entry point and owned fixtures,
    // never by TSF activation or DLL initialization.
    if unsafe { InitCommonControlsEx(&controls) }.as_bool() {
        Ok(())
    } else {
        Err("无法初始化设置窗口的标准控件。".to_owned())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SettingsKeyRoute {
    Dialog,
    NativeDropdown,
    Command(i32),
}

fn settings_key_route(
    hwnd: HWND,
    state: &RefCell<SettingsUiState>,
    message: &MSG,
) -> SettingsKeyRoute {
    use windows::Win32::UI::WindowsAndMessaging::IsChild;
    if message.message != WM_KEYDOWN
        || !matches!(message.wParam.0 as u16, key if key == VK_RETURN.0 || key == VK_ESCAPE.0)
        || !window_is_owned(hwnd, state)
        || (message.hwnd != hwnd && !unsafe { IsChild(hwnd, message.hwnd) }.as_bool())
    {
        return SettingsKeyRoute::Dialog;
    }
    // Bypass IsDialogMessage as well: the open combo must consume Enter/Esc
    // itself, not let the dialog turn them into save/close commands.
    if theme_dropdown_open(state) {
        return SettingsKeyRoute::NativeDropdown;
    }
    let command = if message.wParam.0 == VK_ESCAPE.0 as usize {
        CANCEL_BUTTON_ID
    } else {
        // WM_KEYDOWN's HWND is its focus target; no global focus lookup or
        // handle from another window may determine a settings action.
        match unsafe { GetDlgCtrlID(message.hwnd) } {
            id @ (CANCEL_BUTTON_ID | CLEAR_LEARNING_ID | LEARNING_ID | DOMAIN_ENABLED_ID) => id,
            id @ (390 | 391 | 392 | DRAFT_SHORTCUT_ID | 411 | RESET_SHORTCUTS_ID | 400..=403) => id,
            _ => SAVE_BUTTON_ID,
        }
    };
    SettingsKeyRoute::Command(command)
}

fn capture_shortcut(hwnd: HWND, state: &RefCell<SettingsUiState>, message: &MSG) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState};
    use windows::Win32::UI::WindowsAndMessaging::{WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP};
    let c = state.borrow().controls;
    let Some(index) = c.recording else {
        return false;
    };
    if !window_is_owned(hwnd, state) {
        return false;
    }
    let restore = || {
        state.borrow_mut().controls.recording = None;
        if let Ok(values) = read_shortcuts(c) {
            select_shortcuts(c, values);
        }
    };
    if unsafe { GetFocus() } != c.shortcuts[index] {
        restore();
        return false;
    }
    if !matches!(
        message.message,
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
    ) {
        return false;
    }
    if message.hwnd != c.shortcuts[index] {
        return false;
    }
    let key = message.wParam.0 as u16;
    let down = matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN);
    if key == 0x09 || key == 0x1b || key == 0x0d {
        restore();
        return key != 0x09;
    }
    if down && message.lParam.0 & (1 << 30) != 0 {
        return true;
    }
    let mods = [(0x11, 1), (0x12, 2), (0x10, 4), (0x5b, 8), (0x5c, 8)]
        .into_iter()
        .fold(0, |mask, (vk, bit)| {
            mask | if unsafe { GetKeyState(vk) } < 0 {
                bit
            } else {
                0
            }
        });
    let modifier = matches!(key, 0x10..=0x12 | 0x5b | 0x5c | 0xa0..=0xa5);
    if modifier && down {
        state.borrow_mut().controls.pending_modifiers |= mods;
        return true;
    }
    let value = if modifier && !down && mods == 0 && c.pending_modifiers != 0 {
        Some(WritingShortcut::new(0, c.pending_modifiers))
    } else if !modifier && down {
        Some(WritingShortcut::new(key, mods))
    } else {
        None
    };
    if let Some(value) = value {
        let mut values = read_shortcuts(c).unwrap_or_default();
        let other = if index == 0 {
            values.polish
        } else {
            values.draft
        };
        restore();
        if value.is_supported() && value != other {
            if index == 0 {
                values.draft = value;
            } else {
                values.polish = value;
            }
            select_shortcuts(c, values);
        } else {
            let text = wide_null("组合不支持、为系统保留或与另一功能冲突；原值未更改。");
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                    c.shortcut_hint,
                    PCWSTR(text.as_ptr()),
                );
            }
        }
    }
    true
}

fn preprocess_settings_key(hwnd: HWND, state: &RefCell<SettingsUiState>, message: &MSG) -> bool {
    if capture_shortcut(hwnd, state, message) {
        return true;
    }
    match settings_key_route(hwnd, state, message) {
        SettingsKeyRoute::Dialog => false,
        SettingsKeyRoute::NativeDropdown => {
            unsafe {
                let _ = TranslateMessage(message);
                DispatchMessageW(message);
            }
            true
        }
        SettingsKeyRoute::Command(command) => {
            let _ = unsafe {
                PostMessageW(
                    hwnd,
                    WM_COMMAND,
                    WPARAM(command as usize | ((BN_CLICKED as usize) << 16)),
                    LPARAM(0),
                )
            };
            true
        }
    }
}

struct SettingsUiState {
    store: SettingsStore,
    dictionary_store: DictionaryStore,
    settings: Settings,
    controls: SettingsControls,
    fonts: Vec<HFONT>,
    saving: bool,
    creation_error: Option<String>,
    domain_names: Vec<String>,
    dictionary_ready: bool,
    initial_dictionary: Option<InitialDictionaryLoad>,
    initial_started: bool,
    #[cfg(test)]
    initial_loader: Option<Box<dyn FnOnce() -> DictionaryResult + Send>>,
}

#[derive(Clone, Copy, Default)]
struct SettingsControls {
    recording: Option<usize>,
    pending_modifiers: u8,
    category: usize,
    scroll_x: i32,
    scroll_y: i32,
    categories: [HWND; 4],
    shortcuts: [HWND; 2],
    shortcut_labels: [HWND; 2],
    reset_shortcuts: HWND,
    shortcut_hint: HWND,
    online_settings: HWND,
    online_ai: HWND,
    online_ai_hint: HWND,
    model_api_settings: HWND,
    restore_send_confirmation: HWND,
    script_combo: HWND,
    page_edit: HWND,
    theme_combo: HWND,
    save_button: HWND,
    cancel_button: HWND,
    notice: HWND,
    labels: [HWND; 4],
    learning: HWND,
    clear_learning: HWND,
    domain_combo: HWND,
    domain_enabled: HWND,
    dictionary_status: HWND,
    ai_correction: HWND,
    ai_scope_compatibility: HWND,
    ai_correction_hint: HWND,
    learning_hint: HWND,
    headings: [HWND; 6],
}

unsafe extern "system" fn settings_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: Windows invokes this procedure only for a live settings
        // window whose user-data slot refers to the owner's Box.
        unsafe { handle_settings_message(hwnd, message, wparam, lparam) }
    })) {
        Ok(result) => result,
        Err(_) => {
            if message == WM_DESTROY {
                unsafe { PostQuitMessage(0) };
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

unsafe fn handle_settings_message(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        windows::Win32::UI::WindowsAndMessaging::WM_SIZE
        | windows::Win32::UI::WindowsAndMessaging::WM_HSCROLL
        | windows::Win32::UI::WindowsAndMessaging::WM_VSCROLL => {
            if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                scroll_settings(hwnd, state, message, wparam);
            }
            LRESULT(0)
        }
        WM_NCCREATE => {
            if lparam.0 != 0 {
                let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CREATE => {
            let Some(state) = (unsafe { settings_state_pointer(hwnd).as_ref() }) else {
                return LRESULT(-1);
            };
            match initialize_controls(hwnd, state) {
                Ok(()) => LRESULT(0),
                Err(error) => {
                    if let Ok(mut state) = state.try_borrow_mut() {
                        state.creation_error = Some(error);
                    }
                    LRESULT(-1)
                }
            }
        }
        WM_COMMAND => {
            let control_id = (wparam.0 & 0xffff) as i32;
            let notification = ((wparam.0 >> 16) & 0xffff) as u32;
            if (CATEGORY_ID..CATEGORY_ID + 4).contains(&control_id) && notification == BN_CLICKED {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    show_category(state, (control_id - CATEGORY_ID) as usize);
                }
            } else if (DRAFT_SHORTCUT_ID..DRAFT_SHORTCUT_ID + 2).contains(&control_id)
                && notification == BN_CLICKED
            {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    let index = (control_id - DRAFT_SHORTCUT_ID) as usize;
                    let c = state.borrow().controls;
                    if let Ok(values) = read_shortcuts(c) {
                        select_shortcuts(c, values);
                    }
                    state.borrow_mut().controls.recording = Some(index);
                    state.borrow_mut().controls.pending_modifiers = 0;
                    let text = wide_null("正在录制… Esc取消 / Tab离开");
                    unsafe {
                        let _ = SetFocus(c.shortcuts[index]);
                        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                            c.shortcuts[index],
                            PCWSTR(text.as_ptr()),
                        );
                    }
                }
            } else if control_id == RESET_SHORTCUTS_ID && notification == BN_CLICKED {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    state.borrow_mut().controls.recording = None;
                    select_shortcuts(state.borrow().controls, WritingShortcuts::default());
                }
            } else if control_id == 392 && notification == BN_CLICKED {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    let store = state.borrow().store.clone();
                    if let Err(error) = super::model_api_ui::show_model_api_settings(hwnd, store) {
                        message_box(hwnd, &error, "模型 API 设置", MB_OK | MB_ICONERROR);
                    }
                }
            } else if control_id == 390 && notification == BN_CLICKED {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    let store = state.borrow().store.clone();
                    match super::online_settings_ui::show_online_settings(hwnd, store) {
                        Ok(Some(saved)) => state.borrow_mut().settings.online_polish = saved,
                        Ok(None) => {}
                        Err(error) => {
                            message_box(hwnd, &error, "在线 AI 设置", MB_OK | MB_ICONERROR);
                        }
                    }
                }
            } else if control_id == SAVE_BUTTON_ID && notification == BN_CLICKED {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    save_settings(hwnd, state);
                }
            } else if control_id == 391 && notification == BN_CLICKED {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    let store = state.borrow().store.clone();
                    match crate::writing::consent::remember(&store, false) {
                        Ok(()) => {
                            state.borrow_mut().settings.writing_send_consent = None;
                            message_box(hwnd, "已恢复帮写发送确认。", "Flower 设置", MB_OK);
                        }
                        Err(error) => {
                            message_box(hwnd, &error, "Flower 设置", MB_OK | MB_ICONERROR);
                        }
                    }
                }
            } else if control_id == CANCEL_BUTTON_ID {
                let _ = unsafe { DestroyWindow(hwnd) };
            } else if matches!(
                control_id,
                LEARNING_ID | CLEAR_LEARNING_ID | DOMAIN_ENABLED_ID
            ) && notification == BN_CLICKED
            {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    dictionary_action(hwnd, state, control_id);
                }
            } else if control_id == DOMAIN_COMBO_ID
                && notification == windows::Win32::UI::WindowsAndMessaging::CBN_SELCHANGE
            {
                if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                    if state.borrow().dictionary_ready {
                        refresh_dictionary(hwnd, state);
                    }
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_PAINT => {
            paint_settings(hwnd);
            if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                start_initial_dictionary(hwnd, state);
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == DICTIONARY_TIMER => {
            if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                poll_initial_dictionary(hwnd, state);
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_PRINTCLIENT => {
            let mut rect = RECT::default();
            let _ =
                unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect) };
            paint_canvas(hwnd, HDC(wparam.0 as *mut c_void), &rect);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DRAWITEM => {
            if lparam.0 != 0 {
                let item =
                    unsafe { &*(lparam.0 as *const windows::Win32::UI::Controls::DRAWITEMSTRUCT) };
                paint_button(item);
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | windows::Win32::UI::WindowsAndMessaging::WM_CTLCOLORBTN => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) };
            let background = if matches!(id, NOTICE_ID | 200 | 201 | 400..=403) {
                BACKGROUND
            } else {
                WHITE
            };
            unsafe {
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(
                    hdc,
                    if matches!(
                        id,
                        PAGE_HINT_ID
                            | NOTICE_ID
                            | AI_CORRECTION_HINT_ID
                            | DICTIONARY_STATUS_ID
                            | LEARNING_HINT_ID
                            | 201
                    ) {
                        MUTED
                    } else {
                        TEXT
                    },
                );
                windows::Win32::Graphics::Gdi::SetDCBrushColor(hdc, background);
            }
            LRESULT(
                unsafe {
                    windows::Win32::Graphics::Gdi::GetStockObject(
                        windows::Win32::Graphics::Gdi::DC_BRUSH,
                    )
                }
                .0 as isize,
            )
        }
        WM_DPICHANGED => {
            let new_dpi = (wparam.0 & 0xffff) as u32;
            if lparam.0 != 0 {
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                let _ = unsafe {
                    MoveWindow(
                        hwnd,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        TRUE,
                    )
                };
            }
            if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                apply_layout(state, new_dpi.max(BASE_DPI));
            }
            LRESULT(0)
        }
        WM_SYSCOLORCHANGE => {
            // SAFETY: The live top-level window can repaint its system-colored
            // background after the user changes system colors.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, TRUE);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(state) = unsafe { settings_state_pointer(hwnd).as_ref() } {
                unsafe {
                    let _ =
                        windows::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, DICTIONARY_TIMER);
                }
                state.borrow_mut().dictionary_ready = false;
                state.borrow_mut().initial_dictionary.take();
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn initialize_controls(hwnd: HWND, state: &RefCell<SettingsUiState>) -> Result<(), String> {
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map(HINSTANCE::from)
        .map_err(|error| format!("无法获取设置程序模块：{error}"))?;
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(BASE_DPI);
    let online = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("在线 AI 设置"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_OWNERDRAW as u32,
        390,
    )?;
    update_controls(state, |controls| controls.online_settings = online)?;
    let model_api = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("模型 API…"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_OWNERDRAW as u32,
        392,
    )?;
    update_controls(state, |controls| controls.model_api_settings = model_api)?;
    let restore = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("恢复帮写发送确认"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_PUSHBUTTON as u32,
        391,
    )?;
    update_controls(state, |controls| {
        controls.restore_send_confirmation = restore
    })?;
    let settings = state
        .try_borrow()
        .map(|state| state.settings.clone())
        .map_err(|_| "设置窗口状态正忙，无法初始化".to_owned())?;

    for (index, caption) in [
        String::new(),
        String::new(),
        "常规".to_owned(),
        "AI功能".to_owned(),
        "词库与学习".to_owned(),
        "专业词库".to_owned(),
    ]
    .iter()
    .enumerate()
    {
        if index < 2 {
            continue;
        }
        let text = wide_null(caption);
        let control = create_control(
            hwnd,
            instance,
            windows::core::w!("STATIC"),
            PCWSTR(text.as_ptr()),
            WS_CHILD.0 | WS_VISIBLE.0,
            200 + index as i32,
        )?;
        update_controls(state, |controls| controls.headings[index] = control)?;
    }
    let label = create_control(
        hwnd,
        instance,
        windows::core::w!("STATIC"),
        windows::core::w!("候选页大小（1-99）"),
        WS_CHILD.0 | WS_VISIBLE.0,
        PAGE_LABEL_ID,
    )?;
    update_controls(state, |controls| controls.labels[0] = label)?;

    let page_edit = create_control(
        hwnd,
        instance,
        windows::core::w!("EDIT"),
        PCWSTR::null(),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::WS_BORDER.0
            | ES_NUMBER as u32
            | ES_AUTOHSCROLL as u32,
        PAGE_EDIT_ID,
    )?;
    update_controls(state, |controls| controls.page_edit = page_edit)?;

    let label = create_control(
        hwnd,
        instance,
        windows::core::w!("STATIC"),
        windows::core::w!("可输入 1 到 99。"),
        WS_CHILD.0 | WS_VISIBLE.0,
        PAGE_HINT_ID,
    )?;
    update_controls(state, |controls| controls.labels[1] = label)?;

    let label = create_control(
        hwnd,
        instance,
        windows::core::w!("STATIC"),
        windows::core::w!("候选窗主题"),
        WS_CHILD.0 | WS_VISIBLE.0,
        THEME_LABEL_ID,
    )?;
    update_controls(state, |controls| controls.labels[2] = label)?;

    let theme_combo = create_control(
        hwnd,
        instance,
        windows::core::w!("COMBOBOX"),
        PCWSTR::null(),
        WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
        THEME_COMBO_ID,
    )?;
    update_controls(state, |controls| controls.theme_combo = theme_combo)?;

    let label = create_control(
        hwnd,
        instance,
        windows::core::w!("STATIC"),
        windows::core::w!("输出字形"),
        WS_CHILD.0 | WS_VISIBLE.0,
        SCRIPT_LABEL_ID,
    )?;
    update_controls(state, |controls| controls.labels[3] = label)?;

    let script_combo = create_control(
        hwnd,
        instance,
        windows::core::w!("COMBOBOX"),
        PCWSTR::null(),
        WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
        SCRIPT_COMBO_ID,
    )?;
    update_controls(state, |controls| controls.script_combo = script_combo)?;
    for text in ["简体", "通用繁体"] {
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        unsafe {
            SendMessageW(
                script_combo,
                CB_ADDSTRING,
                WPARAM::default(),
                LPARAM(text.as_ptr() as isize),
            );
        }
    }
    unsafe {
        SendMessageW(
            script_combo,
            CB_SETCURSEL,
            WPARAM(usize::from(
                settings.chinese_script == crate::ChineseScript::Traditional,
            )),
            LPARAM::default(),
        );
    }
    let online_ai = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("启用在线AI功能"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_AUTOCHECKBOX as u32,
        423,
    )?;
    update_controls(state, |controls| controls.online_ai = online_ai)?;
    set_checked(online_ai, settings.online_ai_enabled);
    let online_ai_hint = create_control(
        hwnd,
        instance,
        windows::core::w!("STATIC"),
        windows::core::w!("仅控制在线帮写和润色，不影响离线纠错。\n使用时会向所选模型服务外发文本，可能产生费用。"),
        WS_CHILD.0 | WS_VISIBLE.0,
        424,
    )?;
    update_controls(state, |controls| controls.online_ai_hint = online_ai_hint)?;
    let ai_correction = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("启用离线 AI 纠错"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_AUTOCHECKBOX as u32,
        AI_CORRECTION_ID,
    )?;
    update_controls(state, |controls| controls.ai_correction = ai_correction)?;
    set_checked(ai_correction, settings.ai_correction_enabled);
    let ai_scope_compatibility = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("兼容未声明范围的应用"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_AUTOCHECKBOX as u32,
        AI_SCOPE_COMPATIBILITY_ID,
    )?;
    update_controls(state, |controls| {
        controls.ai_scope_compatibility = ai_scope_compatibility
    })?;
    set_checked(
        ai_scope_compatibility,
        settings.ai_undeclared_scope_compatibility,
    );
    let ai_correction_hint = create_control(
        hwnd,
        instance,
        windows::core::w!("STATIC"),
        windows::core::w!(
            "离线检查，首次加载最多5秒。\n普通排序学习与AI共用兼容，不读取正文。\n自定义私密框未声明时可能无法识别。\n可关闭兼容恢复严格限制。\n保存后切换生效，AI建议须手动选择。"
        ),
        WS_CHILD.0 | WS_VISIBLE.0,
        AI_CORRECTION_HINT_ID,
    )?;
    update_controls(state, |controls| {
        controls.ai_correction_hint = ai_correction_hint
    })?;
    let learning_hint = create_control(
        hwnd, instance, windows::core::w!("STATIC"),
        windows::core::w!("默认开启；选词前移，逐段组词可记忆。\n关闭停止新增，已有自动词仍可使用。\n清空删除自动词，保留手工/专业词条。"),
        WS_CHILD.0 | WS_VISIBLE.0, LEARNING_HINT_ID,
    )?;
    update_controls(state, |controls| controls.learning_hint = learning_hint)?;

    let notice = create_control(
        hwnd,
        instance,
        windows::core::w!("STATIC"),
        windows::core::w!("在线AI开关保存后立即生效；其他设置需重新切换 Flower。"),
        WS_CHILD.0 | WS_VISIBLE.0,
        NOTICE_ID,
    )?;
    update_controls(state, |controls| controls.notice = notice)?;

    let save_button = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("保存"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_OWNERDRAW as u32,
        SAVE_BUTTON_ID,
    )?;
    update_controls(state, |controls| controls.save_button = save_button)?;

    let cancel_button = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("关闭"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_OWNERDRAW as u32,
        CANCEL_BUTTON_ID,
    )?;
    update_controls(state, |controls| controls.cancel_button = cancel_button)?;

    for (id, class, caption, extra_style) in [
        (
            LEARNING_ID,
            windows::core::w!("BUTTON"),
            windows::core::w!("记住选词顺序 · 仅保存在本机"),
            windows::Win32::UI::WindowsAndMessaging::BS_AUTOCHECKBOX as u32,
        ),
        (
            CLEAR_LEARNING_ID,
            windows::core::w!("BUTTON"),
            windows::core::w!("清空学习记录…"),
            windows::Win32::UI::WindowsAndMessaging::BS_OWNERDRAW as u32,
        ),
        (
            DOMAIN_COMBO_ID,
            windows::core::w!("COMBOBOX"),
            PCWSTR::null(),
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
        ),
        (
            DOMAIN_ENABLED_ID,
            windows::core::w!("BUTTON"),
            windows::core::w!("启用选中专业词库（立即保存）"),
            windows::Win32::UI::WindowsAndMessaging::BS_AUTOCHECKBOX as u32,
        ),
        (
            DICTIONARY_STATUS_ID,
            windows::core::w!("STATIC"),
            PCWSTR::null(),
            0,
        ),
    ] {
        let control = create_control(
            hwnd,
            instance,
            class,
            caption,
            WS_CHILD.0
                | WS_VISIBLE.0
                | (if id == DICTIONARY_STATUS_ID {
                    0
                } else {
                    WS_TABSTOP.0
                })
                | extra_style,
            id,
        )?;
        update_controls(state, |controls| match id {
            LEARNING_ID => controls.learning = control,
            CLEAR_LEARNING_ID => controls.clear_learning = control,
            DOMAIN_COMBO_ID => controls.domain_combo = control,
            DOMAIN_ENABLED_ID => controls.domain_enabled = control,
            _ => controls.dictionary_status = control,
        })?;
    }
    for control in {
        let c = state.borrow().controls;
        [
            c.learning,
            c.clear_learning,
            c.domain_combo,
            c.domain_enabled,
        ]
    } {
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(control, FALSE);
        }
    }
    set_control_text(hwnd, DICTIONARY_STATUS_ID, "正在读取用户词典…")?;

    set_control_text(
        hwnd,
        PAGE_EDIT_ID,
        &settings.candidate_page_size.to_string(),
    )?;
    for (index, label) in ["系统颜色", "浅色", "深色"].iter().enumerate() {
        let text = wide_null(label);
        let result = unsafe {
            SendMessageW(
                theme_combo,
                CB_ADDSTRING,
                WPARAM::default(),
                LPARAM(text.as_ptr() as isize),
            )
        };
        if result.0 == CB_ERR as isize {
            return Err("无法初始化主题选项".to_owned());
        }
        if index == theme_index(settings.theme) {
            unsafe {
                SendMessageW(theme_combo, CB_SETCURSEL, WPARAM(index), LPARAM::default());
            }
        }
    }

    for (index, caption) in ["常规", "AI功能", "快捷键", "词库与学习"]
        .into_iter()
        .enumerate()
    {
        let text = wide_null(caption);
        let control = create_control(
            hwnd,
            instance,
            windows::core::w!("BUTTON"),
            PCWSTR(text.as_ptr()),
            WS_CHILD.0
                | WS_VISIBLE.0
                | WS_TABSTOP.0
                | windows::Win32::UI::WindowsAndMessaging::BS_OWNERDRAW as u32,
            CATEGORY_ID + index as i32,
        )?;
        state.borrow_mut().controls.categories[index] = control;
    }
    for (index, caption) in ["帮写", "润色"].into_iter().enumerate() {
        let text = wide_null(caption);
        let label = create_control(
            hwnd,
            instance,
            windows::core::w!("STATIC"),
            PCWSTR(text.as_ptr()),
            WS_CHILD.0 | WS_VISIBLE.0,
            420 + index as i32,
        )?;
        let combo = create_control(
            hwnd,
            instance,
            windows::core::w!("BUTTON"),
            PCWSTR::null(),
            WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0,
            DRAFT_SHORTCUT_ID + index as i32,
        )?;
        state.borrow_mut().controls.shortcuts[index] = combo;
        state.borrow_mut().controls.shortcut_labels[index] = label;
    }
    let reset = create_control(
        hwnd,
        instance,
        windows::core::w!("BUTTON"),
        windows::core::w!("恢复默认快捷键"),
        WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | windows::Win32::UI::WindowsAndMessaging::BS_OWNERDRAW as u32,
        RESET_SHORTCUTS_ID,
    )?;
    state.borrow_mut().controls.reset_shortcuts = reset;
    let hint = create_control(hwnd, instance, windows::core::w!("STATIC"),
        windows::core::w!("仅在有效 Flower 输入上下文中使用，不是全局热键。\n点击按钮或按 Enter 录制；Esc、Enter、Tab 取消，不保存。\n系统可能截获 Win 组合或 Ctrl+Space，无法保证可录制或生效。\nCtrl+Shift+F 保留。恢复默认仍需保存，重新切换 Flower 生效。"),
        WS_CHILD.0 | WS_VISIBLE.0, 422)?;
    state.borrow_mut().controls.shortcut_hint = hint;
    select_shortcuts(state.borrow().controls, settings.writing_shortcuts);
    replace_font(state, dpi).map_err(|error| format!("无法创建设置窗口字体：{error}"))?;
    layout_controls(state, dpi);
    show_category(state, 0);
    Ok(())
}

fn update_controls(
    state: &RefCell<SettingsUiState>,
    update: impl FnOnce(&mut SettingsControls),
) -> Result<(), String> {
    let mut state = state
        .try_borrow_mut()
        .map_err(|_| "设置窗口状态正忙，无法保存控件句柄".to_owned())?;
    update(&mut state.controls);
    Ok(())
}

fn create_control(
    parent: HWND,
    instance: HINSTANCE,
    class_name: PCWSTR,
    text: PCWSTR,
    style: u32,
    control_id: i32,
) -> Result<HWND, String> {
    let menu = HMENU(control_id as isize as *mut c_void);
    unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            text,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(style),
            0,
            0,
            1,
            1,
            parent,
            menu,
            instance,
            None,
        )
    }
    .map_err(|error| format!("无法创建设置控件：{error}"))
}

fn apply_layout(state: &RefCell<SettingsUiState>, dpi: u32) {
    let _ = replace_font(state, dpi);
    layout_controls(state, dpi);
}

fn set_control_rect(
    control: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    dpi: u32,
) -> Result<(), WindowsError> {
    if control.is_invalid() {
        return Ok(());
    }
    unsafe {
        MoveWindow(
            control,
            scaled(x, dpi),
            scaled(y, dpi),
            scaled(width, dpi),
            scaled(height, dpi),
            TRUE,
        )
    }
}

fn layout_controls(state: &RefCell<SettingsUiState>, dpi: u32) {
    let Ok(controls) = state.try_borrow().map(|state| state.controls) else {
        return;
    };
    // Win32 is called only after the state borrow has ended.
    for (control, rect) in control_layout(controls) {
        if !control.is_invalid() {
            if controls.scroll_x == 0 && controls.scroll_y == 0 {
                let _ = set_control_rect(control, rect[0], rect[1], rect[2], rect[3], dpi);
            } else {
                let _ = unsafe {
                    MoveWindow(
                        control,
                        scaled(rect[0], dpi) - controls.scroll_x,
                        scaled(rect[1], dpi) - controls.scroll_y,
                        scaled(rect[2], dpi),
                        scaled(rect[3], dpi),
                        TRUE,
                    )
                };
            }
        }
    }
}

fn ensure_settings_focus_visible(hwnd: HWND, state: &RefCell<SettingsUiState>) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetWindowRect, IsChild, IsWindowVisible, WM_SIZE,
    };
    if !window_is_owned(hwnd, state) {
        return;
    }
    let focused = unsafe { GetFocus() };
    if !unsafe { IsChild(hwnd, focused) }.as_bool()
        || !unsafe { IsWindowVisible(focused) }.as_bool()
    {
        return;
    }
    let mut rect = RECT::default();
    let mut client = RECT::default();
    if unsafe { GetWindowRect(focused, &mut rect) }.is_err()
        || unsafe { GetClientRect(hwnd, &mut client) }.is_err()
    {
        return;
    }
    let mut origin = POINT {
        x: rect.left,
        y: rect.top,
    };
    if !unsafe { ScreenToClient(hwnd, &mut origin) }.as_bool() {
        return;
    }
    let margin = scaled(8, unsafe { GetDpiForWindow(hwnd) }.max(BASE_DPI));
    let delta = |start: i32, length: i32, extent: i32| {
        if start < margin {
            start - margin
        } else if start + length > extent - margin {
            start + length - extent + margin
        } else {
            0
        }
    };
    let dx = delta(origin.x, rect.right - rect.left, client.right);
    let dy = delta(origin.y, rect.bottom - rect.top, client.bottom);
    if dx == 0 && dy == 0 {
        return;
    }
    {
        let Ok(mut state) = state.try_borrow_mut() else {
            return;
        };
        state.controls.scroll_x = state.controls.scroll_x.saturating_add(dx);
        state.controls.scroll_y = state.controls.scroll_y.saturating_add(dy);
    }
    // Clamp/update both scrollbars and move children only after releasing state.
    scroll_settings(hwnd, state, WM_SIZE, WPARAM(0));
}

fn scroll_settings(hwnd: HWND, state: &RefCell<SettingsUiState>, message: u32, wparam: WPARAM) {
    use windows::Win32::UI::WindowsAndMessaging::*;
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(BASE_DPI);
    for (bar, extent, page, horizontal) in [
        (SB_HORZ, scaled(CLIENT_WIDTH, dpi), client.right, true),
        (SB_VERT, scaled(CLIENT_HEIGHT, dpi), client.bottom, false),
    ] {
        let old = state.borrow().controls;
        let mut position = if horizontal {
            old.scroll_x
        } else {
            old.scroll_y
        };
        let relevant = if horizontal { WM_HSCROLL } else { WM_VSCROLL };
        if message == relevant {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_TRACKPOS,
                ..Default::default()
            };
            unsafe {
                let _ = GetScrollInfo(hwnd, bar, &mut info);
            }
            position = match (wparam.0 & 0xffff) as u32 {
                0 => position - scaled(32, dpi),
                1 => position + scaled(32, dpi),
                2 => position - page,
                3 => position + page,
                4 | 5 => info.nTrackPos,
                6 => 0,
                7 => extent,
                _ => position,
            };
        }
        position = position.clamp(0, (extent - page).max(0));
        {
            let mut s = state.borrow_mut();
            if horizontal {
                s.controls.scroll_x = position;
            } else {
                s.controls.scroll_y = position;
            }
        }
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: extent - 1,
            nPage: page.max(1) as u32,
            nPos: position,
            ..Default::default()
        };
        unsafe {
            windows::Win32::UI::Controls::SetScrollInfo(hwnd, bar, &info, TRUE);
        }
    }
    layout_controls(state, dpi);
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, TRUE);
    }
}

fn control_layout(c: SettingsControls) -> Vec<(HWND, [i32; 4])> {
    vec![
        (c.categories[0], [28, 104, 176, 32]),
        (c.categories[1], [212, 104, 152, 32]),
        (c.categories[2], [392, 104, 152, 32]),
        (c.categories[3], [556, 104, 176, 32]),
        (c.shortcut_labels[0], [48, 176, 100, 28]),
        (c.shortcut_labels[1], [48, 224, 100, 28]),
        (c.shortcuts[0], [164, 172, 280, 36]),
        (c.shortcuts[1], [164, 220, 280, 36]),
        (c.reset_shortcuts, [480, 220, 224, 36]),
        (c.shortcut_hint, [48, 296, 656, 112]),
        (c.online_settings, [364, 264, 300, 40]),
        (c.model_api_settings, [48, 264, 280, 40]),
        (c.headings[2], [48, 172, 292, 28]),
        (c.headings[3], [48, 172, 300, 28]),
        (c.headings[4], [48, 172, 300, 28]),
        (c.headings[5], [412, 172, 300, 28]),
        (c.labels[0], [48, 224, 292, 24]),
        (c.page_edit, [48, 256, 80, 32]),
        (c.labels[1], [140, 260, 200, 24]),
        (c.labels[2], [48, 320, 280, 24]),
        (c.theme_combo, [48, 352, 280, 100]),
        (c.labels[3], [412, 320, 280, 24]),
        (c.script_combo, [412, 352, 280, 100]),
        (c.online_ai, [48, 216, 280, 28]),
        (c.online_ai_hint, [348, 212, 356, 44]),
        (c.ai_correction, [48, 324, 300, 28]),
        (c.ai_scope_compatibility, [48, 360, 300, 28]),
        (c.ai_correction_hint, [364, 324, 348, 72]),
        (c.restore_send_confirmation, [364, 404, 300, 40]),
        (c.learning, [48, 216, 320, 28]),
        (c.learning_hint, [48, 260, 320, 80]),
        (c.clear_learning, [48, 380, 224, 36]),
        (c.domain_combo, [412, 216, 300, 144]),
        (c.domain_enabled, [412, 260, 300, 28]),
        (c.dictionary_status, [412, 304, 300, 120]),
        (c.notice, [28, 480, 456, 56]),
        (c.cancel_button, [508, 480, 104, 40]),
        (c.save_button, [628, 480, 104, 40]),
    ]
    .into_iter()
    .map(|(hwnd, mut rect)| {
        rect[1] -= 80;
        (hwnd, rect)
    })
    .collect()
}

fn replace_font(state: &RefCell<SettingsUiState>, dpi: u32) -> Result<(), WindowsError> {
    let old_count = state
        .try_borrow()
        .map(|state| state.fonts.len())
        .unwrap_or(0);
    let font = create_ui_font(dpi)?;
    let controls = match state.try_borrow_mut() {
        Ok(mut state) => {
            state.fonts.push(font);
            state.controls
        }
        Err(_) => {
            unsafe {
                let _ = DeleteObject(HGDIOBJ::from(font));
            }
            return Err(WindowsError::from_hresult(
                windows::Win32::Foundation::E_UNEXPECTED,
            ));
        }
    };
    for control in [
        controls.categories[0],
        controls.categories[1],
        controls.categories[2],
        controls.categories[3],
        controls.shortcuts[0],
        controls.shortcuts[1],
        controls.shortcut_labels[0],
        controls.shortcut_labels[1],
        controls.reset_shortcuts,
        controls.shortcut_hint,
        controls.online_settings,
        controls.online_ai,
        controls.online_ai_hint,
        controls.model_api_settings,
        controls.restore_send_confirmation,
        controls.labels[0],
        controls.page_edit,
        controls.labels[1],
        controls.labels[2],
        controls.labels[3],
        controls.theme_combo,
        controls.script_combo,
        controls.notice,
        controls.save_button,
        controls.cancel_button,
        controls.learning,
        controls.clear_learning,
        controls.domain_combo,
        controls.domain_enabled,
        controls.dictionary_status,
        controls.ai_correction,
        controls.ai_scope_compatibility,
        controls.ai_correction_hint,
        controls.learning_hint,
    ] {
        if !control.is_invalid() {
            unsafe {
                SendMessageW(
                    control,
                    windows::Win32::UI::WindowsAndMessaging::WM_SETFONT,
                    WPARAM(font.0 as usize),
                    LPARAM(TRUE.0 as isize),
                );
            }
        }
    }
    for (index, control) in controls.headings.into_iter().enumerate() {
        let heading = create_sized_font(
            dpi,
            if index == 0 {
                24
            } else if index == 1 {
                13
            } else {
                18
            },
        )?;
        if let Ok(mut current) = state.try_borrow_mut() {
            current.fonts.push(heading);
        } else {
            let _ = unsafe { DeleteObject(HGDIOBJ::from(heading)) };
            continue;
        }
        unsafe {
            SendMessageW(
                control,
                windows::Win32::UI::WindowsAndMessaging::WM_SETFONT,
                WPARAM(heading.0 as usize),
                LPARAM(1),
            );
        }
    }
    let hint_font = create_sized_font(dpi, 13)?;
    if let Ok(mut current) = state.try_borrow_mut() {
        current.fonts.push(hint_font);
    } else {
        let _ = unsafe { DeleteObject(HGDIOBJ::from(hint_font)) };
        return Ok(());
    }
    for control in [
        controls.labels[1],
        controls.notice,
        controls.ai_correction_hint,
        controls.learning_hint,
        controls.dictionary_status,
    ] {
        unsafe {
            SendMessageW(
                control,
                windows::Win32::UI::WindowsAndMessaging::WM_SETFONT,
                WPARAM(hint_font.0 as usize),
                LPARAM(1),
            );
        }
    }
    let retired: Vec<_> = state
        .try_borrow_mut()
        .ok()
        .map(|mut state| state.fonts.drain(..old_count).collect())
        .unwrap_or_default();
    for font in retired {
        let _ = unsafe { DeleteObject(HGDIOBJ::from(font)) };
    }
    Ok(())
}

fn create_ui_font(dpi: u32) -> Result<HFONT, WindowsError> {
    create_sized_font(dpi, 14)
}

fn create_sized_font(dpi: u32, size: i32) -> Result<HFONT, WindowsError> {
    let height = -scaled(size, dpi);
    let font = unsafe {
        CreateFontW(
            height,
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
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            FONT_FACE,
        )
    };
    if font.is_invalid() {
        Err(WindowsError::from_win32())
    } else {
        Ok(font)
    }
}

fn delete_all_fonts(state: &RefCell<SettingsUiState>) {
    let fonts = state
        .try_borrow_mut()
        .ok()
        .map(|mut state| std::mem::take(&mut state.fonts))
        .unwrap_or_default();
    for font in fonts {
        if font.is_invalid() {
            continue;
        }
        unsafe {
            let _ = DeleteObject(HGDIOBJ::from(font));
        }
    }
}

fn theme_dropdown_open(state: &RefCell<SettingsUiState>) -> bool {
    let Ok(controls) = state.try_borrow().map(|state| state.controls) else {
        return false;
    };
    [
        controls.shortcuts[0],
        controls.shortcuts[1],
        controls.theme_combo,
        controls.domain_combo,
        controls.script_combo,
    ]
    .iter()
    .any(|&combo| {
        !combo.is_invalid()
            && unsafe {
                SendMessageW(
                    combo,
                    CB_GETDROPPEDSTATE,
                    WPARAM::default(),
                    LPARAM::default(),
                )
            }
            .0 != 0
    })
}

fn selected_domain(state: &RefCell<SettingsUiState>, combo: HWND) -> Option<String> {
    let selected = unsafe {
        SendMessageW(
            combo,
            windows::Win32::UI::WindowsAndMessaging::CB_GETCURSEL,
            WPARAM::default(),
            LPARAM::default(),
        )
    }
    .0;
    usize::try_from(selected)
        .ok()
        .and_then(|index| state.try_borrow().ok()?.domain_names.get(index).cloned())
}

fn set_checked(control: HWND, checked: bool) {
    unsafe {
        SendMessageW(
            control,
            windows::Win32::UI::WindowsAndMessaging::BM_SETCHECK,
            WPARAM(usize::from(checked)),
            LPARAM::default(),
        );
    }
}

fn is_checked(control: HWND) -> bool {
    unsafe {
        SendMessageW(
            control,
            windows::Win32::UI::WindowsAndMessaging::BM_GETCHECK,
            WPARAM::default(),
            LPARAM::default(),
        )
    }
    .0 == 1
}

/// Re-read rather than treating a failed load as an empty/default dictionary.
/// No RefCell borrow is held across a Win32 call (which can re-enter WndProc).
fn refresh_dictionary(hwnd: HWND, state: &RefCell<SettingsUiState>) {
    let store = state.borrow().dictionary_store.clone();
    render_dictionary(hwnd, state, store.load().map_err(|error| error.to_string()));
}

fn start_initial_dictionary(hwnd: HWND, state: &RefCell<SettingsUiState>) {
    if state.borrow().initial_started || state.borrow().controls.category != 3 {
        return;
    }
    state.borrow_mut().initial_started = true;
    // Flush child controls too before allowing the worker to start disk I/O.
    for control in {
        let c = state.borrow().controls;
        [c.dictionary_status, c.learning, c.clear_learning]
    } {
        unsafe {
            let _ = UpdateWindow(control);
        }
    }
    let timer = unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetTimer(hwnd, DICTIONARY_TIMER, 30, None)
    };
    if timer == 0 {
        render_dictionary(hwnd, state, Err("无法创建词典读取定时器".into()));
        return;
    }
    let store = state.borrow().dictionary_store.clone();
    #[cfg(test)]
    let injected = state.borrow_mut().initial_loader.take();
    let load = move || {
        #[cfg(test)]
        if let Some(load) = injected {
            return load();
        }
        store.load().map_err(|error| error.to_string())
    };
    match InitialDictionaryLoad::start(load) {
        Ok(worker) => state.borrow_mut().initial_dictionary = Some(worker),
        Err(error) => {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, DICTIONARY_TIMER);
            }
            render_dictionary(hwnd, state, Err(error));
        }
    }
}

fn poll_initial_dictionary(hwnd: HWND, state: &RefCell<SettingsUiState>) {
    if !window_is_owned(hwnd, state) {
        return;
    }
    let result = state
        .borrow()
        .initial_dictionary
        .as_ref()
        .map(|load| load.result.try_recv());
    let result = match result {
        Some(Ok(result)) => result,
        Some(Err(mpsc::TryRecvError::Disconnected)) => Err("词典读取线程已停止".into()),
        _ => return,
    };
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, DICTIONARY_TIMER);
    }
    state.borrow_mut().initial_dictionary.take();
    render_dictionary(hwnd, state, result);
}

fn render_dictionary(hwnd: HWND, state: &RefCell<SettingsUiState>, result: DictionaryResult) {
    let Ok(controls) = state.try_borrow().map(|state| state.controls) else {
        return;
    };
    let selected = selected_domain(state, controls.domain_combo);
    state.borrow_mut().dictionary_ready = result.is_ok();
    match result {
        Ok(data) => {
            let names: Vec<_> = data.domains.iter().map(|item| item.name.clone()).collect();
            let selected_index = selected
                .as_ref()
                .and_then(|name| names.iter().position(|item| item == name))
                .unwrap_or(0);
            if let Ok(mut state) = state.try_borrow_mut() {
                state.domain_names = names;
            }
            unsafe {
                SendMessageW(
                    controls.domain_combo,
                    windows::Win32::UI::WindowsAndMessaging::CB_RESETCONTENT,
                    WPARAM::default(),
                    LPARAM::default(),
                );
            }
            for item in &data.domains {
                let caption = wide_null(&format!("{}（{} 条）", item.name, item.entries.len()));
                unsafe {
                    SendMessageW(
                        controls.domain_combo,
                        CB_ADDSTRING,
                        WPARAM::default(),
                        LPARAM(caption.as_ptr() as isize),
                    );
                }
            }
            if data.domains.is_empty() {
                unsafe {
                    SendMessageW(
                        controls.domain_combo,
                        CB_ADDSTRING,
                        WPARAM(0),
                        LPARAM(windows::core::w!("暂无专业词库").as_ptr() as isize),
                    );
                }
            }
            unsafe {
                SendMessageW(
                    controls.domain_combo,
                    CB_SETCURSEL,
                    WPARAM(selected_index),
                    LPARAM::default(),
                );
            }
            set_checked(controls.learning, data.adaptive_ranking_enabled);
            set_checked(
                controls.domain_enabled,
                data.domains
                    .get(selected_index)
                    .is_some_and(|item| item.enabled),
            );
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    controls.learning,
                    TRUE,
                );
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    controls.clear_learning,
                    TRUE,
                );
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    controls.domain_combo,
                    windows::Win32::Foundation::BOOL::from(!data.domains.is_empty()),
                );
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    controls.domain_enabled,
                    windows::Win32::Foundation::BOOL::from(!data.domains.is_empty()),
                );
            }
            let status = format!(
                "手工 {} · 自动词 {} · 排序 {} · 词库 {}\n选中词库：{}。立即保存，切换输入法生效。\n清空删除自动词，保留手工/专业词条。",
                data.entries.len(), data.auto_phrases.len(), data.rankings.len(), data.domains.len(),
                data.domains.get(selected_index).map(|item| if item.enabled { "已启用" } else { "已停用" }).unwrap_or("无"));
            let _ = set_control_text(hwnd, DICTIONARY_STATUS_ID, &status);
        }
        Err(error) => {
            if let Ok(mut current) = state.try_borrow_mut() {
                current.domain_names.clear();
            }
            unsafe {
                SendMessageW(
                    controls.domain_combo,
                    windows::Win32::UI::WindowsAndMessaging::CB_RESETCONTENT,
                    WPARAM(0),
                    LPARAM(0),
                );
                SendMessageW(
                    controls.domain_combo,
                    CB_ADDSTRING,
                    WPARAM(0),
                    LPARAM(windows::core::w!("词典读取失败").as_ptr() as isize),
                );
                SendMessageW(controls.domain_combo, CB_SETCURSEL, WPARAM(0), LPARAM(0));
            }
            set_checked(controls.learning, false);
            set_checked(controls.domain_enabled, false);
            for control in [
                controls.learning,
                controls.clear_learning,
                controls.domain_combo,
                controls.domain_enabled,
            ] {
                let _ = unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(control, FALSE)
                };
            }
            let _ = set_control_text(hwnd, DICTIONARY_STATUS_ID,
                &format!("用户词典读取失败：{error}\n未重建或覆盖文件；词典操作不可用。请修复文件后重新打开设置。"));
        }
    }
}

fn dictionary_action(hwnd: HWND, state: &RefCell<SettingsUiState>, id: i32) {
    if !state.try_borrow().is_ok_and(|state| state.dictionary_ready) {
        return;
    }
    let Some(_guard) = SaveGuard::acquire(state) else {
        return;
    };
    let Ok(controls) = state.try_borrow().map(|state| state.controls) else {
        return;
    };
    // Capture the explicit checkbox action before refreshing from disk.
    let checked = is_checked(if id == LEARNING_ID {
        controls.learning
    } else {
        controls.domain_enabled
    });
    let name = selected_domain(state, controls.domain_combo);
    if id == CLEAR_LEARNING_ID
        && (message_box(
            hwnd,
            "确定清空全部本地学习记录和自动记忆词语？\n\n保留手工个人词条和专业词库。此操作不可撤销。",
            "Flower 设置",
            MB_YESNO,
        ) != IDYES
            || !window_is_owned(hwnd, state))
    {
        return;
    }
    let result = (|| -> Result<(), String> {
        let store = state
            .try_borrow()
            .map_err(|_| "词典状态正忙".to_owned())?
            .dictionary_store
            .clone();
        // Fail closed on corruption. The store also validates again under its write lock.
        store.load().map_err(|e| e.to_string())?;
        match id {
            LEARNING_ID => store.set_learning_enabled(checked),
            CLEAR_LEARNING_ID => store.clear_learning(),
            DOMAIN_ENABLED_ID => store.set_domain_enabled(
                name.as_deref()
                    .ok_or_else(|| "请先导入并选择一个专业词库。".to_owned())?,
                checked,
            ),
            _ => return Ok(()),
        }
        .map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        message_box(
            hwnd,
            &format!("词典操作失败：{error}\n未使用默认词典覆盖文件。"),
            "Flower 设置",
            MB_OK | MB_ICONERROR,
        );
    }
    if window_is_owned(hwnd, state) {
        refresh_dictionary(hwnd, state);
    }
}

fn save_settings(hwnd: HWND, state: &RefCell<SettingsUiState>) {
    let Some(_saving_guard) = SaveGuard::acquire(state) else {
        return;
    };

    let mut settings = match read_form(state) {
        Ok(settings) => settings,
        Err(error) => {
            message_box(hwnd, &error, "Flower 设置", MB_OK | MB_ICONERROR);
            if let Ok(page_edit) = state.try_borrow().map(|state| state.controls.page_edit) {
                if !page_edit.is_invalid() && window_is_owned(hwnd, state) {
                    let _ = unsafe { SetFocus(page_edit) };
                }
            }
            return;
        }
    };

    let Some(store) = state.try_borrow().ok().map(|state| state.store.clone()) else {
        return;
    };
    let saved = save_with_latest_consent(&store, &mut settings);
    if let Err(error) = saved {
        let message = format!(
            "保存设置失败：{error}\n\n路径：{}\n\n原设置文件未被替换，\
             请检查目录权限后重试。",
            store.path().display()
        );
        message_box(hwnd, &message, "Flower 设置", MB_OK | MB_ICONERROR);
        return;
    }

    if let Ok(mut state) = state.try_borrow_mut() {
        state.settings = settings;
    }
    if window_is_owned(hwnd, state) {
        message_box(
            hwnd,
            "设置已保存。\n\n在线AI总开关立即生效，无需切换输入法。\n模型、快捷键及其他设置仍需重新切换 Flower。",
            "Flower 设置",
            MB_OK,
        );
        // The confirmation dialog pumped messages. Re-check ownership before
        // destroying a handle that may no longer belong to this window.
        if window_is_owned(hwnd, state) {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
    }
}

fn save_with_latest_consent(
    store: &SettingsStore,
    settings: &mut Settings,
) -> Result<(), crate::settings::SettingsError> {
    let latest = store.load()?;
    settings.writing_send_consent = latest.writing_send_consent;
    settings.model_api = latest.model_api;
    settings.online_polish = latest.online_polish;
    store.save(settings)
}

#[test]
#[ignore = "isolated file store; explicit session scratch directory required"]
fn save_form_preserves_latest_send_consent() {
    let directory = std::path::PathBuf::from(std::env::var_os("FLOWER_CONSENT_TEST_DIR").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    let store = SettingsStore::new(directory.join("stale-form.json"));
    let mut stale = Settings::default();
    store.save(&stale).unwrap();
    crate::writing::consent::remember(&store, true).unwrap();
    let mut latest = store.load().unwrap();
    latest.model_api.profiles[0].model = "owned-model".into();
    latest.online_polish.style = crate::writing::PolishStyle::Formal;
    store.save(&latest).unwrap();
    stale.writing_shortcuts.draft.key = 0x34;
    stale.online_polish.enabled = true;
    stale.online_ai_enabled = true;
    save_with_latest_consent(&store, &mut stale).unwrap();
    let actual = store.load().unwrap();
    assert_eq!(actual, stale);
    assert_eq!(actual.model_api, latest.model_api);
    assert_eq!(actual.online_polish, latest.online_polish);
    assert!(actual.online_ai_enabled);
    assert_eq!(
        actual.writing_send_consent.as_deref(),
        Some(crate::writing::consent::SCOPE)
    );
    crate::writing::consent::remember(&store, false).unwrap();
    stale.online_ai_enabled = false;
    save_with_latest_consent(&store, &mut stale).unwrap();
    let actual = store.load().unwrap();
    assert_eq!(actual.writing_send_consent, None);
    assert!(!actual.online_ai_enabled);
    assert_eq!(actual.model_api, latest.model_api);
    assert_eq!(actual.online_polish, latest.online_polish);
}

fn read_form(state: &RefCell<SettingsUiState>) -> Result<Settings, String> {
    let controls = state
        .try_borrow()
        .map(|state| state.controls)
        .map_err(|_| "设置窗口状态正忙，无法读取设置".to_owned())?;
    let page_text = read_control_text(controls.page_edit)?;
    let page_size = parse_candidate_page_size_input(&page_text).ok_or_else(|| {
        format!(
            "候选页大小必须是 {MIN_CANDIDATE_PAGE_SIZE} 到 {MAX_CANDIDATE_PAGE_SIZE} 之间的整数。"
        )
    })?;
    let selected = unsafe {
        SendMessageW(
            controls.theme_combo,
            windows::Win32::UI::WindowsAndMessaging::CB_GETCURSEL,
            WPARAM::default(),
            LPARAM::default(),
        )
    }
    .0;
    let theme = match selected {
        0 => Theme::System,
        1 => Theme::Light,
        2 => Theme::Dark,
        _ => return Err("请选择候选窗主题。".to_owned()),
    };
    let settings = Settings {
        online_ai_enabled: is_checked(controls.online_ai),
        candidate_layout_version: 1,
        writing_send_consent: state.borrow().settings.writing_send_consent.clone(),
        writing_shortcuts: read_shortcuts(controls)?,
        online_polish: state.borrow().settings.online_polish.clone(),
        model_api: state.borrow().settings.model_api.clone(),
        chinese_script: match unsafe {
            SendMessageW(
                controls.script_combo,
                windows::Win32::UI::WindowsAndMessaging::CB_GETCURSEL,
                WPARAM::default(),
                LPARAM::default(),
            )
        }
        .0
        {
            0 => crate::ChineseScript::Simplified,
            1 => crate::ChineseScript::Traditional,
            _ => return Err("请选择输出字形。".to_owned()),
        },
        candidate_page_size: page_size,
        theme,
        ai_correction_enabled: is_checked(controls.ai_correction),
        ai_undeclared_scope_compatibility: is_checked(controls.ai_scope_compatibility),
    };
    settings
        .validate()
        .map_err(|error| format!("设置值无效：{error}"))?;
    Ok(settings)
}

fn read_shortcuts(controls: SettingsControls) -> Result<WritingShortcuts, String> {
    let selected: Vec<_> = controls
        .shortcuts
        .into_iter()
        .map(|control| {
            let raw = unsafe { GetWindowLongPtrW(control, GWLP_USERDATA) };
            WritingShortcut::new((raw & 0xffff) as u16, ((raw >> 16) & 0xff) as u8)
        })
        .collect();
    let shortcuts = WritingShortcuts {
        draft: selected[0],
        polish: selected[1],
    };
    if !shortcuts.is_valid() {
        return Err("帮写与润色不能使用相同快捷键，且不能使用保留组合。".into());
    }
    Ok(shortcuts)
}

fn read_control_text(control: HWND) -> Result<String, String> {
    if control.is_invalid() {
        return Err("无法读取候选页大小输入框。".to_owned());
    }
    let length = unsafe { GetWindowTextLengthW(control) };
    if length <= 0 {
        return Err(format!(
            "请输入 {MIN_CANDIDATE_PAGE_SIZE} 到 {MAX_CANDIDATE_PAGE_SIZE} 之间的候选页大小。"
        ));
    }
    if length > 64 {
        return Err("候选页大小输入过长，请输入 1 到 99 之间的整数。".to_owned());
    }

    let mut buffer = vec![0u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(control, &mut buffer) };
    if copied <= 0 {
        return Err(format!(
            "请输入 {MIN_CANDIDATE_PAGE_SIZE} 到 {MAX_CANDIDATE_PAGE_SIZE} 之间的候选页大小。"
        ));
    }
    buffer.truncate(copied as usize);
    String::from_utf16(&buffer).map_err(|_| "候选页大小包含无效字符。".to_owned())
}

struct SaveGuard<'a> {
    state: &'a RefCell<SettingsUiState>,
}

impl<'a> SaveGuard<'a> {
    fn acquire(state: &'a RefCell<SettingsUiState>) -> Option<Self> {
        let mut current = state.try_borrow_mut().ok()?;
        if current.saving {
            None
        } else {
            current.saving = true;
            let button = current.controls.save_button;
            drop(current);
            if !button.is_invalid() {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                        button,
                        windows::core::w!("保存中…"),
                    );
                    let _ =
                        windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(button, FALSE);
                }
            }
            Some(Self { state })
        }
    }
}

impl Drop for SaveGuard<'_> {
    fn drop(&mut self) {
        let button = if let Ok(mut state) = self.state.try_borrow_mut() {
            state.saving = false;
            state.controls.save_button
        } else {
            return;
        };
        let parent = unsafe { windows::Win32::UI::WindowsAndMessaging::GetParent(button) }
            .unwrap_or_default();
        if !button.is_invalid() && window_is_owned(parent, self.state) {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                    button,
                    windows::core::w!("保存"),
                );
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(button, TRUE);
            }
        }
    }
}

fn theme_index(theme: Theme) -> usize {
    match theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}

fn set_control_text(hwnd: HWND, control_id: i32, text: &str) -> Result<(), String> {
    let text = wide_null(text);
    unsafe { SetDlgItemTextW(hwnd, control_id, PCWSTR::from_raw(text.as_ptr())) }
        .map_err(|error| format!("无法设置控件文本：{error}"))
}

fn message_box(
    owner: HWND,
    message: &str,
    caption: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let message = wide_null(message);
    let caption = wide_null(caption);
    unsafe {
        MessageBoxW(
            owner,
            PCWSTR::from_raw(message.as_ptr()),
            PCWSTR::from_raw(caption.as_ptr()),
            style,
        )
    }
}

fn state_pointer(state: &RefCell<SettingsUiState>) -> *const c_void {
    (state as *const RefCell<SettingsUiState>).cast::<c_void>()
}

fn settings_state_pointer(hwnd: HWND) -> *mut RefCell<SettingsUiState> {
    unsafe {
        GetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA)
            as *mut RefCell<SettingsUiState>
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowCleanup {
    AlreadyGone,
    Destroyed,
    DestroyedFailed,
    DetachFailed,
    UnattachedLiveWindow,
}

fn window_is_owned(hwnd: HWND, state: &RefCell<SettingsUiState>) -> bool {
    if hwnd.is_invalid() {
        return false;
    }
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return false;
    }
    let expected = state_pointer(state) as isize;
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) == expected }
}

fn cleanup_window(hwnd: HWND, state: &RefCell<SettingsUiState>) -> WindowCleanup {
    if hwnd.is_invalid() {
        return WindowCleanup::AlreadyGone;
    }
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return WindowCleanup::AlreadyGone;
    }

    let expected = state_pointer(state) as isize;
    let current = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if current != expected {
        return WindowCleanup::UnattachedLiveWindow;
    }

    let previous = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
    if previous != expected {
        return WindowCleanup::DetachFailed;
    }
    state.borrow_mut().dictionary_ready = false;
    state.borrow_mut().initial_dictionary.take();

    match unsafe { DestroyWindow(hwnd) } {
        Ok(()) => WindowCleanup::Destroyed,
        Err(_) => WindowCleanup::DestroyedFailed,
    }
}

fn wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn scaled(value: i32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi.min(480) };
    ((i64::from(value) * i64::from(dpi) + i64::from(BASE_DPI / 2)) / i64::from(BASE_DPI)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // The library test executable has no settings-only manifest. Activate a
    // private v6 context on this fixture thread, never in the production DLL.
    // Raw declarations keep this test from adding a new Cargo feature.
    #[repr(C)]
    struct TestActCtx {
        size: u32,
        flags: u32,
        source: *const u16,
        architecture: u16,
        language: u16,
        assembly_directory: *const u16,
        resource: *const u16,
        application: *const u16,
        module: *mut c_void,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateActCtxW(context: *const TestActCtx) -> *mut c_void;
        fn ActivateActCtx(context: *mut c_void, cookie: *mut usize) -> i32;
        fn DeactivateActCtx(flags: u32, cookie: usize) -> i32;
        fn ReleaseActCtx(context: *mut c_void);
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }

    struct TestVisualStyles {
        context: *mut c_void,
        cookie: usize,
        controls_module: *mut c_void,
    }

    impl TestVisualStyles {
        fn activate(directory: &Path) -> Self {
            let path = directory.join("fixture-v6.manifest");
            std::fs::write(&path, r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<assemblyIdentity name="Flower.SettingsFixture" version="1.0.0.0" type="win32"/>
<dependency><dependentAssembly>
<assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
</dependentAssembly></dependency></assembly>"#).unwrap();
            let source = wide_null(path.to_str().unwrap());
            let description = TestActCtx {
                size: std::mem::size_of::<TestActCtx>() as u32,
                flags: 0,
                source: source.as_ptr(),
                architecture: 0,
                language: 0,
                assembly_directory: std::ptr::null(),
                resource: std::ptr::null(),
                application: std::ptr::null(),
                module: std::ptr::null_mut(),
            };
            let context = unsafe { CreateActCtxW(&description) };
            assert_ne!(context as isize, -1, "fixture v6 context creation failed");
            let mut cookie = 0;
            let activated = unsafe { ActivateActCtx(context, &mut cookie) };
            if activated == 0 {
                unsafe { ReleaseActCtx(context) };
                panic!("fixture v6 context activation failed");
            }
            // The test executable's import table already bound ComCtl32 v5
            // before this thread activated v6. Resolve v6's export inside the
            // private context instead of testing that unrelated v5 import.
            let controls_module =
                unsafe { LoadLibraryW(windows::core::w!("comctl32.dll").as_ptr()) };
            assert!(!controls_module.is_null());
            let styles = Self {
                context,
                cookie,
                controls_module,
            };
            styles.initialize_controls();
            styles
        }

        fn initialize_controls(&self) {
            use windows::Win32::UI::Controls::{ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX};
            let address =
                unsafe { GetProcAddress(self.controls_module, b"InitCommonControlsEx\0".as_ptr()) };
            assert!(!address.is_null());
            let initialize: unsafe extern "system" fn(*const INITCOMMONCONTROLSEX) -> i32 =
                unsafe { std::mem::transmute(address) };
            let controls = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_STANDARD_CLASSES,
            };
            assert_ne!(unsafe { initialize(&controls) }, 0);
        }
    }

    impl Drop for TestVisualStyles {
        fn drop(&mut self) {
            unsafe {
                let _ = FreeLibrary(self.controls_module);
                let _ = DeactivateActCtx(0, self.cookie);
                ReleaseActCtx(self.context);
            }
        }
    }

    fn test_state(directory: &Path) -> RefCell<SettingsUiState> {
        RefCell::new(SettingsUiState {
            store: SettingsStore::new(directory.join("settings.json")),
            dictionary_store: DictionaryStore::new(directory.join("dictionary.json")),
            settings: Settings::default(),
            controls: SettingsControls::default(),
            fonts: Vec::new(),
            saving: false,
            creation_error: None,
            domain_names: Vec::new(),
            dictionary_ready: false,
            initial_dictionary: None,
            initial_started: false,
            initial_loader: None,
        })
    }

    #[test]
    fn settings_initial_loader_cancel_joins_and_discards_late_result() {
        let (entered, ready) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut load = InitialDictionaryLoad::start(move || {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            Ok(UserDictionary::default())
        })
        .unwrap();
        ready.recv().unwrap();
        load.cancelled.store(true, Ordering::Release);
        release.send(()).unwrap();
        load.worker.take().unwrap().join().unwrap();
        assert!(matches!(
            load.result.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        drop(load);
    }

    #[test]
    fn settings_initial_loader_reports_failure_without_default_data() {
        let mut load = InitialDictionaryLoad::start(|| Err("fixture failure".into())).unwrap();
        load.worker.take().unwrap().join().unwrap();
        assert_eq!(load.result.recv().unwrap().unwrap_err(), "fixture failure");
    }

    #[test]
    fn settings_layout_stays_in_client_at_supported_dpis() {
        for dpi in [96, 120, 144, 192, 240, 480] {
            for (_, [x, y, width, height]) in control_layout(SettingsControls::default()) {
                // COMBOBOX height includes the popup, not just its closed field.
                assert!(x >= 28 && y >= 24 && width > 0 && height > 0);
                assert!(scaled(x + width, dpi) <= scaled(CLIENT_WIDTH, dpi));
                assert!(scaled(y + height, dpi) <= scaled(CLIENT_HEIGHT, dpi));
            }
            for [x, y, width, height] in CARDS {
                assert!(scaled(x + width, dpi) <= scaled(CLIENT_WIDTH - 28, dpi));
                assert!(scaled(y + height, dpi) <= scaled(600, dpi));
            }
        }
    }

    #[test]
    fn settings_categories_have_consistent_card_and_spacing() {
        assert_eq!(CARDS, [[28, 72, 704, 304]]);
        // control_layout translates its authored y coordinates by -80.
        // Keep the online description inside the card and a clear footer gap.
        let card_bottom = CARDS[0][1] + CARDS[0][3];
        assert!(404 - 80 + 40 <= card_bottom - 12);
        assert!(card_bottom + 24 <= 480 - 80);
        assert_eq!(scaled(24, 144), 36);
        assert_eq!(scaled(24, 0), 24);
        assert_eq!(scaled(24, 960), 120);
        let c = SettingsControls::default();
        assert_eq!(category_controls(c).len(), 4);
        assert!(!WritingShortcut::new(0x46, 5).is_supported());
    }
    #[test]
    fn settings_save_guard_is_exclusive_and_releases_on_drop() {
        let state = test_state(Path::new("unused-settings-test"));
        let guard = SaveGuard::acquire(&state).unwrap();
        assert!(state.borrow().saving);
        assert!(SaveGuard::acquire(&state).is_none());
        drop(guard);
        assert!(!state.borrow().saving);
        assert!(SaveGuard::acquire(&state).is_some());
    }

    #[test]
    fn settings_state_retains_both_injected_paths() {
        let directory = Path::new("injected-settings-test");
        let state = test_state(directory);
        assert_eq!(state.borrow().store.path(), directory.join("settings.json"));
        assert_eq!(
            state.borrow().dictionary_store.path(),
            directory.join("dictionary.json")
        );
        assert_eq!(theme_index(Theme::System), 0);
        assert_eq!(theme_index(Theme::Light), 1);
        assert_eq!(theme_index(Theme::Dark), 2);
    }

    // Explicit opt-in only. The caller supplies a session-owned artifacts
    // directory. Neither store can resolve to a real user profile, and there
    // is no message loop, global input, save click, or interactive close wait.
    #[test]
    #[ignore = "requires FLOWER_SETTINGS_PREVIEW_DIR pointing to owned session artifacts"]
    fn settings_native_preview_fixture() {
        owned_settings_fixture(false);
    }

    #[test]
    #[ignore = "requires FLOWER_SETTINGS_PREVIEW_DIR pointing to owned session artifacts"]
    fn settings_owned_window_keyboard_route() {
        owned_settings_fixture(true);
    }

    fn owned_settings_fixture(check_route: bool) {
        static OWNED_WINDOW_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = OWNED_WINDOW_LOCK.lock().unwrap();
        let root = std::env::var_os("FLOWER_SETTINGS_PREVIEW_DIR")
            .expect("explicit session artifact directory required");
        let root = std::path::PathBuf::from(root);
        assert!(root.is_absolute(), "preview directory must be absolute");
        assert!(
            root.is_dir(),
            "caller must create the owned artifact directory"
        );
        let directory = root.join(format!(
            "settings-{}-{}",
            if check_route { "route" } else { "preview" },
            std::process::id()
        ));
        std::fs::create_dir(&directory).expect("preview must use a new exclusive directory");
        let _visual_styles = TestVisualStyles::activate(&directory);
        let state = Box::new(test_state(&directory));
        let instance = HINSTANCE::from(unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap());
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(settings_window_proc),
            hInstance: instance,
            lpszClassName: WINDOW_CLASS_NAME,
            ..Default::default()
        };
        assert_ne!(unsafe { RegisterClassExW(&class) }, 0);
        struct Preview {
            hwnd: HWND,
            instance: HINSTANCE,
            state: Box<RefCell<SettingsUiState>>,
        }
        impl Drop for Preview {
            fn drop(&mut self) {
                let outcome = cleanup_window(self.hwnd, self.state.as_ref());
                if matches!(
                    outcome,
                    WindowCleanup::Destroyed | WindowCleanup::AlreadyGone
                ) {
                    delete_all_fonts(self.state.as_ref());
                } else if matches!(
                    outcome,
                    WindowCleanup::DetachFailed | WindowCleanup::UnattachedLiveWindow
                ) {
                    let retained = std::mem::replace(
                        &mut self.state,
                        Box::new(test_state(Path::new("unused-retained-preview"))),
                    );
                    std::mem::forget(retained);
                }
                let _ = unsafe { UnregisterClassW(WINDOW_CLASS_NAME, self.instance) };
            }
        }
        let mut preview = Preview {
            hwnd: HWND::default(),
            instance,
            state,
        };
        let dpi = unsafe { GetDpiForSystem() }.max(BASE_DPI);
        let style = WS_OVERLAPPED
            | WS_CAPTION
            | WS_SYSMENU
            | WS_MINIMIZEBOX
            | windows::Win32::UI::WindowsAndMessaging::WS_SIZEBOX
            | windows::Win32::UI::WindowsAndMessaging::WS_HSCROLL
            | WS_VSCROLL;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scaled(CLIENT_WIDTH, dpi),
            bottom: scaled(CLIENT_HEIGHT, dpi),
        };
        unsafe { AdjustWindowRectExForDpi(&mut rect, style, FALSE, WS_EX_CONTROLPARENT, dpi) }
            .unwrap();
        preview.hwnd = unsafe {
            CreateWindowExW(
                WS_EX_CONTROLPARENT,
                WINDOW_CLASS_NAME,
                windows::core::w!("Flower · 原生设置预览"),
                style,
                32,
                32,
                rect.right - rect.left,
                rect.bottom - rect.top,
                HWND::default(),
                HMENU::default(),
                instance,
                Some(state_pointer(preview.state.as_ref())),
            )
        }
        .unwrap();
        let (release, blocked) = mpsc::channel();
        let fixture_store = preview.state.borrow().dictionary_store.clone();
        preview.state.borrow_mut().initial_loader = Some(Box::new(move || {
            blocked
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            fixture_store.load().map_err(|error| error.to_string())
        }));
        let first_paint = std::time::Instant::now();
        unsafe {
            let _ = ShowWindow(
                preview.hwnd,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE,
            );
            let _ = UpdateWindow(preview.hwnd);
        }
        assert!(
            unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(preview.hwnd) }
                .as_bool()
        );
        assert!(!preview.state.borrow().initial_started);
        assert!(!preview.state.borrow().dictionary_ready);
        for control in {
            let c = preview.state.borrow().controls;
            [
                c.learning,
                c.clear_learning,
                c.domain_combo,
                c.domain_enabled,
            ]
        } {
            assert!(!unsafe {
                windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(control)
            }
            .as_bool());
        }
        assert!(
            read_control_text(preview.state.borrow().controls.dictionary_status)
                .unwrap()
                .contains("正在读取")
        );
        for id in [LEARNING_ID, CLEAR_LEARNING_ID, DOMAIN_ENABLED_ID] {
            dictionary_action(preview.hwnd, preview.state.as_ref(), id);
        }
        assert!(!directory.join("dictionary.json").exists());
        write_preview_bitmap(preview.hwnd, &directory.join("settings-loading.bmp"));
        println!(
            "owned fixture first paint before release: {:?}",
            first_paint.elapsed()
        );
        let online_ai = preview.state.borrow().controls.online_ai;
        assert!(!is_checked(online_ai), "online AI must default to opt-out");
        set_checked(online_ai, true);
        release.send(()).unwrap();
        assert!(!preview.state.borrow().initial_started);
        show_category(preview.state.as_ref(), 3);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !preview.state.borrow().dictionary_ready {
            assert!(std::time::Instant::now() < deadline);
            let mut message = MSG::default();
            unsafe {
                while windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                    &mut message,
                    preview.hwnd,
                    0,
                    0,
                    windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
                )
                .as_bool()
                {
                    let _ = DispatchMessageW(&message);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            read_form(preview.state.as_ref()).unwrap().online_ai_enabled,
            "late dictionary load must not overwrite an edited online AI switch"
        );
        show_category(preview.state.as_ref(), 0);
        set_checked(online_ai, false);
        for (label, expected) in [
            (preview.state.borrow().controls.labels[2], "候选窗主题"),
            (preview.state.borrow().controls.labels[3], "输出字形"),
        ] {
            assert_eq!(read_control_text(label).unwrap(), expected);
            assert!(
                unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(label) }
                    .as_bool()
            );
        }
        if check_route {
            assert_owned_keyboard_route(preview.hwnd, preview.state.as_ref(), &directory);
            return;
        }
        // PrintWindow renders only this owned HWND, including its native child
        // controls. No screen or other application's DC is ever acquired.
        write_preview_bitmap(preview.hwnd, &directory.join("settings.bmp"));
        assert_eq!(
            read_form(preview.state.as_ref()).unwrap(),
            Settings::default()
        );
        assert_owned_shortcuts_and_categories(preview.hwnd, preview.state.as_ref(), &directory);
        // A saved modal value must survive subsequent main-form assembly.
        let online = crate::settings::OnlinePolishSettings {
            enabled: true,
            style: crate::writing::PolishStyle::Concise,
            script: Some(crate::ChineseScript::Traditional),
        };
        preview.state.borrow_mut().settings.online_polish = online.clone();
        assert_eq!(
            read_form(preview.state.as_ref()).unwrap().online_polish,
            online
        );
        assert!(preview.state.borrow().domain_names.is_empty());
        assert!(!directory.join("settings.json").exists());
        assert!(!directory.join("dictionary.json").exists());
        let compatibility = preview.state.borrow().controls.ai_scope_compatibility;
        assert!(is_checked(compatibility));
        set_checked(compatibility, false);
        let strict = read_form(preview.state.as_ref()).unwrap();
        assert!(!strict.ai_undeclared_scope_compatibility);
        assert!(strict.ai_correction_enabled);
        set_checked(compatibility, true);
        let original_font_count = preview.state.borrow().fonts.len();
        replace_font(preview.state.as_ref(), dpi).unwrap();
        assert_eq!(preview.state.borrow().fonts.len(), original_font_count);
        let guard = SaveGuard::acquire(preview.state.as_ref()).unwrap();
        let save_button = preview.state.borrow().controls.save_button;
        assert_eq!(read_control_text(save_button).unwrap(), "保存中…");
        drop(guard);
        assert_eq!(read_control_text(save_button).unwrap(), "保存");

        // The immediate-save action is exercised only against the injected
        // disposable dictionary; refresh must read it back, not a user path.
        let learning = preview.state.borrow().controls.learning;
        set_checked(learning, false);
        dictionary_action(preview.hwnd, preview.state.as_ref(), LEARNING_ID);
        let dictionary = preview.state.borrow().dictionary_store.clone();
        assert!(!dictionary.load().unwrap().adaptive_ranking_enabled);
        assert!(!is_checked(learning));
        std::fs::write(dictionary.path(), b"invalid fixture json").unwrap();
        refresh_dictionary(preview.hwnd, preview.state.as_ref());
        assert!(!is_checked(learning));
        assert!(
            !unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(learning) }
                .as_bool()
        );
        for id in [LEARNING_ID, CLEAR_LEARNING_ID, DOMAIN_ENABLED_ID] {
            dictionary_action(preview.hwnd, preview.state.as_ref(), id);
        }
        write_preview_bitmap(preview.hwnd, &directory.join("settings-error.bmp"));
        assert_eq!(
            std::fs::read(dictionary.path()).unwrap(),
            b"invalid fixture json"
        );
        assert!(!directory.join("settings.json").exists());
        // Re-enter the initial path with corrupt owned bytes. Poll the same
        // completion channel as production, rather than treating failure as
        // an empty dictionary.
        show_category(preview.state.as_ref(), 3);
        preview.state.borrow_mut().initial_started = false;
        start_initial_dictionary(preview.hwnd, preview.state.as_ref());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while preview.state.borrow().initial_dictionary.is_some() {
            assert!(std::time::Instant::now() < deadline);
            poll_initial_dictionary(preview.hwnd, preview.state.as_ref());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!preview.state.borrow().dictionary_ready);
        assert_eq!(
            std::fs::read(dictionary.path()).unwrap(),
            b"invalid fixture json"
        );

        // Destroy while the loader is inside a simulated blocking read. The
        // release helper owns only a channel, not the HWND or UI state.
        let (entered, ready) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        preview.state.borrow_mut().initial_started = false;
        preview.state.borrow_mut().initial_loader = Some(Box::new(move || {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            Ok(UserDictionary::default())
        }));
        start_initial_dictionary(preview.hwnd, preview.state.as_ref());
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let token = preview
            .state
            .borrow()
            .initial_dictionary
            .as_ref()
            .unwrap()
            .cancelled
            .clone();
        let helper = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !token.load(Ordering::Acquire) {
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            release.send(()).unwrap();
        });
        unsafe {
            DestroyWindow(preview.hwnd).unwrap();
        }
        helper.join().unwrap();
        assert!(preview.state.borrow().initial_dictionary.is_none());
        assert!(!preview.state.borrow().dictionary_ready);
        // A retained old state must not apply anything after HWND detachment.
        poll_initial_dictionary(preview.hwnd, preview.state.as_ref());
        assert!(!preview.state.borrow().dictionary_ready);
        assert_eq!(
            std::fs::read(dictionary.path()).unwrap(),
            b"invalid fixture json"
        );
        println!("native preview: {}", directory.display());
    }

    fn assert_owned_shortcuts_and_categories(
        hwnd: HWND,
        state: &RefCell<SettingsUiState>,
        directory: &Path,
    ) {
        use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
        let c = state.borrow().controls;
        for category in 0..4 {
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_COMMAND,
                    WPARAM((CATEGORY_ID + category as i32) as usize),
                    LPARAM(0),
                );
            }
            for (index, controls) in category_controls(c).iter().enumerate() {
                assert_eq!(
                    read_control_text(c.categories[index]).unwrap(),
                    ["常规", "AI功能", "快捷键", "词库与学习"][index]
                );
                for control in controls {
                    assert_eq!(
                        unsafe { IsWindowVisible(*control) }.as_bool(),
                        index == category
                    );
                }
            }
            for control in [c.online_ai, c.online_ai_hint] {
                assert_eq!(
                    unsafe { IsWindowVisible(control) }.as_bool(),
                    category == 1,
                    "online opt-in and cost notice belong exclusively to AI",
                );
            }
            let layout = control_layout(c);
            let mut visible: Vec<_> = layout
                .into_iter()
                .filter(|(control, _)| unsafe { IsWindowVisible(*control) }.as_bool())
                .collect();
            for (control, rect) in &mut visible {
                if [
                    c.theme_combo,
                    c.script_combo,
                    c.domain_combo,
                    c.shortcuts[0],
                    c.shortcuts[1],
                ]
                .contains(control)
                {
                    rect[3] = 32;
                }
            }
            for (index, (_, a)) in visible.iter().enumerate() {
                for (_, b) in &visible[index + 1..] {
                    assert!(
                        a[0] + a[2] <= b[0]
                            || b[0] + b[2] <= a[0]
                            || a[1] + a[3] <= b[1]
                            || b[1] + b[3] <= a[1],
                        "category {category}: overlapping {a:?} {b:?}"
                    );
                }
            }
            unsafe {
                let _ = UpdateWindow(hwnd);
            }
            write_preview_bitmap(
                hwnd,
                &directory.join(format!("settings-category-{category}.bmp")),
            );
        }
        assert_owned_recording(hwnd, state);
        let custom = WritingShortcuts {
            draft: WritingShortcut::new(0x41, 5),
            polish: WritingShortcut::new(0x7b, 5),
        };
        select_shortcuts(c, custom);
        for (control, value) in c.shortcuts.into_iter().zip([custom.draft, custom.polish]) {
            assert_eq!(read_control_text(control).unwrap(), value.label());
        }
        let store = state.borrow().store.clone();
        for enabled in [true, false] {
            set_checked(c.online_ai, enabled);
            let mut form = read_form(state).unwrap();
            assert_eq!(form.online_ai_enabled, enabled);
            assert_eq!(form.candidate_layout_version, 1);
            save_with_latest_consent(&store, &mut form).unwrap();
            assert_eq!(store.load().unwrap().online_ai_enabled, enabled);
        }
        let form = read_form(state).unwrap();
        assert_eq!(form.writing_shortcuts, custom);
        store.save(&form).unwrap();
        let before = std::fs::read(store.path()).unwrap();
        select_shortcuts(
            c,
            WritingShortcuts {
                draft: custom.draft,
                polish: custom.draft,
            },
        );
        assert!(read_form(state).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        unsafe {
            SetWindowLongPtrW(c.shortcuts[0], GWLP_USERDATA, 0);
        }
        assert!(read_form(state).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        select_shortcuts(c, custom);
        let mut online = state.borrow().settings.online_polish.clone();
        online.enabled = true;
        state.borrow_mut().settings.online_polish = online.clone();
        let form = read_form(state).unwrap();
        assert_eq!(form.writing_shortcuts, custom);
        assert_eq!(form.online_polish, online);
        store.save(&form).unwrap();
        assert_eq!(store.load().unwrap(), form);
        unsafe {
            SendMessageW(
                hwnd,
                WM_COMMAND,
                WPARAM(RESET_SHORTCUTS_ID as usize),
                LPARAM(0),
            );
        }
        assert_eq!(
            read_form(state).unwrap().writing_shortcuts,
            WritingShortcuts::default()
        );
        assert_eq!(
            store.load().unwrap(),
            form,
            "reset must not immediately save"
        );
        let defaults = read_form(state).unwrap();
        store.save(&defaults).unwrap();
        assert_eq!(
            store.load().unwrap().writing_shortcuts,
            WritingShortcuts::default()
        );
        state.borrow_mut().settings.online_polish = Default::default();
        std::fs::remove_file(store.path()).unwrap();
        // Shrink only our own HWND and exercise both native scroll ranges.
        let mut original = RECT::default();
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut original).unwrap();
        }
        unsafe {
            MoveWindow(hwnd, original.left, original.top, 480, 360, TRUE).unwrap();
            SendMessageW(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_VSCROLL,
                WPARAM(7),
                LPARAM(0),
            );
            SendMessageW(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_HSCROLL,
                WPARAM(7),
                LPARAM(0),
            );
        }
        assert!(state.borrow().controls.scroll_y > 0);
        assert!(state.borrow().controls.scroll_x > 0);
        write_preview_bitmap(hwnd, &directory.join("settings-small-workarea.bmp"));
        unsafe {
            MoveWindow(
                hwnd,
                original.left,
                original.top,
                original.right - original.left,
                original.bottom - original.top,
                TRUE,
            )
            .unwrap();
        }
        unsafe {
            SendMessageW(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_VSCROLL,
                WPARAM(6),
                LPARAM(0),
            );
            SendMessageW(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_HSCROLL,
                WPARAM(6),
                LPARAM(0),
            );
        }
        assert_eq!(state.borrow().controls.scroll_y, 0);
        assert_eq!(state.borrow().controls.scroll_x, 0);
        show_category(state, 0);
    }

    fn assert_owned_recording(hwnd: HWND, state: &RefCell<SettingsUiState>) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState};
        use windows::Win32::UI::WindowsAndMessaging::WM_KEYUP;
        struct KeyboardRestore([u8; 256]);
        impl Drop for KeyboardRestore {
            fn drop(&mut self) {
                unsafe {
                    let _ = SetKeyboardState(&self.0);
                }
            }
        }
        let mut original = [0; 256];
        unsafe {
            GetKeyboardState(&mut original).unwrap();
        }
        let _restore = KeyboardRestore(original);
        show_category(state, 2);
        let c = state.borrow().controls;
        let send = |index: usize, key: u16, mods: u8, down: bool| {
            let mut keys = [0u8; 256];
            for (vk, bit) in [(0x11, 1), (0x12, 2), (0x10, 4), (0x5b, 8)] {
                if mods & bit != 0 {
                    keys[vk] = 0x80;
                }
            }
            unsafe {
                SetKeyboardState(&keys).unwrap();
            }
            let message = MSG {
                hwnd: c.shortcuts[index],
                message: if down { WM_KEYDOWN } else { WM_KEYUP },
                wParam: WPARAM(key as usize),
                ..Default::default()
            };
            preprocess_settings_key(hwnd, state, &message)
        };
        let begin = |index: usize| unsafe {
            SendMessageW(
                hwnd,
                WM_COMMAND,
                WPARAM((DRAFT_SHORTCUT_ID as usize) + index),
                LPARAM(0),
            );
        };
        select_shortcuts(c, WritingShortcuts::default());
        begin(0);
        assert!(send(0, 0x11, 1, true));
        assert!(send(0, 0x12, 3, true));
        assert!(send(0, 0x12, 1, false));
        assert!(send(0, 0x11, 0, false));
        assert_eq!(read_shortcuts(c).unwrap().draft, WritingShortcut::new(0, 3));
        begin(1);
        assert!(send(1, 0x20, 1, true));
        assert_eq!(
            read_shortcuts(c).unwrap().polish,
            WritingShortcut::new(0x20, 1)
        );
        for (key, mods) in [(0x41, 9), (0xba, 6)] {
            begin(0);
            assert!(send(0, key, mods, true));
            assert_eq!(
                read_shortcuts(c).unwrap().draft,
                WritingShortcut::new(key, mods)
            );
        }
        let confirmed = read_shortcuts(c).unwrap();
        for (key, mods) in [(0x20, 1), (0x73, 2), (0x46, 5), (0x1b, 0), (0x0d, 0)] {
            begin(0);
            assert!(send(0, key, mods, true));
            assert_eq!(read_shortcuts(c).unwrap(), confirmed);
        }
        begin(0);
        assert!(!send(0, 0x09, 0, true));
        assert!(state.borrow().controls.recording.is_none());
        assert_eq!(read_shortcuts(c).unwrap(), confirmed);
        select_shortcuts(c, WritingShortcuts::default());
    }

    fn assert_owned_keyboard_route(hwnd: HWND, state: &RefCell<SettingsUiState>, directory: &Path) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, VK_SPACE, VK_TAB};
        use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, CB_SHOWDROPDOWN, PM_REMOVE};
        let controls = state.borrow().controls;
        assert_owned_small_window_focus(hwnd, state, directory);
        assert_owned_queued_recording(hwnd, state, directory);
        let key_message = |target, key: u16| MSG {
            hwnd: target,
            message: WM_KEYDOWN,
            wParam: WPARAM(key as usize),
            ..Default::default()
        };
        let pop_command = || {
            let mut command = MSG::default();
            assert!(
                unsafe { PeekMessageW(&mut command, hwnd, WM_COMMAND, WM_COMMAND, PM_REMOVE) }
                    .as_bool()
            );
            command
        };
        // Invoke the exact pre-dialog production helper, inspect its posted
        // WM_COMMAND without dispatching save or the destructive confirmation.
        for (target, expected) in [
            (controls.cancel_button, CANCEL_BUTTON_ID),
            (controls.clear_learning, CLEAR_LEARNING_ID),
            (controls.page_edit, SAVE_BUTTON_ID),
            (controls.save_button, SAVE_BUTTON_ID),
            (controls.categories[2], CATEGORY_ID + 2),
            (controls.reset_shortcuts, RESET_SHORTCUTS_ID),
            (controls.online_settings, 390),
            (controls.model_api_settings, 392),
            (controls.restore_send_confirmation, 391),
        ] {
            if target == controls.clear_learning {
                show_category(state, 3);
            } else if target == controls.reset_shortcuts {
                show_category(state, 2);
            } else if target == controls.online_settings
                || target == controls.model_api_settings
                || target == controls.restore_send_confirmation
            {
                show_category(state, 1);
            } else {
                show_category(state, 0);
            }
            let _ = unsafe { SetFocus(target) };
            let key = key_message(target, VK_RETURN.0);
            assert_eq!(
                settings_key_route(hwnd, state, &key),
                SettingsKeyRoute::Command(expected)
            );
            assert!(preprocess_settings_key(hwnd, state, &key));
            let command = pop_command();
            assert_eq!(command.wParam.0 & 0xffff, expected as usize);
            assert_eq!(command.wParam.0 >> 16, BN_CLICKED as usize);
            assert!(!directory.join("settings.json").exists());
            assert!(!directory.join("dictionary.json").exists());
        }
        for key in [VK_TAB.0, VK_SPACE.0] {
            let message = key_message(controls.page_edit, key);
            assert_eq!(
                settings_key_route(hwnd, state, &message),
                SettingsKeyRoute::Dialog
            );
            assert!(!preprocess_settings_key(hwnd, state, &message));
        }
        let foreign = key_message(HWND::default(), VK_RETURN.0);
        assert!(!preprocess_settings_key(hwnd, state, &foreign));
        // Both combos retain native Enter/Esc while expanded. Enable the
        // fixture's empty-domain combo only for exercising its native popup.
        for combo in [
            controls.theme_combo,
            controls.domain_combo,
            controls.script_combo,
        ] {
            show_category(
                state,
                if combo == controls.domain_combo {
                    3
                } else if controls.shortcuts.contains(&combo) {
                    2
                } else {
                    0
                },
            );
            let _ = unsafe { EnableWindow(combo, TRUE) };
            for key in [VK_RETURN.0, VK_ESCAPE.0] {
                unsafe {
                    SendMessageW(combo, CB_SHOWDROPDOWN, WPARAM(1), LPARAM(0));
                }
                assert!(theme_dropdown_open(state));
                let message = key_message(combo, key);
                assert_eq!(
                    settings_key_route(hwnd, state, &message),
                    SettingsKeyRoute::NativeDropdown
                );
                assert!(preprocess_settings_key(hwnd, state, &message));
                let mut command = MSG::default();
                // Selection notifications are allowed, but never IDOK/IDCANCEL.
                while unsafe { PeekMessageW(&mut command, hwnd, WM_COMMAND, WM_COMMAND, PM_REMOVE) }
                    .as_bool()
                {
                    assert!(!matches!(
                        (command.wParam.0 & 0xffff) as i32,
                        SAVE_BUTTON_ID | CANCEL_BUTTON_ID | CLEAR_LEARNING_ID
                    ));
                }
                unsafe {
                    SendMessageW(combo, CB_SHOWDROPDOWN, WPARAM(0), LPARAM(0));
                }
            }
        }
        let escape = key_message(controls.page_edit, VK_ESCAPE.0);
        assert!(preprocess_settings_key(hwnd, state, &escape));
        assert_eq!(pop_command().wParam.0 & 0xffff, CANCEL_BUTTON_ID as usize);
        let close = key_message(controls.cancel_button, VK_RETURN.0);
        assert!(preprocess_settings_key(hwnd, state, &close));
        let close_command = pop_command();
        unsafe {
            DispatchMessageW(&close_command);
        }
        assert!(!window_is_owned(hwnd, state));
        assert!(!directory.join("settings.json").exists());
        assert!(!directory.join("dictionary.json").exists());
        assert!(!preprocess_settings_key(hwnd, state, &close));
    }

    fn assert_owned_queued_recording(
        hwnd: HWND,
        state: &RefCell<SettingsUiState>,
        directory: &Path,
    ) {
        use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE};
        show_category(state, 2);
        let c = state.borrow().controls;
        let original = read_shortcuts(c).unwrap();
        // Only post to our own HWNDs. Use the production loop's ordering:
        // recording/Enter routing first, native dialog navigation second.
        let pump = || {
            let mut message = MSG::default();
            let mut count = 0;
            while unsafe { PeekMessageW(&mut message, hwnd, 0, 0, PM_REMOVE) }.as_bool() {
                count += 1;
                assert!(count < 1000, "owned queue did not settle");
                if message.message == WM_COMMAND {
                    assert!(
                        !matches!(
                            (message.wParam.0 & 0xffff) as i32,
                            SAVE_BUTTON_ID | CANCEL_BUTTON_ID | CLEAR_LEARNING_ID
                        ),
                        "recording must not post a save, close or destructive command"
                    );
                }
                if !preprocess_settings_key(hwnd, state, &message)
                    && !unsafe { IsDialogMessageW(hwnd, &message) }.as_bool()
                {
                    unsafe {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                ensure_settings_focus_visible(hwnd, state);
            }
            assert!(count > 0, "queued recording selector must consume messages");
        };
        for activate_by_enter in [false, true] {
            for cancel in [0x1b, 0x0d, 0x09] {
                let _ = unsafe { SetFocus(c.shortcuts[0]) };
                unsafe {
                    if activate_by_enter {
                        PostMessageW(c.shortcuts[0], WM_KEYDOWN, WPARAM(0x0d), LPARAM(0)).unwrap();
                    } else {
                        PostMessageW(
                            hwnd,
                            WM_COMMAND,
                            WPARAM(DRAFT_SHORTCUT_ID as usize),
                            LPARAM(c.shortcuts[0].0 as isize),
                        )
                        .unwrap();
                    }
                }
                pump();
                assert_eq!(state.borrow().controls.recording, Some(0));
                unsafe {
                    PostMessageW(c.shortcuts[0], WM_KEYDOWN, WPARAM(cancel), LPARAM(0)).unwrap();
                }
                pump();
                assert!(state.borrow().controls.recording.is_none());
                assert_eq!(read_shortcuts(c).unwrap(), original);
                assert_eq!(
                    read_control_text(c.shortcuts[0]).unwrap(),
                    original.draft.label()
                );
                assert!(!directory.join("settings.json").exists());
                assert!(!directory.join("dictionary.json").exists());
            }
        }
    }

    fn assert_owned_small_window_focus(
        hwnd: HWND,
        state: &RefCell<SettingsUiState>,
        directory: &Path,
    ) {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::ScreenToClient;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetFocus, GetKeyboardState, SetKeyboardState, VK_SHIFT, VK_TAB,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetClientRect, GetWindowRect, IsWindowVisible, PeekMessageW, PM_REMOVE,
        };
        struct KeyboardState([u8; 256]);
        impl Drop for KeyboardState {
            fn drop(&mut self) {
                // Thread-local state only: never synthesize or inject a system key.
                let _ = unsafe { SetKeyboardState(&self.0) };
            }
        }
        let mut keys = [0u8; 256];
        unsafe { GetKeyboardState(&mut keys).unwrap() };
        let _restore = KeyboardState(keys);
        let mut original = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut original).unwrap();
            MoveWindow(hwnd, original.left, original.top, 480, 360, TRUE).unwrap();
        }
        let c = state.borrow().controls;
        let mut visited = Vec::new();
        for category in 0..4 {
            show_category(state, category);
            let _ = unsafe { SetFocus(c.categories[category]) };
            for backwards in [false, true] {
                keys[VK_SHIFT.0 as usize] = if backwards { 0x80 } else { 0 };
                unsafe { SetKeyboardState(&keys).unwrap() };
                for _ in 0..24 {
                    let message = MSG {
                        hwnd: unsafe { GetFocus() },
                        message: WM_KEYDOWN,
                        wParam: WPARAM(VK_TAB.0 as usize),
                        ..Default::default()
                    };
                    assert!(!preprocess_settings_key(hwnd, state, &message));
                    assert!(unsafe { IsDialogMessageW(hwnd, &message) }.as_bool());
                    ensure_settings_focus_visible(hwnd, state);
                    let focused = unsafe { GetFocus() };
                    assert!(unsafe { IsWindowVisible(focused) }.as_bool());
                    visited.push(focused);
                    let mut rect = RECT::default();
                    let mut client = RECT::default();
                    unsafe {
                        GetWindowRect(focused, &mut rect).unwrap();
                        GetClientRect(hwnd, &mut client).unwrap();
                    }
                    let mut origin = POINT {
                        x: rect.left,
                        y: rect.top,
                    };
                    assert!(unsafe { ScreenToClient(hwnd, &mut origin) }.as_bool());
                    assert!(
                        origin.x >= 0 && origin.y >= 0
                            && origin.x + rect.right - rect.left <= client.right
                            && origin.y + rect.bottom - rect.top <= client.bottom,
                        "category={category}, backwards={backwards}, focus={}, origin={origin:?}, rect={rect:?}, client={client:?}",
                        unsafe { GetDlgCtrlID(focused) }
                    );
                    let mut command = MSG::default();
                    while unsafe {
                        PeekMessageW(&mut command, hwnd, WM_COMMAND, WM_COMMAND, PM_REMOVE)
                    }
                    .as_bool()
                    {
                        assert!(
                            !matches!(
                                (command.wParam.0 & 0xffff) as i32,
                                SAVE_BUTTON_ID | CLEAR_LEARNING_ID
                            ),
                            "Tab must not save or clear"
                        );
                    }
                }
            }
        }
        for target in c.categories.into_iter().chain([
            c.online_ai,
            c.page_edit,
            c.theme_combo,
            c.script_combo,
            c.save_button,
            c.cancel_button,
            c.reset_shortcuts,
            c.online_settings,
            c.model_api_settings,
            c.ai_correction,
            c.ai_scope_compatibility,
            c.restore_send_confirmation,
        ]) {
            assert!(
                visited.contains(&target),
                "native traversal missed {}",
                unsafe { GetDlgCtrlID(target) }
            );
        }
        assert!(!directory.join("settings.json").exists());
        assert!(!directory.join("dictionary.json").exists());
        write_preview_bitmap(hwnd, &directory.join("settings-small-tab-focus.bmp"));
        unsafe {
            MoveWindow(
                hwnd,
                original.left,
                original.top,
                original.right - original.left,
                original.bottom - original.top,
                TRUE,
            )
            .unwrap();
        }
        show_category(state, 0);
    }

    fn write_preview_bitmap(hwnd: HWND, path: &Path) {
        use windows::Win32::Graphics::Gdi::*;
        #[link(name = "user32")]
        extern "system" {
            fn PrintWindow(hwnd: *mut c_void, dc: *mut c_void, flags: u32) -> i32;
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
