//! Thread-owned non-activating polish preview. No authority crosses the worker boundary.
use super::writing_ui::ApplyCallback;
use crate::writing::{
    worker::{Phase, WritingJob},
    Task, WritingRequest,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForSystem,
            Input::KeyboardAndMouse::EnableWindow,
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
};

pub type ValidateCallback = Box<dyn Fn() -> bool>;
const CLASS: PCWSTR = w!("Flower.PolishPopup.050");
const APPLY: usize = 101;
const CANCEL: usize = 102;
const TIMER: usize = 0x4650;
struct State {
    hwnd: Cell<HWND>,
    output: Cell<HWND>,
    status: Cell<HWND>,
    button: Cell<HWND>,
    font: Cell<HFONT>,
    background: HBRUSH,
    validate: Rc<dyn Fn() -> bool>,
    apply: RefCell<Option<ApplyCallback>>,
    job: RefCell<Option<WritingJob>>,
    id: u64,
    result: RefCell<String>,
    complete: Cell<bool>,
    invalid: Cell<bool>,
    used: Cell<bool>,
    checking: Cell<bool>,
}
/// Not Send/Sync: HWNDs, callbacks and the cancellation/join owner stay on the TSF thread.
pub struct PolishPopup {
    state: Rc<State>,
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn set_text(h: HWND, text: &str) {
    let t = wide(text);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
    }
}
fn qualified(s: &Rc<State>) -> bool {
    if s.invalid.get() || s.used.get() || s.hwnd.get().is_invalid() || s.checking.replace(true) {
        return false;
    }
    // No RefCell borrow spans the validator's COM calls, which may pump messages.
    let valid = (s.validate)();
    s.checking.set(false);
    if !valid {
        invalidate(s);
    }
    valid && !s.invalid.get() && !s.hwnd.get().is_invalid()
}
fn invalidate(s: &State) {
    s.invalid.set(true);
    s.complete.set(false);
    let job = s.job.borrow_mut().take();
    if let Some(job) = job {
        job.cancel();
        drop(job);
    }
    if !s.hwnd.get().is_invalid() {
        unsafe {
            let _ = EnableWindow(s.button.get(), false);
        }
        set_text(s.status.get(), "目标已失效，请关闭后重新选择原文。");
    }
}
fn poll(s: &Rc<State>) {
    if !qualified(s) {
        return;
    }
    let update = s.job.borrow().as_ref().and_then(WritingJob::poll);
    if let Some(update) = update {
        accept_update(s, update);
    }
}
fn accept_update(s: &Rc<State>, update: crate::writing::worker::Update) {
    if update.id != s.id || !qualified(s) {
        return;
    }
    s.complete.set(
        update.phase == Phase::Complete
            && !update.text.trim().is_empty()
            && update.text.chars().count() <= 800,
    );
    *s.result.borrow_mut() = update.text.clone();
    set_text(s.output.get(), &update.text);
    let status = match update.phase {
        Phase::Connecting => "正在连接…".to_owned(),
        Phase::Generating => "正在生成，原文保持不变…".to_owned(),
        Phase::Waiting { attempt, seconds } => {
            format!("等待重试（第 {attempt} 次，{seconds} 秒），原文保持不变…")
        }
        Phase::Complete => "已完成；点击“使用”替换原选区。".to_owned(),
        Phase::Failed(e) => e.to_string(),
    };
    set_text(s.status.get(), &status);
    unsafe {
        let _ = EnableWindow(s.button.get(), s.complete.get() && !s.used.get());
    }
}
fn use_result(s: &Rc<State>) {
    if !s.complete.get() || !qualified(s) || s.used.replace(true) {
        return;
    }
    s.complete.set(false);
    unsafe {
        let _ = EnableWindow(s.button.get(), false);
    }
    let output = s.result.borrow().clone();
    let callback = s.apply.borrow_mut().take();
    if let Some(mut callback) = callback {
        match callback(&output) {
            Ok(()) => close(s),
            Err(e) => {
                invalidate(s);
                set_text(s.status.get(), &e);
            }
        }
    }
}
fn close(s: &State) {
    s.invalid.set(true);
    s.complete.set(false);
    let job = s.job.borrow_mut().take();
    if let Some(job) = job {
        job.cancel();
        drop(job);
    }
    let hwnd = s.hwnd.replace(HWND::default());
    if !hwnd.is_invalid() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}
impl PolishPopup {
    pub fn hwnd(&self) -> HWND {
        self.state.hwnd.get()
    }
    pub fn is_open(&self) -> bool {
        !self.hwnd().is_invalid()
    }
    pub fn close(&mut self) {
        close(&self.state);
    }
    pub fn show(&self) {
        if self.is_open() {
            unsafe {
                let _ = ShowWindow(self.hwnd(), SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(
                    self.hwnd(),
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }
}
impl Drop for PolishPopup {
    fn drop(&mut self) {
        self.close();
    }
}
impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.font.get());
            let _ = DeleteObject(self.background);
        }
    }
}

unsafe extern "system" fn passive_control(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Native buttons/edit controls normally SetFocus on mouse-down. Keep the
        // host focus, but retain native scrollbars/mouse-wheel in the result.
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_CONTEXTMENU => {
            LRESULT(0)
        }
        WM_LBUTTONUP if data != 0 => {
            let _ = PostMessageW(
                GetParent(hwnd).unwrap_or_default(),
                WM_COMMAND,
                WPARAM(data),
                LPARAM(hwnd.0 as isize),
            );
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_KEYDOWN | WM_CHAR => LRESULT(0),
        _ => DefSubclassProc(hwnd, msg, wp, lp),
    }
}
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(lp.0 as *const CREATESTRUCTW);
        let state = &*(create.lpCreateParams as *const Rc<State>);
        let raw = Rc::into_raw(Rc::clone(state));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        state.hwnd.set(hwnd);
    }
    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
    if raw.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    Rc::increment_strong_count(raw);
    let s = Rc::from_raw(raw);
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => {
            if !s.checking.get() {
                poll(&s);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match wp.0 & 0xffff {
                APPLY => use_result(&s),
                CANCEL => close(&s),
                _ => (),
            };
            LRESULT(0)
        }
        WM_CLOSE => {
            close(&s);
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            FillRect(HDC(wp.0 as *mut _), &rect, s.background);
            LRESULT(1)
        }
        WM_DRAWITEM => {
            let item = &*(lp.0 as *const windows::Win32::UI::Controls::DRAWITEMSTRUCT);
            let disabled = item.itemState.0 & windows::Win32::UI::Controls::ODS_DISABLED.0 != 0;
            let primary = item.CtlID as usize == APPLY;
            let saved = SaveDC(item.hDC);
            let fill = if disabled {
                0xe9e5dc
            } else if primary {
                0x696b0f
            } else {
                0xffffff
            };
            SetDCBrushColor(item.hDC, windows::Win32::Foundation::COLORREF(fill));
            FillRect(item.hDC, &item.rcItem, HBRUSH(GetStockObject(DC_BRUSH).0));
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(
                item.hDC,
                windows::Win32::Foundation::COLORREF(if primary && !disabled {
                    0xffffff
                } else {
                    0x34302e
                }),
            );
            SelectObject(item.hDC, s.font.get());
            let mut label: Vec<u16> = if primary { "使用" } else { "取消" }
                .encode_utf16()
                .collect();
            let mut rect = item.rcItem;
            DrawTextW(
                item.hDC,
                &mut label,
                &mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let _ = RestoreDC(item.hDC, saved);
            LRESULT(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
            let dc = HDC(wp.0 as *mut _);
            SetBkColor(dc, windows::Win32::Foundation::COLORREF(0xffffff));
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x34302e));
            LRESULT(GetStockObject(WHITE_BRUSH).0 as isize)
        }
        WM_NCDESTROY => {
            s.hwnd.set(HWND::default());
            s.output.set(HWND::default());
            s.status.set(HWND::default());
            s.button.set(HWND::default());
            s.invalid.set(true);
            s.complete.set(false);
            let job = s.job.borrow_mut().take();
            drop(job);
            let _ = KillTimer(hwnd, TIMER);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Rc::from_raw(raw));
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
fn placement(anchor: RECT, work: RECT, width: i32, height: i32) -> (i32, i32, i32, i32) {
    let width = width.min((work.right - work.left).max(1));
    let height = height.min((work.bottom - work.top).max(1));
    let x = anchor.left.clamp(work.left, work.right - width);
    let y = if anchor.bottom + 8 + height <= work.bottom {
        anchor.bottom + 8
    } else {
        anchor.top - height - 8
    }
    .clamp(work.top, work.bottom - height);
    (x, y, width, height)
}
pub fn open_popup(
    request: WritingRequest,
    profile: crate::writing::config::ApiProfile,
    anchor: RECT,
    apply: ApplyCallback,
    validate: ValidateCallback,
    store: Option<crate::settings::SettingsStore>,
) -> Result<PolishPopup, String> {
    if !crate::writing::config::online_enabled(store.as_ref()) {
        return Err(crate::writing::WritingError::OnlineDisabled.to_string());
    }
    create_popup(request, anchor, apply, validate, |request| {
        if !crate::writing::config::online_enabled(store.as_ref()) {
            return Err(crate::writing::WritingError::OnlineDisabled.to_string());
        }
        if !store
            .as_ref()
            .is_some_and(|store| crate::writing::config::polish_authorized(store, &profile))
        {
            return Err("模型连接或润色授权已变化，本次未发送。".into());
        }
        WritingJob::start_with_profile(request, profile, false)
            .map(Some)
            .map_err(|e| e.to_string())
    })
}

fn create_popup(
    request: WritingRequest,
    anchor: RECT,
    apply: ApplyCallback,
    validate: ValidateCallback,
    start: impl FnOnce(WritingRequest) -> Result<Option<WritingJob>, String>,
) -> Result<PolishPopup, String> {
    if !matches!(request.task, Task::Polish { .. }) {
        return Err("候选浮层仅接受润色任务。".into());
    }
    request.validate().map_err(|e| e.to_string())?;
    if !validate() {
        return Err("原文目标已失效。".into());
    }
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None).map_err(|_| "无法创建浮层。")?.0);
        let background = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00f8f7f4));
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // A registered class outlives individual windows. Never retain the
            // per-window brush here after State::drop has deleted it.
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let state = Rc::new(State {
            hwnd: Cell::new(HWND::default()),
            output: Cell::new(HWND::default()),
            status: Cell::new(HWND::default()),
            button: Cell::new(HWND::default()),
            font: Cell::new(HFONT::default()),
            background,
            validate: Rc::from(validate),
            apply: RefCell::new(Some(apply)),
            job: RefCell::new(None),
            id: request.id,
            result: RefCell::new(String::new()),
            complete: Cell::new(false),
            invalid: Cell::new(false),
            used: Cell::new(false),
            checking: Cell::new(false),
        });
        let popup = PolishPopup {
            state: Rc::clone(&state),
        };
        let dpi = GetDpiForSystem() as i32;
        let scale = |n: i32| n * dpi / 96;
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(
            MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST),
            &mut info,
        )
        .as_bool()
        {
            return Err("无法确定原文所在工作区。".into());
        }
        let (x, y, width, height) = placement(anchor, info.rcWork, scale(480), scale(260));
        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            CLASS,
            w!("AI 润色"),
            WS_POPUP | WS_BORDER,
            x,
            y,
            width,
            height,
            None,
            None,
            instance,
            Some(&state as *const _ as *const _),
        )
        .map_err(|_| "无法创建润色浮层。")?;
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
        state.font.set(font);
        let add = |class: PCWSTR,
                   text: &str,
                   x: i32,
                   y: i32,
                   w: i32,
                   h: i32,
                   style: u32,
                   id: usize|
         -> Result<HWND, String> {
            let text = wide(text);
            let child = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                x,
                y,
                w,
                h,
                hwnd,
                HMENU(id as *mut _),
                instance,
                None,
            )
            .map_err(|_| "无法创建润色控件。")?;
            SendMessageW(child, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            if !SetWindowSubclass(child, Some(passive_control), 1, id).as_bool() {
                return Err("无法保护原文焦点。".into());
            }
            Ok(child)
        };
        let pad = scale(16);
        let inner = (width - 2 * pad).max(1);
        add(
            w!("STATIC"),
            "AI 润色 · 在线模型",
            pad,
            scale(12),
            inner,
            scale(24),
            0,
            0,
        )?;
        state.output.set(add(
            w!("EDIT"),
            "正在生成，原文不会改变…",
            pad,
            scale(42),
            inner,
            (height - scale(132)).max(1),
            WS_BORDER.0
                | WS_VSCROLL.0
                | ES_READONLY as u32
                | ES_MULTILINE as u32
                | ES_AUTOVSCROLL as u32,
            0,
        )?);
        state.status.set(add(
            w!("STATIC"),
            "正在连接…",
            pad,
            height - scale(82),
            inner,
            scale(36),
            0,
            0,
        )?);
        state.button.set(add(
            w!("BUTTON"),
            "使用",
            pad,
            height - scale(40),
            scale(100),
            scale(28),
            BS_OWNERDRAW as u32,
            APPLY,
        )?);
        add(
            w!("BUTTON"),
            "取消",
            pad + scale(112),
            height - scale(40),
            scale(100),
            scale(28),
            BS_OWNERDRAW as u32,
            CANCEL,
        )?;
        let _ = EnableWindow(state.button.get(), false);
        if SetTimer(hwnd, TIMER, 80, None) == 0 {
            return Err("无法启动润色预览。".into());
        }
        if !qualified(&state) {
            return Err("原文目标已失效。".into());
        }
        let job = start(request)?;
        *state.job.borrow_mut() = job;
        popup.show();
        Ok(popup)
    }
}

#[cfg(test)]
pub(crate) fn open_test_popup(
    request: WritingRequest,
    anchor: RECT,
    apply: ApplyCallback,
    validate: ValidateCallback,
) -> Result<PolishPopup, String> {
    create_popup(request, anchor, apply, validate, |_| Ok(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> WritingRequest {
        WritingRequest {
            id: 7,
            task: Task::polish(),
            script: crate::ChineseScript::Traditional,
            input: "隔離原文🙂".into(),
        }
    }
    fn update(phase: Phase) -> crate::writing::worker::Update {
        crate::writing::worker::Update {
            id: 7,
            phase,
            text: "候選結果🙂".into(),
        }
    }
    fn fixture(valid: Rc<Cell<bool>>, calls: Rc<Cell<usize>>, fail: bool) -> PolishPopup {
        create_popup(
            request(),
            RECT {
                left: 100,
                top: 100,
                right: 110,
                bottom: 120,
            },
            Box::new(move |_| {
                calls.set(calls.get() + 1);
                if fail {
                    Err("合成失败".into())
                } else {
                    Ok(())
                }
            }),
            Box::new(move || valid.get()),
            |r| {
                assert_eq!(r.input, "隔離原文🙂");
                assert_eq!(r.script, crate::ChineseScript::Traditional);
                // Deliberately no WritingJob/credentials/network in this fixture.
                Ok(None)
            },
        )
        .unwrap()
    }
    unsafe fn pump(hwnd: HWND) {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
            DispatchMessageW(&msg);
        }
    }

    #[test]
    #[ignore = "owned HWND and isolated Store; FLOWER_CONSENT_TEST_DIR required"]
    fn native_polish_store_gate_rechecks_before_worker() {
        use windows::Win32::Foundation::BOOL;
        let directory =
            std::path::PathBuf::from(std::env::var_os("FLOWER_CONSENT_TEST_DIR").unwrap());
        let store = crate::settings::SettingsStore::new(directory.join("polish-worker-gate.json"));
        let mut settings = crate::settings::Settings::default();
        let profile = settings.model_api.active().unwrap();
        let anchor = RECT {
            left: 100,
            top: 100,
            right: 101,
            bottom: 120,
        };
        store.save(&settings).unwrap();
        assert_eq!(
            open_popup(
                request(),
                profile.clone(),
                anchor,
                Box::new(|_| panic!("must not apply")),
                Box::new(|| panic!("disabled must not create HWND")),
                Some(store.clone())
            )
            .err()
            .unwrap(),
            crate::writing::WritingError::OnlineDisabled.to_string()
        );
        settings.online_ai_enabled = true;
        settings.online_polish.enabled = true;
        store.save(&settings).unwrap();
        fn owned_popups() -> Vec<HWND> {
            unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
                let mut class = [0u16; 64];
                let length = GetClassNameW(hwnd, &mut class);
                if String::from_utf16_lossy(&class[..length as usize]) == "Flower.PolishPopup.050" {
                    (*(data.0 as *mut Vec<HWND>)).push(hwnd);
                }
                BOOL(1)
            }
            let mut windows = Vec::new();
            unsafe {
                EnumThreadWindows(
                    windows::Win32::System::Threading::GetCurrentThreadId(),
                    Some(collect),
                    LPARAM(&mut windows as *mut Vec<HWND> as isize),
                )
                .unwrap();
            }
            windows
        }
        let baseline = owned_popups();
        let pre_window = Rc::new(Cell::new(false));
        let saw_pre_window = Rc::clone(&pre_window);
        let created = Rc::new(RefCell::new(None::<Rc<State>>));
        let observed = Rc::clone(&created);
        let created_hwnd = Rc::new(Cell::new(HWND::default()));
        let observed_hwnd = Rc::clone(&created_hwnd);
        let latest = store.clone();
        let result = open_popup(
            request(),
            profile,
            anchor,
            Box::new(|_| panic!("must not apply")),
            Box::new(move || {
                let new_windows: Vec<_> = owned_popups()
                    .into_iter()
                    .filter(|hwnd| !baseline.contains(hwnd))
                    .collect();
                if !saw_pre_window.get() {
                    assert!(
                        new_windows.is_empty(),
                        "first validation must precede HWND creation"
                    );
                    assert!(crate::writing::config::online_enabled(Some(&latest)));
                    saw_pre_window.set(true);
                    return true;
                }
                assert_eq!(
                    new_windows.len(),
                    1,
                    "must observe this call's newly created owned HWND"
                );
                let hwnd = new_windows[0];
                observed_hwnd.set(hwnd);
                unsafe {
                    assert!(IsWindow(hwnd).as_bool());
                    let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
                    assert!(!raw.is_null());
                    assert_eq!((*raw).hwnd.get(), hwnd);
                    assert!((*raw).job.borrow().is_none(), "no worker before revocation");
                    Rc::increment_strong_count(raw);
                    *observed.borrow_mut() = Some(Rc::from_raw(raw));
                }
                let mut settings = latest.load().unwrap();
                assert!(
                    settings.online_ai_enabled,
                    "pre-HWND validation must not revoke"
                );
                settings.online_ai_enabled = false;
                latest.save(&settings).unwrap();
                true
            }),
            Some(store),
        );
        assert!(pre_window.get());
        let state = created.borrow();
        let state = state
            .as_ref()
            .expect("must reach validation with a real new owned HWND");
        assert_eq!(
            result.err().unwrap(),
            crate::writing::WritingError::OnlineDisabled.to_string()
        );
        assert!(
            state.job.borrow().is_none(),
            "revocation must prevent worker creation"
        );
        assert!(
            !unsafe { IsWindow(created_hwnd.get()) }.as_bool(),
            "failed opening must destroy the owned HWND"
        );
    }

    #[test]
    #[ignore = "creates only owned HWNDs; no network, foreground changes or global input"]
    fn native_polish_owned_popup_noactivate_incomplete_click_once_and_cleanup() {
        unsafe {
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled};
            let original_focus = GetFocus();
            let original_foreground = GetForegroundWindow();
            let calls = Rc::new(Cell::new(0));
            let popup = fixture(Rc::new(Cell::new(true)), Rc::clone(&calls), false);
            let hwnd = popup.hwnd();
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            assert_eq!(
                style & (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW).0,
                (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW).0
            );
            assert_eq!(
                SendMessageW(hwnd, WM_MOUSEACTIVATE, WPARAM(0), LPARAM(0)).0,
                MA_NOACTIVATE as isize
            );
            assert_eq!(GetFocus(), original_focus);
            assert_eq!(GetForegroundWindow(), original_foreground);
            accept_update(&popup.state, update(Phase::Generating));
            assert!(!IsWindowEnabled(popup.state.button.get()).as_bool());
            SendMessageW(hwnd, WM_COMMAND, WPARAM(APPLY), LPARAM(0));
            for key in [0x0d, 0x20, 0x31] {
                SendMessageW(hwnd, WM_KEYDOWN, WPARAM(key), LPARAM(0));
                SendMessageW(hwnd, WM_CHAR, WPARAM(key), LPARAM(0));
            }
            assert_eq!(calls.get(), 0);
            accept_update(
                &popup.state,
                update(Phase::Waiting {
                    attempt: 1,
                    seconds: 2,
                }),
            );
            assert!(!IsWindowEnabled(popup.state.button.get()).as_bool());
            SendMessageW(hwnd, WM_COMMAND, WPARAM(APPLY), LPARAM(0));
            assert_eq!(calls.get(), 0, "waiting is not a usable result");
            accept_update(&popup.state, update(Phase::Complete));
            assert!(IsWindowEnabled(popup.state.button.get()).as_bool());
            let button = popup.state.button.get();
            SendMessageW(button, WM_LBUTTONDOWN, WPARAM(0), LPARAM(0));
            SendMessageW(button, WM_LBUTTONUP, WPARAM(0), LPARAM(0));
            SendMessageW(button, WM_LBUTTONUP, WPARAM(0), LPARAM(0));
            pump(hwnd);
            assert_eq!(calls.get(), 1);
            assert!(!popup.is_open());
            assert!(!IsWindow(hwnd).as_bool());
            assert_eq!(GetFocus(), original_focus);
            assert_eq!(GetForegroundWindow(), original_foreground);
        }
    }

    #[test]
    #[ignore = "owned HWND state fixture with synthetic updates only"]
    fn native_polish_owned_invalidation_late_result_and_failed_apply_never_retry() {
        let valid = Rc::new(Cell::new(true));
        let calls = Rc::new(Cell::new(0));
        let mut popup = fixture(Rc::clone(&valid), Rc::clone(&calls), false);
        accept_update(&popup.state, update(Phase::Complete));
        valid.set(false);
        poll(&popup.state);
        valid.set(true); // Changing back must not restore the old authority.
        accept_update(&popup.state, update(Phase::Complete));
        use_result(&popup.state);
        assert!(popup.state.invalid.get());
        assert!(!popup.state.complete.get());
        assert_eq!(calls.get(), 0);
        popup.close();
        accept_update(&popup.state, update(Phase::Complete));
        assert!(!popup.state.complete.get());

        let popup = fixture(Rc::new(Cell::new(true)), Rc::clone(&calls), true);
        accept_update(&popup.state, update(Phase::Complete));
        use_result(&popup.state);
        use_result(&popup.state);
        assert_eq!(calls.get(), 1);
        assert!(popup.state.used.get());
        assert!(popup.state.invalid.get());
    }

    #[test]
    fn invalid_source_and_wrong_task_do_not_start_worker() {
        let starts = Cell::new(0);
        let result = create_popup(
            request(),
            RECT::default(),
            Box::new(|_| Ok(())),
            Box::new(|| false),
            |_| {
                starts.set(starts.get() + 1);
                Ok(None)
            },
        );
        assert!(result.is_err());
        let mut r = request();
        r.task = Task::draft();
        let result = create_popup(
            r,
            RECT::default(),
            Box::new(|_| Ok(())),
            Box::new(|| true),
            |_| {
                starts.set(starts.get() + 1);
                Ok(None)
            },
        );
        assert!(result.is_err());
        assert_eq!(starts.get(), 0);
    }
    #[test]
    fn placement_clamps_small_and_edge_workspaces() {
        let work = RECT {
            left: 10,
            top: 20,
            right: 310,
            bottom: 200,
        };
        assert_eq!(
            placement(
                RECT {
                    left: 305,
                    top: 190,
                    right: 306,
                    bottom: 195
                },
                work,
                480,
                260
            ),
            (10, 20, 300, 180)
        );
        let work = RECT {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 800,
        };
        assert_eq!(
            placement(
                RECT {
                    left: 990,
                    top: 780,
                    right: 995,
                    bottom: 790
                },
                work,
                480,
                260
            ),
            (520, 512, 480, 260)
        );
    }
}
