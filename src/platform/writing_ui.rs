//! Native, explicitly invoked BYOK writing panel. No host/clipboard reads.
//! Window state is cloned before dispatch; no RefCell borrow spans a Win32 call.
use crate::{
    writing::{
        worker::{Phase, WritingJob},
        Length, Task, Tone, WritingRequest,
    },
    ChineseScript,
};
use std::{cell::RefCell, marker::PhantomData, rc::Rc};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            CreateFontW, DeleteObject, GetSysColorBrush, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
            COLOR_BTNFACE, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_NORMAL, HFONT, HGDIOBJ,
            OUT_DEFAULT_PRECIS,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Controls::{SetScrollInfo, EM_SETLIMITTEXT, EM_SETREADONLY},
            HiDpi::GetDpiForSystem,
            Input::KeyboardAndMouse::{
                EnableWindow, GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_RETURN,
                VK_SHIFT, VK_TAB,
            },
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
};

pub enum PanelSource {
    Manual,
    Insertion,
    Selection(String),
}
pub type ApplyCallback = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Thread-affine owner. The caller keeps its existing message loop running.
pub struct WritingPanel {
    state: Rc<RefCell<State>>,
    _thread: PhantomData<Rc<()>>,
}
const INPUT: usize = 110;
const OUTPUT: usize = 111;
const GENERATE: usize = 112;
const AGAIN: usize = 113;
const COPY: usize = 114;
const APPLY: usize = 115;
const HELP: usize = 117;
const CONNECTION: usize = 118;
const TONE: usize = 119;
const LENGTH: usize = 120;
const SCRIPT: usize = 122;
#[cfg(test)]
const KEY: usize = 123;
#[cfg(test)]
const SAVE: usize = 124;
#[cfg(test)]
const DELETE: usize = 125;
const STATUS: usize = 126;
const RETRY: usize = 129;
const RETRY_NOTE: usize = 130;
const RESTORE: usize = 128;
const TIMER: usize = 0x4657;
const KEY_GENERATE: u32 = WM_APP + 41;
const KEY_ESCAPE: u32 = WM_APP + 42;
const CLASS: PCWSTR = w!("Flower.WritingPanel.050");
type Shared = Rc<RefCell<State>>;
const BACKGROUND: windows::Win32::Foundation::COLORREF =
    windows::Win32::Foundation::COLORREF(0x00f8f7f4);
const WHITE: windows::Win32::Foundation::COLORREF =
    windows::Win32::Foundation::COLORREF(0x00ffffff);
const TEXT_COLOR: windows::Win32::Foundation::COLORREF =
    windows::Win32::Foundation::COLORREF(0x003a2b17);
const MUTED: windows::Win32::Foundation::COLORREF =
    windows::Win32::Foundation::COLORREF(0x00756652);
const ACCENT: windows::Win32::Foundation::COLORREF =
    windows::Win32::Foundation::COLORREF(0x00696b0f);
const BORDER: windows::Win32::Foundation::COLORREF =
    windows::Win32::Foundation::COLORREF(0x00e9e5dc);

fn paint_panel(s: &Shared, dc: windows::Win32::Graphics::Gdi::HDC) {
    use windows::Win32::Graphics::Gdi::*;
    let (hwnd, dpi, offset, connection) = {
        let state = s.borrow();
        (state.hwnd, state.dpi, state.scroll, state.connection_open)
    };
    unsafe {
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let saved = SaveDC(dc);
        let brush = GetStockObject(DC_BRUSH);
        SelectObject(dc, brush);
        SelectObject(dc, GetStockObject(DC_PEN));
        SetDCBrushColor(dc, BACKGROUND);
        FillRect(dc, &client, HBRUSH(brush.0));
        SetDCBrushColor(dc, WHITE);
        SetDCPenColor(dc, BORDER);
        let width = (client.right * 96 / dpi).max(360);
        let extra = if width < 620 { 176 } else { 0 };
        for (top, bottom) in [(96, 432 + extra), (440 + extra, 744 + extra)]
            .into_iter()
            .chain(connection.then_some((892 + extra, 1152 + extra)))
        {
            let _ = RoundRect(
                dc,
                24 * dpi / 96 - offset[0],
                top * dpi / 96 - offset[1],
                (width - 24) * dpi / 96 - offset[0],
                bottom * dpi / 96 - offset[1],
                16 * dpi / 96,
                16 * dpi / 96,
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
        let primary = item.CtlID == GENERATE as u32;
        let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
        let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
        let fill = if disabled {
            BORDER
        } else if primary {
            ACCENT
        } else if pressed {
            BACKGROUND
        } else {
            WHITE
        };
        SetDCBrushColor(item.hDC, fill);
        SetDCPenColor(item.hDC, if primary { fill } else { BORDER });
        let r = item.rcItem;
        // Owner-drawn rounded corners must not retain pixels from prior states.
        let background = if [HELP as u32, CONNECTION as u32].contains(&item.CtlID) {
            BACKGROUND
        } else {
            WHITE
        };
        SetDCBrushColor(item.hDC, background);
        FillRect(item.hDC, &r, HBRUSH(GetStockObject(DC_BRUSH).0));
        SetDCBrushColor(item.hDC, fill);
        let _ = RoundRect(item.hDC, r.left, r.top, r.right, r.bottom, 10, 10);
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(
            item.hDC,
            if disabled {
                MUTED
            } else if primary {
                WHITE
            } else {
                TEXT_COLOR
            },
        );
        let font = SendMessageW(item.hwndItem, WM_GETFONT, WPARAM(0), LPARAM(0));
        if font.0 != 0 {
            SelectObject(item.hDC, HGDIOBJ(font.0 as *mut _));
        }
        let mut label = wide(&text(item.hwndItem));
        let mut rect = r;
        DrawTextW(
            item.hDC,
            &mut label,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        if item.itemState.0 & ODS_FOCUS.0 != 0 {
            let focus = RECT {
                left: r.left + 4,
                top: r.top + 4,
                right: r.right - 4,
                bottom: r.bottom - 4,
            };
            let _ = DrawFocusRect(item.hDC, &focus);
        }
        let _ = RestoreDC(item.hDC, saved);
    }
}

const HELP_CLASS: PCWSTR = w!("Flower.WritingSetupHelp.050");
const HELP_BODY: usize = 201;
struct HelpState {
    body: HWND,
    font: HFONT,
    dpi: i32,
}

/// Shows the sole embedded guide locally. No nested loop, credentials or network.
/// Ownership is Win32 ownership: destroying the owner also destroys its help windows.
pub fn show_setup_help(owner: HWND) -> Result<(), String> {
    create_setup_help(owner).map(|_| ())
}
fn create_setup_help(owner: HWND) -> Result<HWND, String> {
    unsafe {
        let module = GetModuleHandleW(None).map_err(|_| "无法创建使用帮助。")?;
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(help_proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: HELP_CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        });
        let work = work_area();
        let dpi = GetDpiForSystem().max(96) as i32;
        let width = (720 * dpi / 96).min(work.right - work.left);
        let height = (760 * dpi / 96).min(work.bottom - work.top);
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            HELP_CLASS,
            w!("AI 使用帮助 · Flower"),
            WS_OVERLAPPEDWINDOW,
            work.left,
            work.top,
            width,
            height,
            owner,
            None,
            module,
            None,
        )
        .map_err(|_| "无法创建使用帮助。")?;
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        Ok(hwnd)
    }
}
unsafe extern "system" fn help_keys(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    owner: usize,
) -> LRESULT {
    if msg == WM_KEYDOWN && wp.0 == VK_ESCAPE.0 as usize {
        let _ = PostMessageW(HWND(owner as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0));
        return LRESULT(0);
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}
unsafe fn help_layout(hwnd: HWND, state: &HelpState) {
    let mut rect = RECT::default();
    if GetClientRect(hwnd, &mut rect).is_ok() {
        let margin = 24 * state.dpi / 96;
        let _ = MoveWindow(
            state.body,
            margin,
            margin,
            (rect.right - margin * 2).max(1),
            (rect.bottom - margin * 2).max(1),
            true,
        );
    }
}
unsafe fn help_font(state: &mut HelpState) {
    let font = CreateFontW(
        -14 * state.dpi / 96,
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
        w!("Microsoft YaHei UI"),
    );
    if !font.0.is_null() {
        SendMessageW(state.body, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        let old = std::mem::replace(&mut state.font, font);
        if !old.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(old.0));
        }
    }
}
unsafe extern "system" fn help_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CREATE {
        let guide = wide(
            &crate::writing::help::SETUP_GUIDE
                .replace("\r\n", "\n")
                .replace('\n', "\r\n"),
        );
        let body = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            PCWSTR(guide.as_ptr()),
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE(ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32),
            0,
            0,
            1,
            1,
            hwnd,
            HMENU(HELP_BODY as *mut _),
            GetModuleHandleW(None).unwrap_or_default(),
            None,
        );
        let Ok(body) = body else {
            return LRESULT(-1);
        };
        if !SetWindowSubclass(body, Some(help_keys), 1, hwnd.0 as usize).as_bool() {
            return LRESULT(-1);
        }
        let mut state = Box::new(HelpState {
            body,
            font: HFONT::default(),
            dpi: windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) as i32,
        });
        help_font(&mut state);
        help_layout(hwnd, &state);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
        return LRESULT(0);
    }
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HelpState;
    if raw.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    match msg {
        WM_SIZE => {
            help_layout(hwnd, &*raw);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            (*raw).dpi = (wp.0 & 0xffff).max(96) as i32;
            help_font(&mut *raw);
            let r = *(lp.0 as *const RECT);
            let work = work_area();
            let width = (r.right - r.left).min(work.right - work.left);
            let height = (r.bottom - r.top).min(work.bottom - work.top);
            let _ = SetWindowPos(
                hwnd,
                None,
                r.left.clamp(work.left, work.right - width),
                r.top.clamp(work.top, work.bottom - height),
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            help_layout(hwnd, &*raw);
            LRESULT(0)
        }
        WM_SETFOCUS => {
            let body = (*raw).body;
            let _ = SetFocus(body);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let state = Box::from_raw(raw);
            if !state.font.0.is_null() {
                let _ = DeleteObject(HGDIOBJ(state.font.0));
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
/// Immutable task and authority identity; never owns or copies the callback.
#[derive(Clone)]
struct ResultOrigin {
    input: String,
    task: Task,
    script: ChineseScript,
    selection: bool,
    bound: bool,
    binding_id: u64,
}
#[derive(Clone)]
struct ResultText {
    origin: ResultOrigin,
    id: u64,
    text: String,
    complete: bool,
    consumed: bool,
    stale: bool,
    edited: bool,
}
struct Page {
    binding_id: u64,
    task: Task,
    script: ChineseScript,
    input: String,
    selection: bool,
    bound: bool,
    result: Option<ResultText>,
}
struct State {
    preference_store: Option<crate::settings::SettingsStore>,
    hwnd: HWND,
    controls: Vec<(usize, HWND)>,
    font: HFONT,
    headings: Vec<HFONT>,
    layout: Vec<(HWND, [i32; 4])>,
    dpi: i32,
    scroll: [i32; 2],
    laying_out: bool,
    page: Page,
    connection_open: bool,
    apply: Option<ApplyCallback>,
    job: Option<WritingJob>,
    request: Option<u64>,
    backup: Option<ResultText>,
    next: u64,
    pending: bool,
    rendering: bool,
    status: String,
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn text(hwnd: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0; n + 1];
        let read = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..read.max(0) as usize])
    }
}
fn set_text(hwnd: HWND, text: &str) {
    if self::text(hwnd) == text {
        return;
    }
    let s = wide(text);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(s.as_ptr()));
    }
}
unsafe fn enable_changed(hwnd: HWND, enabled: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    if IsWindowEnabled(hwnd).as_bool() != enabled {
        let _ = EnableWindow(hwnd, enabled);
    }
}
unsafe fn readonly_changed(hwnd: HWND, readonly: bool) {
    if (GetWindowLongPtrW(hwnd, GWL_STYLE) & ES_READONLY as isize != 0) != readonly {
        SendMessageW(hwnd, EM_SETREADONLY, WPARAM(readonly as usize), LPARAM(0));
    }
}
fn update_output_text(hwnd: HWND, value: &str) {
    if text(hwnd) == value {
        return;
    }
    use windows::Win32::UI::Controls::{
        EM_GETFIRSTVISIBLELINE, EM_GETSEL, EM_LINESCROLL, EM_SETSEL,
    };
    unsafe {
        let (mut start, mut end) = (0u32, 0u32);
        SendMessageW(
            hwnd,
            EM_GETSEL,
            WPARAM((&mut start as *mut u32) as usize),
            LPARAM((&mut end as *mut u32) as isize),
        );
        let first = SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, WPARAM(0), LPARAM(0)).0;
        SendMessageW(hwnd, WM_SETREDRAW, WPARAM(0), LPARAM(0));
        set_text(hwnd, value);
        SendMessageW(
            hwnd,
            EM_SETSEL,
            WPARAM(start as usize),
            LPARAM(end as isize),
        );
        let now = SendMessageW(hwnd, EM_GETFIRSTVISIBLELINE, WPARAM(0), LPARAM(0)).0;
        SendMessageW(hwnd, EM_LINESCROLL, WPARAM(0), LPARAM(first - now));
        SendMessageW(hwnd, WM_SETREDRAW, WPARAM(1), LPARAM(0));
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
    }
}
fn retry_requested(s: &Shared) -> bool {
    unsafe { SendMessageW(control(s, RETRY), BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1 }
}
fn control(s: &Shared, id: usize) -> HWND {
    s.borrow()
        .controls
        .iter()
        .find(|(n, _)| *n == id)
        .map(|(_, h)| *h)
        .unwrap_or_default()
}
fn alive(s: &Shared) -> bool {
    !s.borrow().hwnd.0.is_null()
}
fn busy(s: &State) -> bool {
    s.pending || s.job.is_some()
}
fn notice(s: &Shared, message: &str) {
    s.borrow_mut().status = message.into();
    render(s);
}
fn selection(hwnd: HWND) -> usize {
    unsafe {
        SendMessageW(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0))
            .0
            .max(0) as usize
    }
}
fn select(hwnd: HWND, n: usize) {
    unsafe {
        // CB_ERR (-1) is not item zero: newly built combos still need selection.
        if SendMessageW(hwnd, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 == n as isize {
            return;
        }
        SendMessageW(hwnd, CB_SETCURSEL, WPARAM(n), LPARAM(0));
    }
}
fn valid_input(input: &str) -> bool {
    !input.trim().is_empty() && input.chars().count() <= 500
}
fn can_apply(page: &Page) -> bool {
    page.bound
        && page.result.as_ref().is_some_and(|r| {
            r.complete
                && !r.consumed
                && !r.stale
                && r.origin.input == page.input
                && r.origin.task == page.task
                && r.origin.script == page.script
                && r.origin.binding_id == page.binding_id
                && r.origin.bound
                && r.origin.selection == page.selection
                && !r.text.trim().is_empty()
                && r.text.chars().count() <= 800
        })
}
impl Page {
    fn origin(&self) -> ResultOrigin {
        ResultOrigin {
            input: self.input.clone(),
            task: self.task,
            script: self.script,
            selection: self.selection,
            bound: self.bound,
            binding_id: self.binding_id,
        }
    }
    fn restore(&mut self) -> bool {
        let Some(result) = self.result.as_mut().filter(|r| r.complete) else {
            return false;
        };
        let origin = &result.origin;
        self.input = origin.input.clone();
        self.task = origin.task;
        self.script = origin.script;
        // Task history is not authority history: manual-mode revocation is
        // irreversible within this panel. Only a new host invocation can bind.
        result.stale = false;
        true
    }
    fn consume(&mut self) -> Option<(String, u64)> {
        if !can_apply(self) {
            return None;
        }
        let result = self.result.as_mut()?;
        result.consumed = true;
        Some((result.text.clone(), result.id))
    }
}

pub fn open_panel(
    task: Task,
    script: ChineseScript,
    source: PanelSource,
    apply: Option<ApplyCallback>,
) -> Result<WritingPanel, String> {
    open_panel_with_store(
        task,
        script,
        source,
        apply,
        crate::settings::SettingsStore::for_current_user().ok(),
    )
}
pub(crate) fn open_panel_with_store(
    task: Task,
    script: ChineseScript,
    source: PanelSource,
    apply: Option<ApplyCallback>,
    preference_store: Option<crate::settings::SettingsStore>,
) -> Result<WritingPanel, String> {
    if !crate::writing::config::online_enabled(preference_store.as_ref()) {
        return Err(crate::writing::WritingError::OnlineDisabled.to_string());
    }
    require_draft(task)?;
    let insertion = matches!(source, PanelSource::Insertion);
    let bound = insertion && apply.is_some();
    // A selection is not an insertion grant. Never reuse replacement authority.
    let page = Page {
        binding_id: u64::from(bound),
        task,
        script,
        input: String::new(),
        selection: false,
        bound,
        result: None,
    };
    let state = Rc::new(RefCell::new(State {
        preference_store,
        hwnd: HWND::default(),
        controls: vec![],
        font: HFONT::default(),
        headings: vec![],
        layout: vec![],
        dpi: unsafe { GetDpiForSystem().max(96) as i32 },
        scroll: [0, 0],
        laying_out: false,
        page,
        connection_open: false,
        apply: if bound { apply } else { None },
        job: None,
        request: None,
        backup: None,
        next: 0,
        pending: false,
        rendering: false,
        status: "在线模型处理；请先管理模型 API。打开或保存不发送文本。".into(),
    }));
    unsafe {
        let module = GetModuleHandleW(None).map_err(|_| "无法创建面板。".to_string())?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        // Registration is process-wide and idempotent; another live panel may use it.
        RegisterClassExW(&class);
        let raw = Box::into_raw(Box::new(state.clone()));
        let dpi = GetDpiForSystem().max(96) as i32;
        let work = work_area();
        let (width, height) = initial_size(dpi, work.right - work.left, work.bottom - work.top);
        let created = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            CLASS,
            w!("AI 帮写"),
            WS_OVERLAPPED
                | WS_CAPTION
                | WS_SYSMENU
                | WS_MINIMIZEBOX
                | WS_MAXIMIZEBOX
                | WS_THICKFRAME
                | WS_VSCROLL
                | WS_CLIPCHILDREN,
            work.left,
            work.top,
            width,
            height,
            None,
            None,
            module,
            Some(raw.cast()),
        );
        if created.is_err() {
            // WM_NCDESTROY releases the state if WM_NCCREATE ran.
            if Rc::strong_count(&state) > 1 {
                drop(Box::from_raw(raw));
            }
            return Err("无法创建面板。".into());
        }
    }
    let panel = WritingPanel {
        state,
        _thread: PhantomData,
    };
    panel.show(task)?;
    Ok(panel)
}
impl WritingPanel {
    pub fn hwnd(&self) -> HWND {
        self.state.borrow().hwnd
    }
    pub fn is_open(&self) -> bool {
        alive(&self.state)
    }
    pub fn show(&self, task: Task) -> Result<(), String> {
        require_draft(task)?;
        if !self.is_open() {
            return Err("面板已关闭。".into());
        }
        if busy(&self.state.borrow()) {
            notice(&self.state, "请先停止当前生成，再切换任务。");
        } else {
            require_draft(task)?;
            render(&self.state);
        }
        unsafe {
            let _ = ShowWindow(self.hwnd(), SW_SHOWNORMAL);
            let _ = SetForegroundWindow(self.hwnd());
        }
        Ok(())
    }
    pub fn close(&mut self) {
        let hwnd = self.hwnd();
        if !hwnd.0.is_null() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}
impl Drop for WritingPanel {
    fn drop(&mut self) {
        self.close();
    }
}
/// Standalone manual entry only. Never posts thread-wide WM_QUIT.
pub fn run_writing_app(task: Task) -> Result<(), String> {
    let panel = open_panel(task, ChineseScript::Simplified, PanelSource::Manual, None)?;
    unsafe {
        let mut msg = MSG::default();
        while panel.is_open() {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            if result == -1 {
                return Err("面板消息处理失败。".into());
            }
            if result == 0 {
                break;
            }
            if !IsDialogMessageW(panel.hwnd(), &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let cs = &*(lp.0 as *const CREATESTRUCTW);
        let raw = cs.lpCreateParams as *mut Shared;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        (*raw).borrow_mut().hwnd = hwnd;
    }
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Shared;
    if raw.is_null() {
        return DefWindowProcW(hwnd, message, wp, lp);
    }
    let s = (*raw).clone();
    match message {
        WM_PAINT => {
            use windows::Win32::Graphics::Gdi::*;
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            paint_panel(&s, dc);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint_panel(&s, windows::Win32::Graphics::Gdi::HDC(wp.0 as *mut _));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DRAWITEM => {
            paint_button(&*(lp.0 as *const windows::Win32::UI::Controls::DRAWITEMSTRUCT));
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            use windows::Win32::Graphics::Gdi::*;
            let dc = HDC(wp.0 as *mut _);
            let id = GetDlgCtrlID(HWND(lp.0 as *mut _)) as usize;
            let on_card = ![10, 11, STATUS].contains(&id);
            SetBkMode(dc, OPAQUE);
            SetBkColor(dc, if on_card { WHITE } else { BACKGROUND });
            SetTextColor(
                dc,
                if [11, 12, 13, RETRY_NOTE].contains(&id) {
                    MUTED
                } else {
                    TEXT_COLOR
                },
            );
            SetDCBrushColor(dc, if on_card { WHITE } else { BACKGROUND });
            LRESULT(GetStockObject(DC_BRUSH).0 as isize)
        }
        WM_CREATE => {
            if build(&s).is_err() {
                return LRESULT(-1);
            }
            render(&s);
            LRESULT(0)
        }
        WM_COMMAND => {
            command(&s, wp.0 & 0xffff, (wp.0 >> 16) as u32);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER => {
            poll(&s);
            LRESULT(0)
        }
        WM_SIZE => {
            layout(&s);
            LRESULT(0)
        }
        WM_VSCROLL | WM_HSCROLL => {
            scroll_message(&s, message == WM_HSCROLL, wp);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = (wp.0 >> 16) as u16 as i16 as i32;
            s.borrow_mut().scroll[1] -= delta / 120 * 48;
            layout(&s);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xffff).max(96) as i32;
            s.borrow_mut().dpi = dpi;
            replace_font(&s);
            let suggested = *(lp.0 as *const RECT);
            let work = work_area();
            let width = (suggested.right - suggested.left).min(work.right - work.left);
            let height = (suggested.bottom - suggested.top).min(work.bottom - work.top);
            let _ = SetWindowPos(
                hwnd,
                None,
                suggested.left.clamp(work.left, work.right - width),
                suggested.top.clamp(work.top, work.bottom - height),
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            layout(&s);
            LRESULT(0)
        }
        KEY_GENERATE => {
            generate(&s);
            LRESULT(0)
        }
        KEY_ESCAPE => {
            if busy(&s.borrow()) {
                stop(&s);
            } else {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER);
            let (job, callback) = {
                let mut state = s.borrow_mut();
                state.hwnd = HWND::default();
                state.request = None;
                (state.job.take(), state.apply.take())
            };
            drop(job);
            drop(callback);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let fonts = {
                let mut state = s.borrow_mut();
                let mut fonts = std::mem::take(&mut state.headings);
                fonts.push(std::mem::take(&mut state.font));
                fonts
            };
            for font in fonts {
                if !font.0.is_null() {
                    let _ = DeleteObject(HGDIOBJ(font.0));
                }
            }
            drop(Box::from_raw(raw));
            DefWindowProcW(hwnd, message, wp, lp)
        }
        _ => DefWindowProcW(hwnd, message, wp, lp),
    }
}
unsafe extern "system" fn keys(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_GETDLGCODE
        && (wp.0 as u16 == VK_ESCAPE.0
            || (wp.0 as u16 == VK_RETURN.0 && GetKeyState(VK_CONTROL.0 as i32) < 0))
    {
        return LRESULT(4); // DLGC_WANTALLKEYS only for our explicit commands.
    }
    if msg == WM_KEYDOWN {
        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
        let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
        let alt = GetKeyState(VK_MENU.0 as i32) < 0;
        if wp.0 as u16 == VK_TAB.0 && !ctrl && !alt {
            if let Ok(next) = GetNextDlgTabItem(HWND(data as *mut _), hwnd, shift) {
                let _ = SetFocus(next);
            }
            return LRESULT(0);
        }
        if let Some(command) = key_command(wp.0 as u16, ctrl, shift, alt) {
            if lp.0 & (1 << 30) == 0 {
                let _ = PostMessageW(HWND(data as *mut _), command, WPARAM(0), LPARAM(0));
            }
            return LRESULT(0);
        }
    }
    if msg == WM_SETFOCUS {
        let parent = HWND(data as *mut _);
        let raw = GetWindowLongPtrW(parent, GWLP_USERDATA) as *mut Shared;
        if !raw.is_null() {
            let state = (*raw).clone();
            reveal(&state, hwnd);
        }
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}
fn key_command(key: u16, ctrl: bool, shift: bool, alt: bool) -> Option<u32> {
    if key == VK_ESCAPE.0 && !ctrl && !shift && !alt {
        Some(KEY_ESCAPE)
    } else if key == VK_RETURN.0 && ctrl && !shift && !alt {
        Some(KEY_GENERATE)
    } else {
        None
    }
}
fn work_area() -> RECT {
    let mut rect = RECT::default();
    unsafe {
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut rect as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_err()
        {
            rect.right = GetSystemMetrics(SM_CXSCREEN).max(1);
            rect.bottom = GetSystemMetrics(SM_CYSCREEN).max(1);
        }
    }
    rect
}
fn initial_size(dpi: i32, width: i32, height: i32) -> (i32, i32) {
    (
        (740 * dpi / 96).min(width).max(1),
        (960 * dpi / 96).min(height).max(1),
    )
}
fn require_draft(task: Task) -> Result<(), String> {
    if matches!(task, Task::Draft { .. }) {
        Ok(())
    } else {
        Err("润色请使用原文旁候选浮层；AI帮写窗口不接受润色任务。".into())
    }
}
fn content_height(width: i32, _connection: bool) -> i32 {
    1000 + if width < 620 { 176 } else { 0 }
}
#[cfg(test)]
fn scroll_limit(dpi: i32, viewport: [i32; 2]) -> [i32; 2] {
    let width = (viewport[0] * 96 / dpi).max(360);
    [
        (width * dpi / 96 - viewport[0]).max(0),
        (content_height(width, true) * dpi / 96 - viewport[1]).max(0),
    ]
}
fn positions(width: i32) -> Vec<(usize, [i32; 4])> {
    let narrow = width < 620;
    let extra = if narrow { 176 } else { 0 };
    let inner = (width - 80).max(120);
    let mut v = vec![
        (10, [24, 20, 180, 32]),
        (HELP, [(width - 160).max(200), 20, 136, 32]),
        (11, [24, 60, width - 48, 24]),
        (1, [40, 112, inner, 24]),
        (12, [40, 144, inner, 24]),
        (INPUT, [40, 176, inner, 120]),
        (3, [40, 456 + extra, inner, 24]),
        (13, [40, 488 + extra, inner, 24]),
        (OUTPUT, [40, 520 + extra, inner, 152]),
        (STATUS, [24, 760 + extra, width - 48, 72]),
        (CONNECTION, [24, 844 + extra, 180, 32]),
        (RETRY, [40, 888 + extra, inner, 44]),
        (RETRY_NOTE, [40, 936 + extra, inner, 44]),
    ];
    for (i, id) in [TONE, LENGTH, SCRIPT].into_iter().enumerate() {
        let col = if narrow { 0 } else { i as i32 };
        let row = if narrow { i as i32 * 72 } else { 0 };
        let w = if narrow { inner } else { (inner - 24) / 3 };
        v.push((id + 1000, [40 + col * (w + 12), 308 + row, w, 24]));
        v.push((id, [40 + col * (w + 12), 336 + row, w, 160]));
    }
    for (i, id) in [GENERATE, AGAIN, RESTORE].into_iter().enumerate() {
        let w = (inner - 24) / 3;
        v.push((id, [40 + i as i32 * (w + 12), 380 + extra, w, 36]));
    }
    v.push((COPY, [40, 688 + extra, 120, 36]));
    v.push((APPLY, [172, 688 + extra, (inner - 132).max(120), 36]));
    v
}
fn layout(s: &Shared) {
    let (hwnd, dpi) = {
        let state = s.borrow();
        if state.laying_out {
            return;
        }
        (state.hwnd, state.dpi)
    };
    if hwnd.0.is_null() {
        return;
    }
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return;
    }
    let viewport = [client.right.max(1), client.bottom.max(1)];
    let logical_width = (viewport[0] * 96 / dpi).max(360);
    // Preserve the readable 360px card canvas on tiny work areas, but make its
    // complete width reachable. Do not impose a window minimum beyond the screen.
    // At >=360 logical client pixels the horizontal bar remains absent.
    let limits = [
        (logical_width * dpi / 96 - viewport[0]).max(0),
        (content_height(logical_width, s.borrow().connection_open) * dpi / 96 - viewport[1]).max(0),
    ];
    let positions = positions(logical_width);
    {
        let mut state = s.borrow_mut();
        state.layout = state
            .controls
            .iter()
            .filter_map(|(id, h)| {
                positions
                    .iter()
                    .find(|(n, _)| n == id)
                    .map(|(_, r)| (*h, *r))
            })
            .collect();
    }
    let (controls, offset) = {
        let mut state = s.borrow_mut();
        state.laying_out = true;
        for (i, limit) in limits.iter().enumerate() {
            state.scroll[i] = state.scroll[i].clamp(0, *limit);
        }
        (state.layout.clone(), state.scroll)
    };
    unsafe {
        for (axis, bar) in [(0, SB_HORZ), (1, SB_VERT)] {
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE
                    | SIF_PAGE
                    | SIF_POS
                    | if axis == 1 {
                        SIF_DISABLENOSCROLL
                    } else {
                        SCROLLINFO_MASK(0)
                    },
                nMin: 0,
                nMax: limits[axis] + viewport[axis] - 1,
                nPage: viewport[axis] as u32,
                nPos: offset[axis],
                ..Default::default()
            };
            SetScrollInfo(hwnd, bar, &info, true);
        }
        // Do not copy old child pixels during thumb tracking. All geometry is
        // committed before the parent and children are erased and repainted.
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW | SWP_NOCOPYBITS;
        let mut batch = BeginDeferWindowPos(controls.len() as i32).ok();
        for (h, [x, y, w, hgt]) in &controls {
            if let Some(handle) = batch {
                batch = DeferWindowPos(
                    handle,
                    *h,
                    None,
                    x * dpi / 96 - offset[0],
                    y * dpi / 96 - offset[1],
                    w * dpi / 96,
                    hgt * dpi / 96,
                    flags,
                )
                .ok();
            }
        }
        let committed = batch.is_some_and(|handle| EndDeferWindowPos(handle).is_ok());
        if !committed {
            for (h, [x, y, w, hgt]) in controls {
                let _ = SetWindowPos(
                    h,
                    None,
                    x * dpi / 96 - offset[0],
                    y * dpi / 96 - offset[1],
                    w * dpi / 96,
                    hgt * dpi / 96,
                    flags,
                );
            }
        }
    }
    s.borrow_mut().laying_out = false;
    // Showing/hiding the fallback horizontal bar changes the vertical viewport.
    // Recompute its range after that synchronous WM_SIZE (guarded above).
    let mut after = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut after) }.is_ok()
        && (after.right != client.right || after.bottom != client.bottom)
    {
        layout(s);
    }
    unsafe {
        use windows::Win32::Graphics::Gdi::*;
        let _ = RedrawWindow(
            hwnd,
            None,
            None,
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
    }
}
fn scroll_message(s: &Shared, horizontal: bool, wp: WPARAM) {
    let axis = if horizontal { 0 } else { 1 };
    let hwnd = s.borrow().hwnd;
    let mut info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        ..Default::default()
    };
    if unsafe { GetScrollInfo(hwnd, if horizontal { SB_HORZ } else { SB_VERT }, &mut info) }
        .is_err()
    {
        return;
    }
    scroll_from_info(s, axis, (wp.0 & 0xffff) as u32, info);
}
fn scroll_from_info(s: &Shared, axis: usize, code: u32, info: SCROLLINFO) {
    let value = match code {
        0 => info.nPos - 32,
        1 => info.nPos + 32,
        2 => info.nPos - info.nPage as i32,
        3 => info.nPos + info.nPage as i32,
        4 | 5 => info.nTrackPos,
        6 => 0,
        7 => info.nMax,
        _ => info.nPos,
    };
    s.borrow_mut().scroll[axis] = value;
    layout(s);
}
fn reveal(s: &Shared, child: HWND) {
    let (hwnd, dpi, rect, mut offset) = {
        let state = s.borrow();
        (
            state.hwnd,
            state.dpi,
            state
                .layout
                .iter()
                .find(|(h, _)| *h == child)
                .map(|(_, r)| *r),
            state.scroll,
        )
    };
    let Some([x, y, w, h]) = rect else {
        return;
    };
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return;
    }
    // A dropdown's visible collapsed height, not its popup list height.
    let h = if h == 160 { 30 } else { h };
    for (axis, start, length, extent) in [(0, x, w, client.right), (1, y, h, client.bottom)] {
        let start = start * dpi / 96;
        let end = start + length * dpi / 96;
        if start < offset[axis] {
            offset[axis] = start;
        } else if end > offset[axis] + extent {
            offset[axis] = end - extent;
        }
    }
    s.borrow_mut().scroll = offset;
    layout(s);
}
fn replace_font(s: &Shared) {
    let (dpi, controls) = {
        let state = s.borrow();
        (state.dpi, state.controls.clone())
    };
    let font = unsafe {
        CreateFontW(
            -14 * dpi / 96,
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
            w!("Microsoft YaHei UI"),
        )
    };
    if font.0.is_null() {
        return;
    }
    let headings: Vec<_> = [26, 16]
        .into_iter()
        .map(|size| unsafe {
            CreateFontW(
                -size * dpi / 96,
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                CLEARTYPE_QUALITY.0 as u32,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                w!("Microsoft YaHei UI"),
            )
        })
        .collect();
    let (old, old_headings) = {
        let mut state = s.borrow_mut();
        (
            std::mem::replace(&mut state.font, font),
            std::mem::replace(&mut state.headings, headings.clone()),
        )
    };
    for (id, h) in controls {
        let selected = match id {
            10 => headings[0],
            1 | 3 => headings[1],
            _ => font,
        };
        let selected = if selected.0.is_null() { font } else { selected };
        unsafe {
            SendMessageW(h, WM_SETFONT, WPARAM(selected.0 as usize), LPARAM(1));
        }
    }
    for old in old_headings.into_iter().chain(Some(old)) {
        if !old.0.is_null() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
        }
    }
}
fn build(s: &Shared) -> Result<(), String> {
    unsafe {
        let hwnd = s.borrow().hwnd;
        let dpi = s.borrow().dpi;
        let scale = |v: i32| v * dpi / 96;
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
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Microsoft YaHei UI"),
        );
        s.borrow_mut().font = font;
        let add = |id: usize,
                   class: PCWSTR,
                   label: &str,
                   x,
                   y,
                   width,
                   height,
                   extra: u32|
         -> Result<HWND, String> {
            let label = wide(label);
            let h = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(label.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(extra),
                scale(x),
                scale(y),
                scale(width),
                scale(height),
                hwnd,
                HMENU(id as *mut _),
                GetModuleHandleW(None).map_err(|_| "无法创建控件。")?,
                None,
            )
            .map_err(|_| "无法创建控件。")?;
            SendMessageW(h, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            if !SetWindowSubclass(h, Some(keys), 1, hwnd.0 as usize).as_bool() {
                return Err("无法设置键盘导航。".into());
            }
            s.borrow_mut().controls.push((id, h));
            s.borrow_mut().layout.push((h, [x, y, width, height]));
            Ok(h)
        };
        let button = WS_TABSTOP.0 | BS_OWNERDRAW as u32;
        add(10, w!("STATIC"), "AI 帮写", 24, 20, 200, 32, 0)?;
        add(HELP, w!("BUTTON"), "使用帮助", 560, 20, 136, 32, button)?;
        add(
            11,
            w!("STATIC"),
            "自选模型 API · 在线处理",
            24,
            60,
            500,
            24,
            0,
        )?;
        add(1, w!("STATIC"), "01  写下要点", 40, 112, 620, 24, 0)?;
        add(
            12,
            w!("STATIC"),
            "最多500字 · 仅在你确认生成后发送",
            40,
            144,
            620,
            24,
            0,
        )?;
        let edit = WS_TABSTOP.0
            | WS_BORDER.0
            | WS_VSCROLL.0
            | ES_MULTILINE as u32
            | ES_AUTOVSCROLL as u32
            | ES_WANTRETURN as u32;
        let input = add(INPUT, w!("EDIT"), "", 40, 176, 640, 120, edit)?;
        SendMessageW(input, EM_SETLIMITTEXT, WPARAM(32767), LPARAM(0));
        for (id, label, items) in [
            (TONE, "语气", vec!["自然", "正式", "礼貌"]),
            (
                LENGTH,
                "长度",
                vec!["简短（50–100字）", "适中（100–200字）"],
            ),
            (SCRIPT, "输出字形", vec!["简体中文", "通用繁体中文"]),
        ] {
            add(id + 1000, w!("STATIC"), label, 40, 308, 180, 24, 0)?;
            let h = add(
                id,
                w!("COMBOBOX"),
                "",
                40,
                336,
                180,
                160,
                WS_TABSTOP.0 | CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0,
            )?;
            for item in items {
                let item = wide(item);
                SendMessageW(h, CB_ADDSTRING, WPARAM(0), LPARAM(item.as_ptr() as isize));
            }
        }
        add(GENERATE, w!("BUTTON"), "生成草稿", 40, 380, 140, 36, button)?;
        add(AGAIN, w!("BUTTON"), "重新生成", 192, 380, 140, 36, button)?;
        add(
            RESTORE,
            w!("BUTTON"),
            "恢复原任务",
            344,
            380,
            180,
            36,
            button,
        )?;
        add(3, w!("STATIC"), "02  核对与编辑结果", 40, 456, 640, 24, 0)?;
        add(
            13,
            w!("STATIC"),
            "尚无结果 · 完成后可编辑，最多800字",
            40,
            488,
            640,
            24,
            0,
        )?;
        let output = add(OUTPUT, w!("EDIT"), "", 40, 520, 640, 152, edit)?;
        SendMessageW(output, EM_SETLIMITTEXT, WPARAM(32767), LPARAM(0));
        add(COPY, w!("BUTTON"), "复制结果", 40, 688, 140, 36, button)?;
        add(
            APPLY,
            w!("BUTTON"),
            "插入到原位置（一次）",
            192,
            688,
            240,
            36,
            button,
        )?;
        add(
            RETRY,
            w!("BUTTON"),
            "智谱繁忙时自动等待后重试（最多2次）",
            40,
            1132,
            640,
            44,
            BS_AUTOCHECKBOX as u32 | BS_MULTILINE as u32 | WS_TABSTOP.0,
        )?;
        add(
            RETRY_NOTE,
            w!("STATIC"),
            "每次再次发送都可能计入账号额度。",
            40,
            1180,
            640,
            44,
            0,
        )?;
        add(STATUS, w!("STATIC"), "", 24, 848, 672, 72, 0)?;
        add(
            CONNECTION,
            w!("BUTTON"),
            "管理模型 API…",
            24,
            932,
            180,
            32,
            button,
        )?;
        if SetTimer(hwnd, TIMER, 80, None) == 0 {
            return Err("无法启动面板更新。".into());
        }
        replace_font(s);
        layout(s);
        Ok(())
    }
}

fn render(s: &Shared) {
    if !alive(s) {
        return;
    }
    let (controls, input, result, status, running, readonly, task, script, apply_ok) = {
        let mut state = s.borrow_mut();
        if state.rendering {
            return;
        }
        state.rendering = true;
        let p = &state.page;
        let running = busy(&state);
        let mut status = format!(
            "要点 {}/500 字 · 结果 {}/800 字\n{}",
            p.input.chars().count(),
            p.result.as_ref().map_or(0, |r| r.text.chars().count()),
            state.status
        );
        if p.input.chars().count() > 500 {
            status.push_str(" 原输入已保留：超过500字，请缩短后生成。");
        }
        if let Some(r) = &p.result {
            if r.text.chars().count() > 800 {
                status.push_str(" 编辑结果超过800字，请缩短后应用。");
            }
            if r.stale {
                status.push_str(" 上次结果：输入或选项已变化；请点“恢复原任务”后核对目标。");
            }
            if r.edited {
                status.push_str(" 已手动编辑。");
            }
            if !r.complete {
                status.push_str(" 未完成片段，不可应用。");
            }
            if r.consumed {
                status.push_str(" 本次应用已消费，不可重复应用。");
            }
        }
        (
            state.controls.clone(),
            p.input.clone(),
            p.result
                .as_ref()
                .map(|r| r.text.clone())
                .unwrap_or_default(),
            status,
            running,
            p.selection,
            p.task,
            p.script,
            can_apply(p) && state.apply.is_some(),
        )
    };
    for (id, h) in controls {
        if !alive(s) {
            break;
        }
        match id {
            INPUT => {
                if text(h) != input {
                    set_text(h, &input);
                }
                unsafe {
                    readonly_changed(h, readonly || running);
                }
            }
            OUTPUT => {
                update_output_text(h, &result);
                let editable = {
                    let state = s.borrow();
                    state.page.result.as_ref().is_some_and(|r| r.complete)
                };
                unsafe {
                    readonly_changed(h, running || !editable);
                }
            }
            STATUS => set_text(h, &status),
            13 => set_text(
                h,
                if running {
                    "正在生成 · 完成前不可编辑或插入"
                } else if result.is_empty() {
                    "尚无结果 · 完成后可编辑，最多800字"
                } else {
                    "请核对事实 · 可以编辑，最多800字"
                },
            ),
            GENERATE => {
                set_text(
                    h,
                    if running {
                        "停止生成"
                    } else {
                        "生成草稿"
                    },
                );
                unsafe {
                    enable_changed(h, running || valid_input(&input));
                }
            }
            AGAIN => unsafe {
                enable_changed(h, !running && valid_input(&input));
            },
            RESTORE => {
                let available = {
                    let state = s.borrow();
                    state
                        .page
                        .result
                        .as_ref()
                        .is_some_and(|r| r.complete && r.stale)
                };
                unsafe {
                    enable_changed(h, !running && available);
                }
            }
            COPY => unsafe {
                enable_changed(h, !running && !result.is_empty());
            },
            APPLY => {
                set_text(h, "插入到原位置（一次）");
                unsafe {
                    enable_changed(h, !running && apply_ok);
                }
            }
            TONE => {
                if let Task::Draft { tone, .. } = task {
                    select(
                        h,
                        match tone {
                            Tone::Natural => 0,
                            Tone::Formal => 1,
                            Tone::Polite => 2,
                        },
                    );
                }
                unsafe {
                    enable_changed(h, !running && matches!(task, Task::Draft { .. }));
                }
            }
            LENGTH => {
                if let Task::Draft { length, .. } = task {
                    select(h, if length == Length::Short { 0 } else { 1 });
                }
                unsafe {
                    enable_changed(h, !running && matches!(task, Task::Draft { .. }));
                }
            }
            SCRIPT => {
                select(
                    h,
                    if script == ChineseScript::Simplified {
                        0
                    } else {
                        1
                    },
                );
                unsafe {
                    enable_changed(h, !running);
                }
            }
            RETRY | CONNECTION => unsafe {
                enable_changed(h, !running);
            },
            _ => {}
        }
    }
    s.borrow_mut().rendering = false;
}
fn command(s: &Shared, id: usize, notification: u32) {
    if !alive(s) || s.borrow().rendering {
        return;
    }
    if notification == EN_CHANGE && (id == INPUT || id == OUTPUT) {
        let value = text(control(s, id));
        {
            let mut state = s.borrow_mut();
            let p = &mut state.page;
            if id == INPUT {
                p.input = value;
                if let Some(r) = &mut p.result {
                    r.stale = true;
                }
            } else if let Some(r) = &mut p.result {
                r.text = value;
                r.edited = true;
            }
        }
        render(s);
        return;
    }
    if notification == CBN_SELCHANGE && [TONE, LENGTH, SCRIPT].contains(&id) {
        let tone = selection(control(s, TONE));
        let length = selection(control(s, LENGTH));
        let script = selection(control(s, SCRIPT));
        {
            let mut state = s.borrow_mut();
            if busy(&state) {
                return;
            }
            let p = &mut state.page;
            p.task = Task::Draft {
                tone: match tone {
                    1 => Tone::Formal,
                    2 => Tone::Polite,
                    _ => Tone::Natural,
                },
                length: if length == 0 {
                    Length::Short
                } else {
                    Length::Medium
                },
            };
            p.script = if script == 0 {
                ChineseScript::Simplified
            } else {
                ChineseScript::Traditional
            };
            if let Some(r) = &mut p.result {
                r.stale = true;
            }
        }
        render(s);
        return;
    }
    if notification != BN_CLICKED {
        return;
    }
    match id {
        GENERATE => {
            if busy(&s.borrow()) {
                stop(s);
            } else {
                generate(s);
            }
        }
        AGAIN => generate(s),
        RESTORE => {
            if busy(&s.borrow()) {
                return;
            }
            {
                let mut state = s.borrow_mut();
                state.page.restore();
            }
            notice(
                s,
                "已恢复上次结果的原文、任务及字形；来源和权限不变。已解除绑定的任务仍仅可复制，有效绑定应用时仍须核验原目标。",
            );
        }
        HELP => {
            let owner = s.borrow().hwnd;
            if let Err(error) = show_setup_help(owner) {
                notice(s, &error);
            }
        }
        CONNECTION => {
            let (hwnd, store) = {
                let state = s.borrow();
                (state.hwnd, state.preference_store.clone())
            };
            if let Some(store) = store {
                if let Err(error) = super::model_api_ui::show_model_api_settings(hwnd, store) {
                    notice(s, &error);
                }
                if alive(s) {
                    let store = s.borrow().preference_store.clone();
                    let target = crate::writing::config::load_active(store.as_ref())
                        .and_then(|p| p.display_target())
                        .map(|target| format!("当前连接：{target}；仅生成时发送。"))
                        .unwrap_or_else(|_| "当前无有效模型连接；不会发送文本。".into());
                    notice(s, &target);
                }
            } else {
                notice(s, "无法读取模型 API 配置。");
            }
        }
        COPY => {
            if busy(&s.borrow()) {
                return;
            }
            let value = text(control(s, OUTPUT));
            let hwnd = s.borrow().hwnd;
            notice(
                s,
                if write_clipboard(hwnd, &value).is_ok() {
                    "已复制。"
                } else {
                    "复制失败，剪贴板可能正被其他程序占用。"
                },
            );
        }
        APPLY => apply_result(s),
        _ => {}
    }
}
fn generate(s: &Shared) {
    generate_with(
        s,
        super::send_confirmation::confirm_send_for,
        WritingJob::start_with_profile,
    );
}

fn generate_with(
    s: &Shared,
    confirm: impl FnOnce(
        HWND,
        &crate::writing::config::ApiProfile,
    ) -> Result<super::send_confirmation::SendDecision, String>,
    start: impl FnOnce(
        WritingRequest,
        crate::writing::config::ApiProfile,
        bool,
    ) -> Result<WritingJob, crate::writing::WritingError>,
) {
    if !alive(s) || busy(&s.borrow()) {
        return;
    }
    let store = s.borrow().preference_store.clone();
    if !crate::writing::config::online_enabled(store.as_ref()) {
        notice(s, &crate::writing::WritingError::OnlineDisabled.to_string());
        return;
    }
    let profile = match crate::writing::config::load_active(store.as_ref()) {
        Ok(profile) => profile,
        Err(error) => {
            notice(s, &error.to_string());
            return;
        }
    };
    let request = {
        let mut state = s.borrow_mut();
        let p = &state.page;
        if !valid_input(&p.input) {
            drop(state);
            notice(s, "请输入1至500字；超限原文不会被截断发送。");
            return;
        }
        let task = p.task;
        let script = p.script;
        let input = p.input.clone();
        state.next = state.next.wrapping_add(1).max(1);
        state.pending = true;
        WritingRequest {
            id: state.next,
            task,
            script,
            input,
        }
    };
    render(s);
    let hwnd = s.borrow().hwnd;
    let answer = if crate::writing::consent::confirmation_required_for(store.as_ref(), &profile) {
        confirm(hwnd, &profile)
    } else {
        Ok(super::send_confirmation::SendDecision::Send {
            dont_ask_again: false,
        })
    };
    if !alive(s) || !s.borrow().pending || s.borrow().next != request.id {
        return;
    }
    if !crate::writing::config::online_enabled(store.as_ref()) {
        s.borrow_mut().pending = false;
        notice(s, &crate::writing::WritingError::OnlineDisabled.to_string());
        return;
    }
    if !crate::writing::config::is_current(store.as_ref(), &profile) {
        s.borrow_mut().pending = false;
        notice(s, "当前连接已变化，本次未发送；请重新生成并确认。");
        return;
    }
    let decision = match answer {
        Ok(decision) => decision,
        Err(error) => {
            s.borrow_mut().pending = false;
            notice(s, &error);
            return;
        }
    };
    match super::send_confirmation::apply_decision_for(store.as_ref(), &profile, decision) {
        Ok(false) => {
            s.borrow_mut().pending = false;
            notice(s, "未发送文本。");
            return;
        }
        Err(_) => unsafe {
            MessageBoxW(
                hwnd,
                w!("本次可发送，未记住偏好。"),
                w!("发送偏好"),
                MB_OK | MB_ICONWARNING,
            );
        },
        Ok(true) => {}
    }
    if !alive(s)
        || !s.borrow().pending
        || s.borrow().next != request.id
        || !crate::writing::config::is_current(store.as_ref(), &profile)
    {
        s.borrow_mut().pending = false;
        return;
    }
    // No state borrow spans credential access or worker construction.
    if !crate::writing::config::online_enabled(store.as_ref()) {
        s.borrow_mut().pending = false;
        notice(s, &crate::writing::WritingError::OnlineDisabled.to_string());
        return;
    }
    let retry = retry_requested(s);
    let job = start(request.clone(), profile.clone(), retry);
    if !alive(s) {
        drop(job);
        return;
    }
    {
        let mut state = s.borrow_mut();
        state.pending = false;
        match job {
            Ok(job) => {
                state.backup = state.page.result.clone().filter(|r| r.complete);
                state.page.result = Some(ResultText {
                    origin: state.page.origin(),
                    id: request.id,
                    text: String::new(),
                    complete: false,
                    consumed: false,
                    stale: false,
                    edited: false,
                });
                state.request = Some(request.id);
                state.job = Some(job);
                state.status = format!(
                    "正在连接 {}；可停止生成。",
                    profile
                        .display_target()
                        .unwrap_or_else(|_| "选定模型服务".into())
                );
            }
            Err(error) => state.status = error.to_string(),
        }
    }
    render(s);
}
fn poll(s: &Shared) {
    let update = {
        let state = s.borrow();
        state.job.as_ref().and_then(|j| j.poll())
    };
    let Some(update) = update else {
        return;
    };
    receive_update(s, update);
}
fn receive_update(s: &Shared, update: crate::writing::worker::Update) {
    let mut finished = None;
    {
        let mut state = s.borrow_mut();
        let Some(id) = state.request else {
            return;
        };
        if id != update.id {
            return;
        }
        let terminal = matches!(update.phase, Phase::Complete | Phase::Failed(_));
        if let Some(result) = &mut state.page.result {
            result.text = update.text;
            result.complete = update.phase == Phase::Complete;
        }
        state.status = match update.phase {
            Phase::Connecting => "正在连接选定模型服务。".into(),
            Phase::Generating => "正在生成；结果尚未完成。".into(),
            Phase::Waiting { attempt, seconds } => {
                format!("繁忙等待：准备第{attempt}次尝试，剩余{seconds}秒；可停止。")
            }
            Phase::Complete => "生成完成；请核对事实后复制或明确应用。".into(),
            Phase::Failed(error) => error.to_string(),
        };
        if terminal {
            if matches!(update.phase, Phase::Failed(_)) {
                if let Some(previous) = state.backup.take() {
                    state.page.result = Some(previous);
                    state.status.push_str(" 已保留上次完整结果（原任务）。");
                }
            }
            state.backup = None;
            state.request = None;
            finished = state.job.take();
        }
    }
    drop(finished);
    render(s);
}
fn stop(s: &Shared) {
    let job = {
        let mut state = s.borrow_mut();
        if state.pending {
            state.pending = false;
            state.next = state.next.wrapping_add(1);
        }
        state.request = None;
        let job = state.job.take();
        if let Some(previous) = state.backup.take() {
            state.page.result = Some(previous);
        }
        state.status = "已停止；片段未完成。已发送文本无法撤回，服务商可能继续处理或计额。".into();
        job
    };
    drop(job);
    render(s);
}
fn apply_result(s: &Shared) {
    let (mut callback, value, id) = {
        let mut state = s.borrow_mut();
        if busy(&state) {
            return;
        }
        if !can_apply(&state.page) {
            return;
        }
        let Some(callback) = state.apply.take() else {
            return;
        };
        let (value, id) = state.page.consume().unwrap();
        (callback, value, id)
    };
    render(s);
    if !alive(s) {
        drop(callback);
        return;
    }
    let outcome = callback(&value);
    if !alive(s) {
        drop(callback);
        return;
    }
    {
        let mut state = s.borrow_mut();
        state.apply = Some(callback);
        if state.page.result.as_ref().is_some_and(|r| r.id == id) {
            state.status = if outcome.is_ok() {
                "已应用；不会自动发送，也不能重复应用本结果。"
            } else {
                "目标校验或写入失败；本次应用已消费以防部分写入后重复，可以复制结果。"
            }
            .into();
        }
    }
    render(s);
}

// Explicit write-only clipboard API. Locally declared to avoid adding Cargo features.
#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(hwnd: *mut std::ffi::c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, data: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}
#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut std::ffi::c_void;
    fn GlobalLock(data: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn GlobalUnlock(data: *mut std::ffi::c_void) -> i32;
    fn GlobalFree(data: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}
fn write_clipboard(hwnd: HWND, value: &str) -> Result<(), ()> {
    unsafe {
        let value = wide(value);
        let memory = GlobalAlloc(0x0002, value.len() * 2);
        if memory.is_null() {
            return Err(());
        }
        let destination = GlobalLock(memory);
        if destination.is_null() {
            GlobalFree(memory);
            return Err(());
        }
        std::ptr::copy_nonoverlapping(value.as_ptr(), destination.cast::<u16>(), value.len());
        GlobalUnlock(memory);
        if OpenClipboard(hwnd.0) == 0 {
            GlobalFree(memory);
            return Err(());
        }
        let success = EmptyClipboard() != 0 && !SetClipboardData(13, memory).is_null();
        CloseClipboard();
        if success {
            Ok(())
        } else {
            GlobalFree(memory);
            Err(())
        }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "owned panel and isolated Store; FLOWER_CONSENT_TEST_DIR required"]
    fn online_gate_reloads_open_panel_and_after_confirmation() {
        let dir = std::path::PathBuf::from(std::env::var_os("FLOWER_CONSENT_TEST_DIR").unwrap());
        let store = crate::settings::SettingsStore::new(dir.join("writing-online-gate.json"));
        let mut settings = crate::settings::Settings::default();
        store.save(&settings).unwrap();
        assert!(super::open_panel_with_store(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None,
            Some(store.clone())
        )
        .is_err());
        settings.online_ai_enabled = true;
        store.save(&settings).unwrap();
        let panel = super::open_panel_with_store(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None,
            Some(store.clone()),
        )
        .unwrap();
        panel.state.borrow_mut().page.input = "合成测试要点".into();
        settings.online_ai_enabled = false;
        store.save(&settings).unwrap();
        generate_with(
            &panel.state,
            |_, _| panic!("disabled must not confirm"),
            |_, _, _| panic!("disabled must not send"),
        );
        assert_eq!(
            panel.state.borrow().status,
            crate::writing::WritingError::OnlineDisabled.to_string()
        );
        settings.online_ai_enabled = true;
        store.save(&settings).unwrap();
        generate_with(
            &panel.state,
            |_, _| {
                let mut latest = store.load().unwrap();
                latest.online_ai_enabled = false;
                store.save(&latest).unwrap();
                Ok(super::super::send_confirmation::SendDecision::Send {
                    dont_ask_again: false,
                })
            },
            |_, _, _| panic!("modal revocation must not send"),
        );
        assert_eq!(
            panel.state.borrow().status,
            crate::writing::WritingError::OnlineDisabled.to_string()
        );
        assert!(!panel.state.borrow().pending);
        assert!(panel.state.borrow().job.is_none());
    }

    #[test]
    #[ignore = "isolated store and owned panel; FLOWER_CONSENT_TEST_DIR required"]
    fn model_snapshot_live_panel_reloads_scopes_and_cancels_changed_target() {
        use super::super::send_confirmation::SendDecision;
        use crate::writing::{config, consent};
        let dir = std::path::PathBuf::from(std::env::var_os("FLOWER_CONSENT_TEST_DIR").unwrap());
        let store = crate::settings::SettingsStore::new(dir.join("snapshot-panel.json"));
        let mut settings = crate::settings::Settings {
            online_ai_enabled: true,
            ..crate::settings::Settings::default()
        };
        store.save(&settings).unwrap();
        let panel = super::open_panel_with_store(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None,
            Some(store.clone()),
        )
        .unwrap();
        panel.state.borrow_mut().page.input = "合成要点".into();
        let old = config::load_active(Some(&store)).unwrap();
        consent::remember_for(&store, &old, true).unwrap();
        // An already-open panel reads the latest connection, not the one at creation.
        settings = store.load().unwrap();
        settings.model_api.profiles[0].model = "changed-model".into();
        store.save(&settings).unwrap();
        let selected = config::load_active(Some(&store)).unwrap();
        let prompts = std::cell::Cell::new(0);
        generate_with(
            &panel.state,
            |_, profile| {
                prompts.set(prompts.get() + 1);
                assert_eq!(profile, &selected);
                Ok(SendDecision::Cancel)
            },
            |_, _, _| panic!("cancel must not start or load a key"),
        );
        assert_eq!(prompts.get(), 1);
        assert_eq!(
            store.load().unwrap().writing_send_consent,
            Some(old.scope().unwrap())
        );
        generate_with(
            &panel.state,
            |_, profile| {
                assert_eq!(profile, &selected);
                let mut latest = store.load().unwrap();
                latest.model_api.profiles[0].model = "another-model".into();
                store.save(&latest).unwrap();
                Ok(SendDecision::Send {
                    dont_ask_again: true,
                })
            },
            |_, _, _| panic!("changed destination must not start"),
        );
        assert!(!panel.state.borrow().pending);
        assert!(panel.state.borrow().job.is_none());
        assert_eq!(
            store.load().unwrap().writing_send_consent,
            Some(old.scope().unwrap())
        );
        // Missing store / no active connection fail closed without asking or sending.
        panel.state.borrow_mut().preference_store = None;
        generate_with(
            &panel.state,
            |_, _| panic!("missing store"),
            |_, _, _| panic!("missing store"),
        );
        panel.state.borrow_mut().preference_store = Some(store.clone());
        let mut latest = store.load().unwrap();
        latest.model_api.active_id = None;
        store.save(&latest).unwrap();
        generate_with(
            &panel.state,
            |_, _| panic!("no active"),
            |_, _, _| panic!("no active"),
        );
        assert!(panel.state.borrow().job.is_none());
    }

    #[test]
    #[ignore = "isolated store and owned panel; FLOWER_CONSENT_TEST_DIR required"]
    fn model_snapshot_unpersisted_edits_never_change_sent_profile() {
        use super::super::send_confirmation::SendDecision;
        use crate::writing::config;
        let dir = std::path::PathBuf::from(std::env::var_os("FLOWER_CONSENT_TEST_DIR").unwrap());
        let store = crate::settings::SettingsStore::new(dir.join("unsaved-panel.json"));
        store
            .save(&crate::settings::Settings {
                online_ai_enabled: true,
                ..Default::default()
            })
            .unwrap();
        let panel = super::open_panel_with_store(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None,
            Some(store.clone()),
        )
        .unwrap();
        panel.state.borrow_mut().page.input = "合成要点".into();
        let saved = config::load_active(Some(&store)).unwrap();
        let mut unsaved = saved.clone();
        unsaved.base_url = "https://unsaved.example/v1".into();
        let starts = std::cell::Cell::new(0);
        generate_with(
            &panel.state,
            |_, profile| {
                assert_ne!(profile, &unsaved);
                assert_eq!(profile, &saved);
                Ok(SendDecision::Send {
                    dont_ask_again: false,
                })
            },
            |_, profile, _| {
                assert_eq!(profile, saved);
                starts.set(starts.get() + 1);
                // A fake start: never consult credentials or construct a network task.
                Err(crate::writing::WritingError::InvalidConfiguration)
            },
        );
        assert_eq!(starts.get(), 1);
        assert!(panel.state.borrow().job.is_none());
    }
    #[test]
    #[ignore = "owned confirmation screenshot; explicit artifact directory required"]
    fn send_confirmation_screenshot() {
        let directory =
            std::path::PathBuf::from(std::env::var_os("FLOWER_WRITING_FIXTURE_DIR").unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let decision = super::super::send_confirmation::confirm_with(HWND::default(), |hwnd| {
            write_client_bitmap(hwnd, &directory.join("send-confirmation.bmp"));
            unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        })
        .unwrap();
        assert_eq!(
            decision,
            super::super::send_confirmation::SendDecision::Cancel
        );
    }

    #[test]
    #[ignore = "owned panels with isolated SettingsStore; explicit scratch directory required"]
    fn consent_restore_reloads_live_panel() {
        let directory =
            std::path::PathBuf::from(std::env::var_os("FLOWER_CONSENT_TEST_DIR").unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let store = crate::settings::SettingsStore::new(directory.join("live-panel.json"));
        store
            .save(&crate::settings::Settings {
                online_ai_enabled: true,
                ..Default::default()
            })
            .unwrap();
        let panel = super::open_panel_with_store(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None,
            Some(store.clone()),
        )
        .unwrap();
        crate::writing::consent::remember(&store, true).unwrap();
        let new_panel = super::open_panel_with_store(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None,
            Some(store.clone()),
        )
        .unwrap();
        assert!(!crate::writing::consent::confirmation_required(
            new_panel.state.borrow().preference_store.as_ref()
        ));
        crate::writing::consent::remember(&store, false).unwrap();
        assert!(crate::writing::consent::confirmation_required(
            panel.state.borrow().preference_store.as_ref()
        ));
    }
    // Native fixtures never discover the real current-user settings location.
    fn open_panel(
        task: Task,
        script: ChineseScript,
        source: PanelSource,
        apply: Option<ApplyCallback>,
    ) -> Result<WritingPanel, String> {
        require_draft(task)?;
        let directory = std::env::var_os("FLOWER_WRITING_FIXTURE_DIR")
            .or_else(|| std::env::var_os("FLOWER_CONSENT_TEST_DIR"))
            .expect("explicit isolated fixture directory");
        let store = crate::settings::SettingsStore::new(
            std::path::PathBuf::from(directory).join("writing-ui-authorized.json"),
        );
        store
            .save(&crate::settings::Settings {
                online_ai_enabled: true,
                ..Default::default()
            })
            .unwrap();
        super::open_panel_with_store(task, script, source, apply, Some(store))
    }
    use super::*;
    fn edit_control(hwnd: HWND, value: &str) {
        use windows::Win32::UI::Controls::{EM_REPLACESEL, EM_SETSEL};
        let value = wide(value);
        unsafe {
            SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
            SendMessageW(
                hwnd,
                EM_REPLACESEL,
                WPARAM(1),
                LPARAM(value.as_ptr() as isize),
            );
        }
    }
    #[test]
    fn input_boundary_preserves_overflow() {
        assert!(valid_input(&"字".repeat(500)));
        assert!(!valid_input(&"字".repeat(501)));
        assert!(!valid_input(" \n"));
    }
    #[test]
    fn draft_only_rejects_polish_before_creating_any_window() {
        assert!(require_draft(Task::draft()).is_ok());
        assert!(require_draft(Task::polish()).is_err());
        assert!(open_panel(
            Task::polish(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None
        )
        .is_err());
        assert!(run_writing_app(Task::polish()).is_err());
    }
    #[test]
    fn responsive_controls_have_no_horizontal_overflow() {
        for width in [360, 400, 619, 620, 740, 1000] {
            for (id, [x, _, w, _]) in positions(width) {
                assert!(x >= 0 && x + w <= width, "control {id} at width {width}");
            }
            assert_eq!(
                content_height(width, true) - content_height(width, false),
                0
            );
        }
        assert!(!crate::writing::help::SETUP_GUIDE.is_empty());
    }
    #[test]
    fn tiny_work_area_horizontal_fallback_reaches_actions_at_high_dpi() {
        for dpi in [96, 144, 192] {
            for width in [260, 300, 360] {
                let viewport = [width * dpi / 96, 260 * dpi / 96];
                let limit = scroll_limit(dpi, viewport);
                assert_eq!(limit[0], (360 - width).max(0) * dpi / 96);
                for (id, [x, y, w, h]) in positions(width.max(360)) {
                    let h = if h == 160 { 30 } else { h };
                    let right = (x + w) * dpi / 96;
                    let bottom = (y + h) * dpi / 96;
                    assert!(right <= viewport[0] + limit[0], "control {id}");
                    assert!(bottom <= viewport[1] + limit[1], "control {id}");
                    if [HELP, CONNECTION, SAVE, DELETE, GENERATE, APPLY].contains(&id) {
                        // Each action fits wholly at a reachable horizontal offset.
                        let offset = (right - viewport[0]).max(0).min(limit[0]);
                        assert!(x * dpi / 96 >= offset, "action {id}");
                        assert!(right - offset <= viewport[0], "action {id}");
                    }
                }
            }
        }
    }
    #[test]
    fn keyboard_never_applies() {
        assert_eq!(
            key_command(VK_RETURN.0, true, false, false),
            Some(KEY_GENERATE)
        );
        assert_eq!(
            key_command(VK_ESCAPE.0, false, false, false),
            Some(KEY_ESCAPE)
        );
        assert_eq!(key_command(VK_RETURN.0, true, true, false), None);
        assert_eq!(key_command(VK_RETURN.0, false, false, false), None);
    }
    #[test]
    fn incomplete_stale_and_consumed_never_apply() {
        let mut p = Page {
            binding_id: 2,
            task: Task::draft(),
            script: ChineseScript::Simplified,
            input: "原文".into(),
            selection: false,
            bound: true,
            result: Some(ResultText {
                origin: ResultOrigin {
                    input: "原文".into(),
                    task: Task::draft(),
                    script: ChineseScript::Simplified,
                    selection: false,
                    bound: true,
                    binding_id: 2,
                },
                id: 1,
                text: "结果".into(),
                complete: true,
                consumed: false,
                stale: false,
                edited: false,
            }),
        };
        assert!(can_apply(&p));
        p.result.as_mut().unwrap().stale = true;
        assert!(!can_apply(&p));
        p.result.as_mut().unwrap().stale = false;
        p.result.as_mut().unwrap().consumed = true;
        assert!(!can_apply(&p));
        p.result.as_mut().unwrap().consumed = false;
        p.result.as_mut().unwrap().complete = false;
        assert!(!can_apply(&p));
    }
    fn completed_page() -> Page {
        let mut p = Page {
            binding_id: 2,
            task: Task::draft(),
            script: ChineseScript::Simplified,
            input: "原任务文本".into(),
            selection: false,
            bound: true,
            result: None,
        };
        p.result = Some(ResultText {
            origin: p.origin(),
            id: 7,
            text: "完整结果".into(),
            complete: true,
            consumed: false,
            stale: false,
            edited: false,
        });
        p
    }
    #[test]
    fn changed_input_restore_preserves_existing_authority() {
        let mut p = completed_page();
        p.input = "新任务".into();
        p.result.as_mut().unwrap().stale = true;
        assert!(!can_apply(&p));
        assert!(p.restore());
        assert_eq!(p.input, "原任务文本");
        assert!(!p.selection && p.bound);
        assert_eq!(p.binding_id, 2);
        assert!(can_apply(&p)); // Actual application still invokes root target validation.
    }
    #[test]
    fn manual_revocation_survives_restore_and_failed_regeneration() {
        let mut p = completed_page();
        p.selection = false;
        p.bound = false;
        p.binding_id = 0;
        p.input = "手动文本".into();
        p.result.as_mut().unwrap().stale = true;
        assert!(p.restore());
        assert_eq!(p.input, "原任务文本");
        assert!(!p.selection && !p.bound);
        assert_eq!(p.binding_id, 0);
        assert!(!can_apply(&p));
        assert!(p.consume().is_none());
        p.input = "另一次手动任务".into();
        p.result.as_mut().unwrap().stale = true;
        let backup = p.result.clone();
        p.result = Some(ResultText {
            origin: p.origin(),
            id: 8,
            text: "失败片段".into(),
            complete: false,
            consumed: false,
            stale: false,
            edited: false,
        });
        p.result = backup;
        assert!(p.restore());
        assert!(!p.selection && !p.bound);
        assert_eq!(p.binding_id, 0);
        assert!(!can_apply(&p));
        assert!(p.consume().is_none());
    }
    #[test]
    fn failed_new_task_restores_old_task_identity() {
        let mut p = completed_page();
        p.task = Task::Draft {
            tone: Tone::Formal,
            length: Length::Medium,
        };
        p.script = ChineseScript::Traditional;
        p.result.as_mut().unwrap().stale = true;
        let backup = p.result.clone();
        p.result = Some(ResultText {
            origin: p.origin(),
            id: 8,
            text: "失败片段".into(),
            complete: false,
            consumed: false,
            stale: false,
            edited: false,
        });
        // Same failure transition used by poll: reinstall the immutable backup.
        p.result = backup;
        assert!(!can_apply(&p));
        assert!(p.restore());
        assert_eq!(p.task, Task::draft());
        assert_eq!(p.script, ChineseScript::Simplified);
        assert_eq!(p.result.as_ref().unwrap().id, 7);
        assert!(can_apply(&p));
    }
    #[test]
    fn edited_overflow_and_consumption_are_fail_closed() {
        let mut p = completed_page();
        p.result.as_mut().unwrap().text = "字".repeat(800);
        assert!(can_apply(&p));
        p.result.as_mut().unwrap().text = "字".repeat(801);
        assert!(p.consume().is_none());
        p.result.as_mut().unwrap().text = "可用结果".into();
        assert!(p.consume().is_some()); // Consumption precedes callback, including failure.
        assert!(p.consume().is_none());
        p.result.as_mut().unwrap().stale = true;
        assert!(p.restore());
        assert!(!can_apply(&p));
    }
    #[test]
    fn small_work_area_dpi_has_scroll_reachability() {
        for dpi in [96, 120, 144, 192] {
            let (w, h) = initial_size(dpi, 1366, 720);
            assert!(w <= 1366 && h <= 720);
            let limit = scroll_limit(dpi, [w - 40, h - 80]);
            assert!(limit[1] > 0);
            assert!(696 * dpi / 96 - limit[1] <= h - 80);
            assert_eq!(limit[0], 0);
        }
    }
    #[test]
    #[ignore = "creates only owned native windows; requires FLOWER_WRITING_FIXTURE_DIR in session artifacts"]
    fn writing_native_isolated_fixture() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled};
        let directory = std::path::PathBuf::from(
            std::env::var_os("FLOWER_WRITING_FIXTURE_DIR")
                .expect("explicit artifact directory required"),
        );
        std::fs::create_dir_all(&directory).unwrap();
        let count = Rc::new(std::cell::Cell::new(0));
        let calls = count.clone();
        let mut panel = open_panel(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Insertion,
            Some(Box::new(move |_| {
                calls.set(calls.get() + 1);
                Err("synthetic partial write".into())
            })),
        )
        .unwrap();
        let s = panel.state.clone();
        for id in [TONE, SCRIPT] {
            assert_eq!(
                unsafe { SendMessageW(control(&s, id), CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 },
                0,
                "default choice must be visibly selected"
            );
        }
        assert!(control(&s, 116).0.is_null());
        assert!(control(&s, 127).0.is_null());
        assert_eq!(
            unsafe { SendMessageW(control(&s, RETRY), BM_GETCHECK, WPARAM(0), LPARAM(0)).0 },
            0
        );
        assert!(!retry_requested(&s));
        unsafe {
            SendMessageW(control(&s, RETRY), BM_SETCHECK, WPARAM(1), LPARAM(0));
        }
        assert!(retry_requested(&s));
        unsafe {
            SendMessageW(control(&s, RETRY), BM_SETCHECK, WPARAM(0), LPARAM(0));
        }
        assert!(!s.borrow().connection_open);
        // The default work-area-sized window can scroll to its collapsed entry.
        reveal(&s, control(&s, CONNECTION));
        assert_control_visible(&s, CONNECTION);
        assert_eq!(s.borrow().scroll[0], 0);
        reveal(&s, control(&s, HELP));
        for id in [KEY, SAVE, DELETE] {
            assert!(!unsafe { IsWindowVisible(control(&s, id)) }.as_bool());
        }
        assert!(panel.show(Task::polish()).is_err());
        // Selection replacement permission is never repurposed as insertion permission.
        let mut selection_panel = open_panel(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Selection("合成选区，不得用于插入授权".into()),
            Some(Box::new(|_| panic!("selection must never grant insertion"))),
        )
        .unwrap();
        assert!(!selection_panel.state.borrow().page.bound);
        assert!(selection_panel.state.borrow().apply.is_none());
        selection_panel.close();
        {
            let mut state = s.borrow_mut();
            state.page = completed_page();
            state.page.input = "请写一条周五下午三点举行项目例会的通知，地点是第二会议室。".into();
            state.page.result.as_mut().unwrap().text =
                "各位同事：本周五下午三点将在第二会议室举行项目例会，请大家提前整理本周进展与待讨论事项，准时参加。".into();
            state.status = "合成示例 · 已完成，可核对并编辑。未调用在线服务。".into();
        }
        render(&s);
        write_client_bitmap(panel.hwnd(), &directory.join("writing-draft.bmp"));
        write_bitmap(
            panel.hwnd(),
            &directory.join("writing-normal-client.bmp"),
            true,
        );
        // Native EDIT thumb tracking, wheel and keyboard scrolling of synthetic
        // multiline content, independent of the parent's large-range scrollbar.
        let original = s.borrow().page.result.as_ref().unwrap().text.clone();
        s.borrow_mut().page.result.as_mut().unwrap().text = (0..100)
            .map(|n| format!("合成第{n}行：滚动显示应清晰，无旧像素残留。\r\n"))
            .collect();
        render(&s);
        unsafe {
            use windows::Win32::UI::Controls::{EM_LINESCROLL, EM_SETSEL};
            use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_UP};
            let output = control(&s, OUTPUT);
            for position in (0..80).chain((0..80).rev()) {
                SendMessageW(
                    output,
                    WM_VSCROLL,
                    WPARAM(SB_THUMBTRACK.0 as usize | (position << 16)),
                    LPARAM(0),
                );
                SendMessageW(
                    output,
                    WM_MOUSEWHEEL,
                    WPARAM(((120i16 as u16 as u32) << 16) as usize),
                    LPARAM(0),
                );
                SendMessageW(output, WM_KEYDOWN, WPARAM(VK_DOWN.0 as usize), LPARAM(0));
                SendMessageW(output, WM_KEYDOWN, WPARAM(VK_UP.0 as usize), LPARAM(0));
            }
            SendMessageW(output, EM_SETSEL, WPARAM(0), LPARAM(0));
            SendMessageW(output, EM_LINESCROLL, WPARAM(0), LPARAM(-10000));
        }
        s.borrow_mut().page.result.as_mut().unwrap().text = original;
        render(&s);
        // Synthetic SIF_TRACKPOS snapshots exercise the exact native thumbtrack
        // handler without moving the user's global mouse or sending global keys.
        for _ in 0..4 {
            for position in (0..600).step_by(13).chain((0..600).rev().step_by(13)) {
                scroll_from_info(
                    &s,
                    1,
                    SB_THUMBTRACK.0 as u32,
                    SCROLLINFO {
                        nTrackPos: position,
                        ..Default::default()
                    },
                );
            }
            unsafe {
                SendMessageW(
                    panel.hwnd(),
                    WM_MOUSEWHEEL,
                    WPARAM(((120i16 as u16 as u32) << 16) as usize),
                    LPARAM(0),
                );
                SendMessageW(
                    panel.hwnd(),
                    WM_VSCROLL,
                    WPARAM(SB_LINEDOWN.0 as usize),
                    LPARAM(0),
                );
                SendMessageW(
                    panel.hwnd(),
                    WM_VSCROLL,
                    WPARAM(SB_LINEUP.0 as usize),
                    LPARAM(0),
                );
            }
        }
        s.borrow_mut().scroll = [0, 0];
        layout(&s);
        unsafe {
            let _ = SetFocus(control(&s, HELP));
        }
        let incremental = directory.join("writing-incremental-client.bmp");
        let full = directory.join("writing-full-redraw-client.bmp");
        write_bitmap(panel.hwnd(), &incremental, true);
        unsafe {
            use windows::Win32::Graphics::Gdi::*;
            let _ = RedrawWindow(
                panel.hwnd(),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
        write_bitmap(panel.hwnd(), &full, true);
        // Ignore alpha and outer animation/caret areas; compare stable inner RGB.
        let a = std::fs::read(&incremental).unwrap();
        let b = std::fs::read(&full).unwrap();
        let width = i32::from_le_bytes(a[18..22].try_into().unwrap()) as usize;
        let height = i32::from_le_bytes(a[22..26].try_into().unwrap()).unsigned_abs() as usize;
        for y in 100..height.saturating_sub(30) {
            for x in 30..width.saturating_sub(30) {
                let offset = 54 + (y * width + x) * 4;
                assert_eq!(
                    &a[offset..offset + 3],
                    &b[offset..offset + 3],
                    "incremental/full redraw differs at {x},{y}"
                );
            }
        }
        // An unchanged render must not reset native selection or editor scroll.
        unsafe {
            use windows::Win32::UI::Controls::{EM_GETFIRSTVISIBLELINE, EM_GETSEL, EM_SETSEL};
            let input = control(&s, INPUT);
            SendMessageW(input, EM_SETSEL, WPARAM(2), LPARAM(5));
            let before = SendMessageW(input, EM_GETSEL, WPARAM(0), LPARAM(0));
            let line = SendMessageW(input, EM_GETFIRSTVISIBLELINE, WPARAM(0), LPARAM(0));
            for _ in 0..30 {
                render(&s);
            }
            assert_eq!(SendMessageW(input, EM_GETSEL, WPARAM(0), LPARAM(0)), before);
            assert_eq!(
                SendMessageW(input, EM_GETFIRSTVISIBLELINE, WPARAM(0), LPARAM(0)),
                line
            );
            let output = control(&s, OUTPUT);
            SendMessageW(output, EM_SETSEL, WPARAM(2), LPARAM(5));
            let selected = SendMessageW(output, EM_GETSEL, WPARAM(0), LPARAM(0));
            for _ in 0..10 {
                s.borrow_mut()
                    .page
                    .result
                    .as_mut()
                    .unwrap()
                    .text
                    .push_str("合成流式片段");
                render(&s);
                assert_eq!(
                    SendMessageW(output, EM_GETSEL, WPARAM(0), LPARAM(0)),
                    selected
                );
            }
        }
        // Fake worker snapshot: waiting is not completion and cannot lose backup.
        {
            let mut state = s.borrow_mut();
            state.backup = state.page.result.clone();
            state.pending = true;
            state.request = Some(901);
        }
        receive_update(
            &s,
            crate::writing::worker::Update {
                id: 901,
                text: String::new(),
                phase: Phase::Waiting {
                    attempt: 2,
                    seconds: 3,
                },
            },
        );
        assert!(s.borrow().backup.is_some());
        assert_eq!(s.borrow().request, Some(901));
        assert!(text(control(&s, STATUS)).contains("第2次"));
        assert!(unsafe { IsWindowEnabled(control(&s, GENERATE)) }.as_bool());
        assert!(!unsafe { IsWindowEnabled(control(&s, RETRY)) }.as_bool());
        assert!(!unsafe { IsWindowEnabled(control(&s, APPLY)) }.as_bool());
        stop(&s);
        assert_eq!(count.get(), 0);
        s.borrow_mut().page = completed_page();
        render(&s);
        // Full guide is locally accessible, read-only and scrollable. No nested loop.
        let help = create_setup_help(panel.hwnd()).unwrap();
        unsafe {
            let body = GetDlgItem(help, HELP_BODY as i32).unwrap();
            assert_eq!(
                text(body).replace("\r\n", "\n"),
                crate::writing::help::SETUP_GUIDE.replace("\r\n", "\n")
            );
            assert_ne!(GetWindowLongPtrW(body, GWL_STYLE) & ES_READONLY as isize, 0);
            write_client_bitmap(help, &directory.join("writing-help.bmp"));
            let help_rect = RECT {
                left: 0,
                top: 0,
                right: 560,
                bottom: 420,
            };
            SendMessageW(
                help,
                WM_DPICHANGED,
                WPARAM(144 | (144 << 16)),
                LPARAM((&help_rect as *const RECT) as isize),
            );
            SendMessageW(body, WM_VSCROLL, WPARAM(SB_BOTTOM.0 as usize), LPARAM(0));
            assert!(
                SendMessageW(
                    body,
                    windows::Win32::UI::Controls::EM_GETFIRSTVISIBLELINE,
                    WPARAM(0),
                    LPARAM(0)
                )
                .0 > 0
            );
            write_client_bitmap(help, &directory.join("writing-help-small-144dpi.bmp"));
            SendMessageW(help, WM_CLOSE, WPARAM(0), LPARAM(0));
            assert!(!IsWindow(help).as_bool());
        }
        s.borrow_mut().page = completed_page();
        render(&s);
        // Real native EDIT notification preserves 501 scalars and gives visible feedback.
        {
            let mut state = s.borrow_mut();
            state.page.bound = false;
            state.page.selection = false;
            state.page.binding_id = 0;
        }
        edit_control(control(&s, INPUT), &"字".repeat(501));
        assert_eq!(text(control(&s, INPUT)).chars().count(), 501);
        assert!(text(control(&s, STATUS)).contains("超过500"));
        assert!(!unsafe { IsWindowEnabled(control(&s, GENERATE)) }.as_bool());
        command(&s, RESTORE, BN_CLICKED);
        assert_eq!(text(control(&s, INPUT)), "原任务文本");
        assert!(!s.borrow().page.bound);
        assert!(!s.borrow().page.selection);
        assert_eq!(s.borrow().page.binding_id, 0);
        assert!(!unsafe { IsWindowEnabled(control(&s, APPLY)) }.as_bool());
        assert!(control(&s, 127).0.is_null());
        assert!(control(&s, 116).0.is_null());
        apply_result(&s);
        assert_eq!(count.get(), 0);
        // Independent bound-result fixture for overflow and callback consumption;
        // production restore never installs this test-only authority.
        {
            let mut state = s.borrow_mut();
            state.page = completed_page();
        }
        render(&s);
        edit_control(control(&s, OUTPUT), &"字".repeat(801));
        assert!(!can_apply(&s.borrow().page));
        assert!(!unsafe { IsWindowEnabled(control(&s, APPLY)) }.as_bool());
        apply_result(&s);
        assert_eq!(count.get(), 0);
        edit_control(control(&s, OUTPUT), "编辑后的完整结果");
        apply_result(&s);
        apply_result(&s);
        assert_eq!(count.get(), 1);
        assert!(s.borrow().page.result.as_ref().unwrap().consumed);
        // Synthetic 125% DPI change, capped to a small owned rectangle.
        let rect = RECT {
            left: 0,
            top: 0,
            right: 900,
            bottom: 600,
        };
        unsafe {
            SendMessageW(
                panel.hwnd(),
                WM_DPICHANGED,
                WPARAM(120 | (120 << 16)),
                LPARAM((&rect as *const RECT) as isize),
            );
        }
        assert_eq!(s.borrow().dpi, 120);
        reveal(&s, control(&s, HELP));
        write_client_bitmap(panel.hwnd(), &directory.join("writing-120dpi-top.bmp"));
        command(&s, CONNECTION, BN_CLICKED);
        assert!(!s.borrow().connection_open);
        assert!(control(&s, KEY).is_invalid());
        reveal(&s, control(&s, CONNECTION));
        let offset = s.borrow().scroll[1];
        assert!(offset > 0);
        for id in [KEY, SAVE, DELETE] {
            assert!(control(&s, id).is_invalid());
        }
        write_client_bitmap(
            panel.hwnd(),
            &directory.join("writing-120dpi-model-api.bmp"),
        );
        reveal(&s, control(&s, APPLY));
        assert_control_visible(&s, APPLY);
        // Exercise actual subclass navigation in both directions without injected global keys.
        unsafe {
            let input = control(&s, INPUT);
            let _ = SetFocus(input);
            keys(
                input,
                WM_KEYDOWN,
                WPARAM(VK_TAB.0 as usize),
                LPARAM(0),
                1,
                panel.hwnd().0 as usize,
            );
            assert_ne!(GetFocus(), input);
            let previous = GetNextDlgTabItem(panel.hwnd(), GetFocus(), true).unwrap();
            let _ = SetFocus(previous);
            assert_eq!(GetFocus(), input);
        }
        // Check shortcut mapping only: never invoke generation, even with empty text.
        {
            let mut state = s.borrow_mut();
            state.page.bound = false;
            state.page.selection = false;
            state.page.binding_id = 0;
        }
        edit_control(control(&s, INPUT), "");
        assert_eq!(
            key_command(VK_RETURN.0, true, false, false),
            Some(KEY_GENERATE)
        );
        assert!(s.borrow().job.is_none());
        assert!(!s.borrow().pending);
        // Narrow high-DPI layout reaches connection controls using vertical scrolling only.
        let narrow = RECT {
            left: 0,
            top: 0,
            right: 600,
            bottom: 540,
        };
        unsafe {
            SendMessageW(
                panel.hwnd(),
                WM_DPICHANGED,
                WPARAM(144 | (144 << 16)),
                LPARAM((&narrow as *const RECT) as isize),
            );
        }
        for id in [HELP, TONE, LENGTH, SCRIPT, CONNECTION] {
            reveal(&s, control(&s, id));
            assert_control_visible(&s, id);
        }
        assert_eq!(s.borrow().scroll[0], 0);
        reveal(&s, control(&s, HELP));
        write_client_bitmap(
            panel.hwnd(),
            &directory.join("writing-draft-small-144dpi.bmp"),
        );
        command(&s, CONNECTION, BN_CLICKED);
        assert!(!s.borrow().connection_open);
        assert!(!unsafe { IsWindowVisible(control(&s, KEY)) }.as_bool());
        // Exact 260/300 logical client widths at both high-DPI scales. Only the
        // tiny-width fallback scrolls horizontally; no global keys or real jobs.
        for dpi in [144, 192] {
            for width in [260, 300, 360] {
                let mut outer = RECT::default();
                let mut client = RECT::default();
                unsafe {
                    GetWindowRect(panel.hwnd(), &mut outer).unwrap();
                    GetClientRect(panel.hwnd(), &mut client).unwrap();
                    let rect = RECT {
                        left: 0,
                        top: 0,
                        right: width * dpi / 96 + outer.right - outer.left - client.right,
                        bottom: 560,
                    };
                    SendMessageW(
                        panel.hwnd(),
                        WM_DPICHANGED,
                        WPARAM((dpi | (dpi << 16)) as usize),
                        LPARAM((&rect as *const RECT) as isize),
                    );
                    GetClientRect(panel.hwnd(), &mut client).unwrap();
                    assert_eq!(client.right, width * dpi / 96);
                    let style = GetWindowLongPtrW(panel.hwnd(), GWL_STYLE);
                    assert_eq!(style & WS_HSCROLL.0 as isize != 0, width < 360);
                }
                reveal(&s, control(&s, CONNECTION));
                assert_control_visible(&s, CONNECTION);
                command(&s, CONNECTION, BN_CLICKED);
                for id in [HELP, CONNECTION, SAVE, DELETE, GENERATE, APPLY] {
                    reveal(&s, control(&s, id));
                    assert_control_visible(&s, id);
                    assert!(unsafe { IsWindowVisible(control(&s, id)) }.as_bool());
                }
                assert!(s.borrow().job.is_none());
                assert!(!s.borrow().pending);
                if width == 260 {
                    reveal(&s, control(&s, HELP));
                    write_client_bitmap(
                        panel.hwnd(),
                        &directory.join(format!("writing-tiny-{width}-{dpi}dpi-help.bmp")),
                    );
                    reveal(&s, control(&s, DELETE));
                    write_client_bitmap(
                        panel.hwnd(),
                        &directory.join(format!("writing-tiny-{width}-{dpi}dpi-key.bmp")),
                    );
                }
                command(&s, CONNECTION, BN_CLICKED);
            }
        }
        // Callback reenters dispatch and destroys this window; no borrow or dangling owner.
        {
            let mut state = s.borrow_mut();
            state.page = completed_page();
        }
        let hwnd = panel.hwnd();
        s.borrow_mut().apply = Some(Box::new(move |_| {
            unsafe {
                SendMessageW(hwnd, WM_COMMAND, WPARAM(APPLY), LPARAM(0));
                DestroyWindow(hwnd).unwrap();
            }
            Ok(())
        }));
        apply_result(&s);
        assert!(!panel.is_open());
        panel.close();
        // Esc closes an independent owned window; mapping never applies anything.
        let other = open_panel(
            Task::draft(),
            ChineseScript::Simplified,
            PanelSource::Manual,
            None,
        )
        .unwrap();
        other.state.borrow_mut().pending = true;
        unsafe {
            SendMessageW(other.hwnd(), KEY_ESCAPE, WPARAM(0), LPARAM(0));
        }
        assert!(other.is_open());
        assert!(!other.state.borrow().pending);
        assert!(other.state.borrow().job.is_none());
        unsafe {
            SendMessageW(other.hwnd(), KEY_ESCAPE, WPARAM(0), LPARAM(0));
        }
        assert!(!other.is_open());
    }
    fn assert_control_visible(s: &Shared, id: usize) {
        let state = s.borrow();
        let (_, r) = state
            .layout
            .iter()
            .find(|(h, _)| *h == control(s, id))
            .unwrap();
        let mut client = RECT::default();
        unsafe {
            GetClientRect(state.hwnd, &mut client).unwrap();
        }
        let x = r[0] * state.dpi / 96 - state.scroll[0];
        let y = r[1] * state.dpi / 96 - state.scroll[1];
        let height = if r[3] == 160 { 30 } else { r[3] };
        assert!(
            x >= 0
                && y >= 0
                && x + r[2] * state.dpi / 96 <= client.right
                && y + height * state.dpi / 96 <= client.bottom,
            "control {id}: position=({x},{y}), size=({},{height}), dpi={}, viewport=({},{})",
            r[2],
            state.dpi,
            client.right,
            client.bottom
        );
    }
    fn write_client_bitmap(hwnd: HWND, path: &std::path::Path) {
        write_bitmap(hwnd, path, false);
    }
    fn write_bitmap(hwnd: HWND, path: &std::path::Path, live: bool) {
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
            GetClientRect(hwnd, &mut rect).unwrap();
            let (width, height) = (rect.right, rect.bottom);
            let dc = CreateCompatibleDC(HDC::default());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: 40,
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
            let old = SelectObject(dc, bitmap);
            let rendered = if live {
                // Read only this owned client's pixels, with no scaling or desktop DC.
                let source = GetDC(hwnd);
                let ok = BitBlt(dc, 0, 0, width, height, source, 0, 0, SRCCOPY).is_ok();
                ReleaseDC(hwnd, source);
                i32::from(ok)
            } else {
                PrintWindow(hwnd.0, dc.0, 1) // PW_CLIENTONLY: never capture desktop.
            };
            let length = (width * height * 4) as usize;
            let mut bytes = Vec::with_capacity(54 + length);
            bytes.extend_from_slice(b"BM");
            bytes.extend_from_slice(&((54 + length) as u32).to_le_bytes());
            bytes.extend_from_slice(&[0; 4]);
            bytes.extend_from_slice(&54u32.to_le_bytes());
            bytes.extend_from_slice(std::slice::from_raw_parts(
                (&info.bmiHeader as *const BITMAPINFOHEADER).cast::<u8>(),
                40,
            ));
            bytes.extend_from_slice(std::slice::from_raw_parts(bits.cast::<u8>(), length));
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(dc);
            assert_ne!(rendered, 0);
            std::fs::write(path, bytes).unwrap();
        }
    }
}
