//! 非激活式 Win32 候选弹窗外壳。
//!
//! Windows 实现使用与平台无关的 `CandidatePopupLayout` 同时绘制和命中候选。
//! 鼠标动作先排队，再由自定义窗口消息延迟派发；窗口本身不持有
//! `TextService`，也不运行自己的消息循环。

#[cfg(any(target_os = "windows", test))]
use crate::settings::Theme;

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PopupPalette {
    background: u32,
    text: u32,
    highlight: u32,
    highlight_text: u32,
}

#[cfg(any(target_os = "windows", test))]
const LIGHT_PALETTE: PopupPalette = PopupPalette {
    background: 0x00ff_ffff,
    text: 0x0018_1818,
    highlight: 0x00d7_7800,
    highlight_text: 0x00ff_ffff,
};

#[cfg(any(target_os = "windows", test))]
const DARK_PALETTE: PopupPalette = PopupPalette {
    background: 0x0020_2020,
    text: 0x00f2_f2f2,
    highlight: 0x0000_78d7,
    highlight_text: 0x00ff_ffff,
};

#[cfg(any(target_os = "windows", test))]
pub(crate) fn popup_palette(theme: Theme, system: PopupPalette) -> PopupPalette {
    match theme {
        Theme::System => system,
        Theme::Light => LIGHT_PALETTE,
        Theme::Dark => DARK_PALETTE,
    }
}

/// Convert a validated target-monitor measurement, never an old popup DPI.
#[cfg(any(target_os = "windows", test))]
fn target_candidate_width_budget(work_width: i32, target_dpi: u32) -> u32 {
    if work_width <= 0 || target_dpi == 0 {
        return 512;
    }
    let logical = (work_width as u64 * 96 / u64::from(target_dpi)).min(560);
    logical.saturating_sub(48).clamp(1, 512) as u32
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::rc::Rc;
    use std::sync::{Mutex, MutexGuard};

    use thiserror::Error;
    use windows::core::{Error as WindowsError, HRESULT, PCWSTR};
    use windows::Win32::Foundation::{
        COLORREF, ERROR_CLASS_ALREADY_EXISTS, ERROR_CLASS_DOES_NOT_EXIST, E_UNEXPECTED, HINSTANCE,
        HMODULE, HWND, LPARAM, LRESULT, RECT, SIZE, TRUE, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
        GetDC, GetMonitorInfoW, GetSysColor, GetTextExtentPoint32W, IntersectClipRect,
        InvalidateRect, MonitorFromRect, MonitorFromWindow, ReleaseDC, RestoreDC, SaveDC,
        SelectObject, SetBkMode, SetDIBitsToDevice, SetTextColor, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
        COLOR_WINDOW, COLOR_WINDOWTEXT, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_LEFT,
        DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_NORMAL, HBRUSH, HDC, HFONT,
        HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
        TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::{
        GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    use windows::Win32::UI::HiDpi::{
        AreDpiAwarenessContextsEqual, GetDpiForWindow, GetWindowDpiAwarenessContext,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
        IsWindowVisible, LoadCursorW, PostMessageW, RegisterClassExW, SetWindowLongPtrW,
        SetWindowPos, ShowWindow, UnregisterClassW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
        GWLP_USERDATA, HCURSOR, HMENU, IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SYSCOLORCHANGE,
        WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    use crate::ai::AiStatus;
    use crate::platform::candidate::{
        clamp_candidate_popup, position_candidate_popup, CandidatePopupAction,
        CandidatePopupLayout, PopupPoint, PopupRect, PopupSize,
    };
    use crate::platform::CandidatePopupSnapshot;
    use crate::settings::Theme;

    use super::{popup_palette, target_candidate_width_budget, PopupPalette};

    const POPUP_CLASS_NAME: PCWSTR = windows::core::w!("Flower.CandidatePopupWindow");
    const POPUP_WINDOW_NAME: PCWSTR = windows::core::w!("Flower candidates");
    const FONT_FACE: PCWSTR = windows::core::w!("Microsoft YaHei UI");

    const BASE_DPI: u32 = 96;
    const WM_FLOWER_DISPATCH_ACTION: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
    const WM_FLOWER_SETTINGS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;
    const AI_IDLE_TIMER: usize = 0x4641;

    #[cfg(test)]
    std::thread_local! {
        // Only the HWND explicitly enrolled by this thread's test is observed.
        static MESSAGE_COUNTS: RefCell<(HWND, usize, usize, usize)> =
            RefCell::new((HWND::default(), 0, 0, 0));
    }

    /// Keeps status geometry out of the selectable candidate model.
    struct PopupLayout {
        candidates: CandidatePopupLayout,
        size: PopupSize,
        status: Option<PopupRect>,
        lines: Vec<Vec<String>>,
        full_rows: Vec<PopupRect>,
        viewport: PopupRect,
        line_height: i32,
        scroll: i32,
        max_scroll: i32,
    }

    impl PopupLayout {
        fn new(
            snapshot: &CandidatePopupSnapshot,
            dpi: u32,
            bounds: PopupSize,
            status: Option<AiStatus>,
        ) -> Self {
            let height = bounds.height.max(1);
            let total_width = bounds.width.max(1).min(scaled(560, dpi));
            let settings_width = scaled(32, dpi).min((total_width - 1).max(0));
            let content_width = (total_width - settings_width).max(1);
            let status_height = if status.is_some() {
                scaled(22, dpi).max(1).min(height)
            } else {
                0
            };
            let mut candidates = CandidatePopupLayout::new(
                snapshot,
                dpi,
                PopupSize::new(content_width, (height - status_height).max(1)),
            );
            let line_height = scaled(30, dpi).max(1);
            let padding = scaled(8, dpi);
            let mut lines = Vec::new();
            // This DC is owned only for measurement, never used to capture pixels.
            let dc = MeasureDc::new();
            let font = dc.as_ref().and_then(|dc| PaintFont::new(dc.0, dpi));
            let measure = |text: &str| {
                font.as_ref()
                    .and_then(|_| measured_width(dc.as_ref()?.0, text))
            };
            let labels: Vec<String> = snapshot
                .candidates
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}. {}", i + 1, c.display_text()))
                .collect();
            let vertical = labels.len() != candidates.candidates.len()
                || labels
                    .iter()
                    .zip(&candidates.candidates)
                    .any(|(text, row)| {
                        measure(text).map_or(true, |w| w > row.width() - scaled(6, dpi))
                    });
            if vertical && font.is_some() {
                let width = content_width;
                let available = (width - padding * 2 - scaled(6, dpi)).max(0);
                candidates.candidates.clear();
                let mut top = padding;
                for label in &labels {
                    let wrapped = wrap_candidate_text(label, available, &measure);
                    let bottom =
                        top.saturating_add(line_height.saturating_mul(wrapped.len() as i32));
                    candidates.candidates.push(PopupRect::new(
                        padding,
                        top,
                        (width - padding).max(padding),
                        bottom,
                    ));
                    lines.push(wrapped);
                    top = bottom;
                }
                candidates.size = PopupSize::new(
                    width,
                    top.saturating_add(padding)
                        .min((height - status_height).max(0)),
                );
            } else if font.is_some() {
                lines = labels.into_iter().map(|s| vec![s]).collect();
            } else {
                // Font/measurement failure must not leave selectable unseen text.
                candidates.candidates.clear();
            }
            if candidates.size.height < padding * 2 + line_height {
                candidates.candidates.clear();
                lines.clear();
            }
            // CandidatePopupLayout requires a positive bound; at a one-pixel
            // work area the status owns that pixel and no candidate can fit.
            if height <= status_height {
                candidates.candidates.clear();
                candidates.size.height = 0;
            }
            let size = PopupSize::new(
                candidates.size.width + settings_width,
                candidates
                    .size
                    .height
                    .saturating_add(status_height)
                    .min(height),
            );
            let status = status.map(|_| {
                PopupRect::new(
                    0,
                    candidates.size.height,
                    candidates.size.width,
                    size.height,
                )
            });
            if settings_width >= scaled(32, dpi) && size.height >= scaled(28, dpi) {
                candidates.settings_button = Some(PopupRect::new(
                    candidates.size.width + scaled(4, dpi),
                    0,
                    size.width,
                    scaled(28, dpi),
                ));
            }
            let full_rows = candidates.candidates.clone();
            let viewport = PopupRect::new(
                0,
                padding.min(candidates.size.height),
                candidates.size.width,
                (candidates.size.height - padding).max(padding.min(candidates.size.height)),
            );
            let max_scroll = full_rows
                .last()
                .map_or(0, |r| (r.bottom - viewport.bottom).max(0));
            let mut result = Self {
                candidates,
                size,
                status,
                lines,
                full_rows,
                viewport,
                line_height,
                scroll: 0,
                max_scroll,
            };
            let offset = snapshot
                .highlighted
                .and_then(|i| result.full_rows.get(i))
                .map_or(0, |r| {
                    if r.height() > viewport.height() {
                        r.top - viewport.top
                    } else {
                        (r.bottom - viewport.bottom).max(0)
                    }
                });
            result.set_scroll(offset);
            result
        }

        fn set_scroll(&mut self, offset: i32) {
            self.scroll = offset.clamp(0, self.max_scroll);
            self.candidates.candidates = self
                .full_rows
                .iter()
                .map(|r| {
                    PopupRect::new(
                        r.left,
                        (r.top - self.scroll).max(self.viewport.top),
                        r.right,
                        (r.bottom - self.scroll).min(self.viewport.bottom),
                    )
                })
                .collect();
        }
    }

    struct MeasureDc(HDC);
    impl MeasureDc {
        fn new() -> Option<Self> {
            // SAFETY: Screen DC is acquired without reading its contents.
            let dc = unsafe { GetDC(HWND::default()) };
            (!dc.is_invalid()).then_some(Self(dc))
        }
    }
    impl Drop for MeasureDc {
        fn drop(&mut self) {
            // SAFETY: Release exactly the DC acquired by GetDC.
            unsafe {
                ReleaseDC(HWND::default(), self.0);
            }
        }
    }

    fn wrap_candidate_text(
        text: &str,
        available: i32,
        measure: &impl Fn(&str) -> Option<i32>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for ch in text.chars() {
            if measure(&ch.to_string()).map_or(true, |w| w > available || w < 0) {
                return Vec::new();
            }
            let mut next = line.clone();
            next.push(ch);
            if !line.is_empty() && measure(&next).map_or(true, |w| w > available) {
                lines.push(std::mem::take(&mut line));
            }
            line.push(ch);
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// Owns only the temporary built-in hidden window, never the real popup.
    struct HiddenDpiProbe(Option<HWND>);

    impl HiddenDpiProbe {
        fn destroy(&mut self) -> bool {
            match self.0.take() {
                // SAFETY: This guard exclusively owns the window on its creating thread.
                Some(hwnd) => unsafe { DestroyWindow(hwnd) }.is_ok(),
                None => true,
            }
        }
    }

    impl Drop for HiddenDpiProbe {
        fn drop(&mut self) {
            let _ = self.destroy();
        }
    }

    /// A failure while creating, updating, or destroying the candidate popup.
    #[derive(Debug, Error)]
    pub enum PopupError {
        /// No snapshot has been supplied for display.
        #[error("the candidate popup has no snapshot to display")]
        NoSnapshot,

        /// The window has already been destroyed.
        #[error("the candidate popup window has been destroyed")]
        Destroyed,

        /// A Win32 operation failed.
        #[error("candidate popup {operation} failed: {source}")]
        Win32 {
            /// The operation that failed.
            operation: &'static str,
            /// The Win32 or HRESULT error.
            #[source]
            source: WindowsError,
        },
    }

    impl PopupError {
        fn win32(operation: &'static str, source: WindowsError) -> Self {
            Self::Win32 { operation, source }
        }
    }

    fn state_busy_error() -> PopupError {
        PopupError::win32("state borrow", WindowsError::from_hresult(E_UNEXPECTED))
    }

    pub(crate) type PopupActionSink = Rc<dyn Fn(CandidatePopupAction) + 'static>;

    struct PresentedCandidates {
        snapshot: CandidatePopupSnapshot,
        dpi: u32,
        status: Option<AiStatus>,
        offset: Option<i32>,
        client: PopupRect,
        window: PopupRect,
        candidates: CandidatePopupLayout,
    }

    struct PopupState {
        presented: Option<PresentedCandidates>,
        scroll_offset: Option<i32>,
        snapshot: Option<CandidatePopupSnapshot>,
        ai_status: Option<AiStatus>,
        last_caret: Option<PopupRect>,
        dpi: u32,
        theme: Theme,
        pending_actions: VecDeque<CandidatePopupAction>,
        action_sink: Option<PopupActionSink>,
        settings_sink: Option<Rc<dyn Fn()>>,
        pending_settings: Option<(PopupPoint, CandidatePopupSnapshot)>,
        idle_sink: Option<Rc<dyn Fn()>>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum DetachOutcome {
        NotOwned,
        AlreadyDetached,
        Detached,
        Failed,
    }

    struct ClassRegistration {
        references: usize,
    }

    static CLASS_REGISTRATION: Mutex<ClassRegistration> =
        Mutex::new(ClassRegistration { references: 0 });

    struct ClassLease {
        instance: HINSTANCE,
        released: bool,
    }

    impl ClassLease {
        fn acquire(instance: HINSTANCE, cursor: HCURSOR) -> Result<Self, PopupError> {
            let mut registration = lock_class_registration();
            if registration.references == 0 {
                let window_class = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(popup_window_proc),
                    hInstance: instance,
                    hCursor: cursor,
                    lpszClassName: POPUP_CLASS_NAME,
                    ..Default::default()
                };

                // SAFETY: `window_class` is fully initialized with the DLL
                // instance that owns `popup_window_proc`, and it remains alive
                // for the registration call. This is a class-only operation;
                // no window creation or destruction happens under the lock.
                let atom = unsafe { RegisterClassExW(&window_class) };
                if atom == 0 {
                    let error = WindowsError::from_win32();
                    let already_exists =
                        error.code() == HRESULT::from_win32(ERROR_CLASS_ALREADY_EXISTS.0);
                    if !already_exists {
                        return Err(PopupError::win32("class registration", error));
                    }
                }
            }

            registration.references = registration.references.saturating_add(1);
            Ok(Self {
                instance,
                released: false,
            })
        }

        fn release(&mut self) -> Result<(), WindowsError> {
            if self.released {
                return Ok(());
            }
            self.released = true;

            let mut registration = lock_class_registration();
            registration.references = registration.references.saturating_sub(1);
            if registration.references != 0 {
                return Ok(());
            }

            // SAFETY: The class name and module instance identify the class
            // registered by `acquire`. This call does not create or destroy a
            // window, so it cannot re-enter popup window creation/destruction.
            match unsafe { UnregisterClassW(POPUP_CLASS_NAME, self.instance) } {
                Ok(()) => Ok(()),
                Err(error) => {
                    let code = error.code();
                    if code == HRESULT::from_win32(ERROR_CLASS_DOES_NOT_EXIST.0) {
                        Ok(())
                    } else {
                        // The class (and its function pointer into this DLL)
                        // cannot be confirmed unloaded. Pin this module until
                        // process exit rather than letting DllCanUnloadNow
                        // unload code that may still be referenced.
                        pin_popup_module()?;
                        Err(error)
                    }
                }
            }
        }
    }

    impl Drop for ClassLease {
        fn drop(&mut self) {
            let _ = self.release();
        }
    }

    fn lock_class_registration() -> MutexGuard<'static, ClassRegistration> {
        match CLASS_REGISTRATION.lock() {
            Ok(registration) => registration,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn module_handle(flags: u32) -> Result<HMODULE, WindowsError> {
        let window_proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
            popup_window_proc;
        let function_address = (window_proc as *const ()).cast::<u16>();
        let mut module = HMODULE::default();

        // SAFETY: `function_address` points into this DLL, and the caller
        // chooses whether the returned module handle is reference-counted,
        // unchanged, or permanently pinned.
        unsafe { GetModuleHandleExW(flags, PCWSTR::from_raw(function_address), &mut module) }?;

        Ok(module)
    }

    fn popup_module_instance() -> Result<HINSTANCE, PopupError> {
        module_handle(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        )
        .map(HINSTANCE::from)
        .map_err(|error| PopupError::win32("module lookup", error))
    }

    fn pin_popup_module() -> Result<(), WindowsError> {
        module_handle(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN)
            .map(|_| ())
    }

    /// A hidden, non-activating Win32 popup that renders candidate snapshots.
    ///
    /// The window belongs to the thread that creates it. Callers must keep
    /// dispatching messages on that thread for the window to repaint and react
    /// to DPI changes.
    pub struct CandidatePopupWindow {
        hwnd: HWND,
        state: Option<Box<RefCell<PopupState>>>,
        class_lease: Option<ClassLease>,
    }

    impl CandidatePopupWindow {
        /// Registers the popup class and creates a hidden popup window.
        pub fn new() -> Result<Self, PopupError> {
            let instance = popup_module_instance()?;

            // SAFETY: A null instance requests the shared system arrow cursor,
            // and IDC_ARROW is a valid system cursor identifier.
            let cursor = unsafe { LoadCursorW(HINSTANCE::default(), IDC_ARROW) }
                .map_err(|error| PopupError::win32("cursor load", error))?;

            let class_lease = ClassLease::acquire(instance, cursor)?;

            let state = Box::new(RefCell::new(PopupState {
                presented: None,
                scroll_offset: None,
                snapshot: None,
                ai_status: None,
                last_caret: None,
                dpi: BASE_DPI,
                theme: Theme::System,
                pending_actions: VecDeque::new(),
                action_sink: None,
                settings_sink: None,
                pending_settings: None,
                idle_sink: None,
            }));
            let state_pointer: *const c_void =
                (state.as_ref() as *const RefCell<PopupState>).cast::<c_void>();

            // SAFETY: The class is registered, the optional create parameter is
            // a live pointer to `state`, and the window remains hidden until
            // `show_at` is called. `state` is retained by this struct for the
            // window's entire lifetime.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    POPUP_CLASS_NAME,
                    POPUP_WINDOW_NAME,
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    HWND::default(),
                    HMENU::default(),
                    instance,
                    Some(state_pointer),
                )
            }
            .map_err(|error| PopupError::win32("window creation", error))?;

            Ok(Self {
                hwnd,
                state: Some(state),
                class_lease: Some(class_lease),
            })
        }

        /// Selects the candidate colors used by subsequent repaints.
        ///
        /// `Theme::System` samples current system window, text, and highlight
        /// colors at paint time. It follows system color settings, including
        /// high-contrast values, rather than inferring an app dark mode from
        /// registry settings.
        pub fn set_theme(&mut self, theme: Theme) -> Result<(), PopupError> {
            self.ensure_alive()?;
            {
                let mut state = self
                    .state
                    .as_deref()
                    .ok_or(PopupError::Destroyed)?
                    .try_borrow_mut()
                    .map_err(|_| state_busy_error())?;
                state.theme = theme;
            }

            // SAFETY: The window is live and the null rectangle invalidates the
            // complete client area for the new colors.
            if !unsafe { InvalidateRect(self.hwnd, None, TRUE) }.as_bool() {
                return Err(PopupError::win32(
                    "theme redraw invalidation",
                    WindowsError::from_win32(),
                ));
            }
            Ok(())
        }

        /// Changes only the fixed, non-selectable diagnostic row.
        pub(crate) fn set_ai_status(&mut self, status: AiStatus) -> Result<(), PopupError> {
            self.ensure_alive()?;
            let (added, caret) = {
                let mut state = self
                    .state
                    .as_deref()
                    .ok_or(PopupError::Destroyed)?
                    .try_borrow_mut()
                    .map_err(|_| state_busy_error())?;
                if state.ai_status == Some(status) {
                    return Ok(());
                }
                let added = state.ai_status.is_none();
                state.ai_status = Some(status);
                state.presented = None;
                (added, state.last_caret)
            };
            // Only the first status changes geometry. Never show a hidden popup.
            if added && self.is_visible() {
                if let Some(caret) = caret {
                    self.show_at(caret)?;
                }
            }
            // SAFETY: The live window is invalidated without activating it.
            if !unsafe { InvalidateRect(self.hwnd, None, TRUE) }.as_bool() {
                return Err(PopupError::win32(
                    "AI status invalidation",
                    WindowsError::from_win32(),
                ));
            }
            Ok(())
        }

        /// 设置入口独立于候选选择，回调不得强持有窗口。
        pub(crate) fn set_settings_action_sink(
            &mut self,
            sink: Option<Rc<dyn Fn()>>,
        ) -> Result<(), PopupError> {
            self.ensure_alive()?;
            let mut state = self
                .state
                .as_deref()
                .ok_or(PopupError::Destroyed)?
                .try_borrow_mut()
                .map_err(|_| state_busy_error())?;
            state.settings_sink = sink;
            state.pending_settings = None;
            Ok(())
        }

        /// 注册或解除鼠标动作回调。
        ///
        /// 回调只接收纯数据动作。窗口销毁、隐藏和失焦都会清除尚未派发的动作；
        /// 回调对象不得反向强持有窗口或其线程局部存储。
        pub(crate) fn set_action_sink(
            &mut self,
            sink: Option<PopupActionSink>,
        ) -> Result<(), PopupError> {
            self.ensure_alive()?;
            let mut state = self
                .state
                .as_deref()
                .ok_or(PopupError::Destroyed)?
                .try_borrow_mut()
                .map_err(|_| state_busy_error())?;
            state.action_sink = sink;
            if state.action_sink.is_none() {
                state.pending_actions.clear();
                state.pending_settings = None;
            }
            Ok(())
        }

        /// 取消已经排队但尚未派发的鼠标动作。
        pub(crate) fn cancel_pending_actions(&mut self) {
            let Some(state) = self.state.as_deref() else {
                return;
            };
            if let Ok(mut state) = state.try_borrow_mut() {
                state.pending_actions.clear();
                state.pending_settings = None;
            }
        }

        /// Owner-thread polling hook. The callback may request asynchronous TSF
        /// reads, but must never edit a host document from the window procedure.
        pub(crate) fn set_idle_sink(
            &mut self,
            sink: Option<Rc<dyn Fn()>>,
        ) -> Result<(), PopupError> {
            self.ensure_alive()?;
            let enabled = sink.is_some();
            let state = self.state.as_deref().ok_or(PopupError::Destroyed)?;
            state
                .try_borrow_mut()
                .map_err(|_| state_busy_error())?
                .idle_sink = sink;
            unsafe {
                if enabled {
                    if windows::Win32::UI::WindowsAndMessaging::SetTimer(
                        self.hwnd,
                        AI_IDLE_TIMER,
                        50,
                        None,
                    ) == 0
                    {
                        state
                            .try_borrow_mut()
                            .map_err(|_| state_busy_error())?
                            .idle_sink = None;
                        return Err(PopupError::win32("AI timer", WindowsError::from_win32()));
                    }
                } else {
                    let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                        self.hwnd,
                        AI_IDLE_TIMER,
                    );
                }
            }
            Ok(())
        }

        /// 替换绘制快照，并按当前 DPI 设置自然尺寸。
        pub fn update(&mut self, snapshot: &CandidatePopupSnapshot) -> Result<(), PopupError> {
            self.update_snapshot(snapshot)?;
            self.ensure_alive()?;

            // SAFETY: `self.hwnd` is a live popup window owned by this struct,
            // and the call has no pointer arguments.
            let dpi = unsafe { GetDpiForWindow(self.hwnd) };
            let dpi = if dpi == 0 { BASE_DPI } else { dpi };
            let status = self
                .state
                .as_deref()
                .ok_or(PopupError::Destroyed)?
                .try_borrow()
                .map_err(|_| state_busy_error())?
                .ai_status;
            // SAFETY: The window is live. Resizing does not activate it, and a
            // null insert-after handle is ignored because SWP_NOZORDER is set.
            if !self.is_visible() {
                let natural =
                    PopupLayout::new(snapshot, dpi, PopupSize::new(i32::MAX, i32::MAX), status)
                        .size;
                // Hidden creation can use the natural size. A visible refresh is
                // resized only by show_at's work-area-bounded placement below.
                unsafe {
                    SetWindowPos(
                        self.hwnd,
                        HWND::default(),
                        0,
                        0,
                        natural.width,
                        natural.height,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                }
                .map_err(|error| PopupError::win32("resize", error))?;
            }

            {
                let mut state = self
                    .state
                    .as_deref()
                    .ok_or(PopupError::Destroyed)?
                    .try_borrow_mut()
                    .map_err(|_| state_busy_error())?;
                state.dpi = dpi;
            }

            // A visible refresh must retain its caret anchor and screen clamp.
            let caret = self
                .state
                .as_deref()
                .ok_or(PopupError::Destroyed)?
                .try_borrow()
                .map_err(|_| state_busy_error())?
                .last_caret;
            if self.is_visible() {
                if let Some(caret) = caret {
                    return self.show_at(caret);
                }
            }

            // SAFETY: The window is live and the null rectangle invalidates the
            // complete client area.
            let invalidated = unsafe { InvalidateRect(self.hwnd, None, TRUE) };
            if !invalidated.as_bool() {
                return Err(PopupError::win32(
                    "redraw invalidation",
                    WindowsError::from_win32(),
                ));
            }

            Ok(())
        }

        /// Replaces model state without changing visibility or native geometry.
        pub fn update_snapshot(
            &mut self,
            snapshot: &CandidatePopupSnapshot,
        ) -> Result<(), PopupError> {
            self.ensure_alive()?;
            let mut state = self
                .state
                .as_deref()
                .ok_or(PopupError::Destroyed)?
                .try_borrow_mut()
                .map_err(|_| state_busy_error())?;
            if state.snapshot.as_ref() != Some(snapshot) {
                state.scroll_offset = None;
            }
            state.snapshot = Some(snapshot.clone());
            state.presented = None;
            state.pending_actions.clear();
            state.pending_settings = None;
            Ok(())
        }

        /// 查询插入符目标显示器的逻辑内容宽预算。
        /// 跨屏时短暂创建独立隐藏探针；不移动弹窗、不更改宿主或线程 DPI。
        /// 仅供成功编辑后的发布路径使用，不用于按键试探。失败保留默认预算。
        pub fn candidate_width_budget(&self, caret: PopupRect) -> Result<u32, PopupError> {
            self.ensure_alive()?;
            let native = windows_rect(caret);
            // SAFETY: The rectangle is valid storage and neither query mutates windows.
            let target = unsafe { MonitorFromRect(&native, MONITOR_DEFAULTTONEAREST) };
            if target.is_invalid() {
                return Ok(544);
            }
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: The monitor is valid and the output structure has its required size.
            if !unsafe { GetMonitorInfoW(target, &mut info) }.as_bool() {
                return Ok(544);
            }
            // SAFETY: `self.hwnd` is live. Only a same-monitor popup supplies target DPI.
            let dpi = if unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) } == target
            {
                unsafe { GetDpiForWindow(self.hwnd) }
            } else {
                // Built-in STATIC needs no custom class or callback. No WS_VISIBLE,
                // owner, parent, focus API, or awareness mutation is involved.
                let probe = unsafe {
                    CreateWindowExW(
                        WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                        windows::core::w!("STATIC"),
                        PCWSTR::null(),
                        WS_POPUP,
                        caret.left,
                        caret.top,
                        1,
                        1,
                        HWND::default(),
                        HMENU::default(),
                        HINSTANCE::default(),
                        None,
                    )
                };
                let Ok(probe) = probe else {
                    return Ok(544);
                };
                let mut guard = HiddenDpiProbe(Some(probe));
                // SAFETY: Both windows are live. The guard destroys the probe on
                // every return path, including monitor/context validation failure.
                let valid = unsafe {
                    let probe_context = GetWindowDpiAwarenessContext(probe);
                    let popup_context = GetWindowDpiAwarenessContext(self.hwnd);
                    MonitorFromWindow(probe, MONITOR_DEFAULTTONEAREST) == target
                        && !probe_context.0.is_null()
                        && !popup_context.0.is_null()
                        && AreDpiAwarenessContextsEqual(probe_context, popup_context).as_bool()
                };
                if !valid {
                    return Ok(544);
                }
                let dpi = unsafe { GetDpiForWindow(probe) };
                if !guard.destroy() {
                    return Ok(544);
                }
                dpi
            };
            Ok(target_candidate_width_budget(
                rect_from_windows(info.rcWork).width(),
                dpi,
            ))
        }

        /// 根据插入符位置选择显示器，重算目标 DPI 布局后再显示。
        ///
        /// 弹窗不调用前台或焦点 API。跨显示器时先把隐藏窗口移动到目标显示器，
        /// 让 `GetDpiForWindow` 返回目标 DPI，再按工作区限制尺寸。
        pub fn show_at(&self, caret: PopupRect) -> Result<(), PopupError> {
            let result = self.show_at_inner(caret);
            if result.is_err() {
                self.hide();
            }
            result
        }

        fn show_at_inner(&self, caret: PopupRect) -> Result<(), PopupError> {
            self.ensure_alive()?;
            let snapshot = self.snapshot().ok_or(PopupError::NoSnapshot)?;
            if snapshot.candidates.is_empty() {
                self.hide();
                return Ok(());
            }
            let status = self
                .state
                .as_deref()
                .ok_or(PopupError::Destroyed)?
                .try_borrow()
                .map_err(|_| state_busy_error())?
                .ai_status;
            {
                let mut state = self
                    .state
                    .as_deref()
                    .ok_or(PopupError::Destroyed)?
                    .try_borrow_mut()
                    .map_err(|_| state_busy_error())?;
                state.pending_actions.clear();
                state.pending_settings = None;
                state.presented = None;
            }
            let native_caret = RECT {
                left: caret.left,
                top: caret.top,
                right: caret.right,
                bottom: caret.bottom,
            };

            // SAFETY: `native_caret` is live for the query. The nearest monitor
            // fallback covers caret rectangles outside a particular monitor.
            let monitor = unsafe { MonitorFromRect(&native_caret, MONITOR_DEFAULTTONEAREST) };
            if monitor.is_invalid() {
                return Err(PopupError::win32(
                    "monitor lookup",
                    WindowsError::from_win32(),
                ));
            }
            let mut monitor_info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: The monitor handle came from `MonitorFromRect`, and
            // `monitor_info` is a valid output structure with `cbSize` set.
            if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
                return Err(PopupError::win32(
                    "monitor work area",
                    WindowsError::from_win32(),
                ));
            }
            let work_area = rect_from_windows(monitor_info.rcWork);
            let same_monitor =
                unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) == monitor };

            // Move while hidden with a temporary size that fits the selected
            // monitor. This guarantees that `GetDpiForWindow` observes the
            // target monitor rather than an adjacent monitor at the caret edge.
            if !same_monitor {
                self.hide();
                let temporary_layout = PopupLayout::new(
                    &snapshot,
                    BASE_DPI,
                    PopupSize::new(work_area.width(), work_area.height()),
                    status,
                );
                let temporary = position_candidate_popup(caret, temporary_layout.size, work_area)
                    .ok_or_else(|| {
                    PopupError::win32(
                        "temporary placement",
                        WindowsError::from_hresult(E_UNEXPECTED),
                    )
                })?;
                unsafe {
                    SetWindowPos(
                        self.hwnd,
                        HWND::default(),
                        temporary.left,
                        temporary.top,
                        temporary.width(),
                        temporary.height(),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                }
                .map_err(|error| PopupError::win32("target monitor move", error))?;
            }
            let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(BASE_DPI);
            let layout = PopupLayout::new(
                &snapshot,
                dpi,
                PopupSize::new(work_area.width(), work_area.height()),
                status,
            );
            let placement =
                position_candidate_popup(caret, layout.size, work_area).ok_or_else(|| {
                    PopupError::win32("empty work area", WindowsError::from_hresult(E_UNEXPECTED))
                })?;

            {
                let state = self
                    .state
                    .as_deref()
                    .ok_or(PopupError::Destroyed)?
                    .try_borrow()
                    .map_err(|_| state_busy_error())?;
                if state.snapshot.as_ref() != Some(&snapshot) {
                    return Err(PopupError::NoSnapshot);
                }
            }
            {
                let mut state = self
                    .state
                    .as_deref()
                    .ok_or(PopupError::Destroyed)?
                    .try_borrow_mut()
                    .map_err(|_| state_busy_error())?;
                state.dpi = dpi;
                state.last_caret = Some(caret);
            }

            // SAFETY: The window is live. SWP_SHOWWINDOW shows it without
            // activation, and the placement is already clipped to the monitor
            // work area. No foreground or focus API is called.
            let mut current = RECT::default();
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetWindowRect(self.hwnd, &mut current)
            }
            .map_err(|error| PopupError::win32("window geometry", error))?;
            if !self.is_visible() || rect_from_windows(current) != placement {
                unsafe {
                    SetWindowPos(
                        self.hwnd,
                        HWND::default(),
                        placement.left,
                        placement.top,
                        layout.size.width,
                        layout.size.height,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    )
                }
                .map_err(|error| PopupError::win32("show", error))?;
            }

            // SAFETY: The window is live and the null rectangle invalidates the
            // complete client area after any cross-monitor DPI change.
            if !unsafe { InvalidateRect(self.hwnd, None, TRUE) }.as_bool() {
                return Err(PopupError::win32(
                    "show invalidation",
                    WindowsError::from_win32(),
                ));
            }

            Ok(())
        }

        fn snapshot(&self) -> Option<CandidatePopupSnapshot> {
            let state = self.state.as_deref()?;
            let state = state.try_borrow().ok()?;
            state.snapshot.clone()
        }

        /// 隐藏窗口并取消仍在排队的点击。
        pub fn hide(&self) {
            let Some(state) = self.state.as_deref() else {
                return;
            };
            if let Ok(mut state) = state.try_borrow_mut() {
                state.pending_actions.clear();
                state.pending_settings = None;
                state.presented = None;
            }
            if self.ensure_alive().is_err() {
                return;
            }

            // SAFETY: The handle belongs to this live popup. SW_HIDE hides the
            // window without changing keyboard focus.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }

        /// Reports whether the popup is currently visible.
        pub fn is_visible(&self) -> bool {
            if self.ensure_alive().is_err() {
                return false;
            }

            // SAFETY: The handle belongs to this popup; the query has no
            // pointer arguments and does not change window state.
            unsafe { IsWindowVisible(self.hwnd).as_bool() }
        }

        /// Destroys the native window while retaining the Rust-owned state.
        pub fn destroy(&mut self) -> Result<(), PopupError> {
            if self.hwnd.is_invalid() {
                self.state = None;
                return self.release_class_lease();
            }

            let hwnd = self.hwnd;
            let state_pointer = self.state_pointer();
            match unsafe { detach_popup_state(hwnd, state_pointer) } {
                DetachOutcome::NotOwned | DetachOutcome::AlreadyDetached => {
                    // The handle was already destroyed, detached, or recycled
                    // by an unrelated window. Never call destructive window
                    // APIs for it.
                    self.hwnd = HWND::default();
                    self.state = None;
                    self.release_class_lease()
                }
                DetachOutcome::Failed => Err(PopupError::win32(
                    "user data detach",
                    WindowsError::from_win32(),
                )),
                DetachOutcome::Detached => {
                    // SAFETY: Ownership was confirmed by `detach_popup_state`,
                    // which also cleared GWLP_USERDATA before this call.
                    let destroy_result = unsafe { DestroyWindow(hwnd) };
                    self.hwnd = HWND::default();
                    self.state = None;
                    let class_result = self.release_class_lease();
                    match destroy_result {
                        Ok(()) => class_result,
                        Err(error) => Err(PopupError::win32("destroy", error)),
                    }
                }
            }
        }

        fn ensure_alive(&self) -> Result<(), PopupError> {
            if self.window_is_owned() {
                Ok(())
            } else {
                Err(PopupError::Destroyed)
            }
        }

        fn window_is_owned(&self) -> bool {
            if self.hwnd.is_invalid() {
                return false;
            }
            let Some(state) = self.state.as_deref() else {
                return false;
            };
            let expected = state as *const RefCell<PopupState>;

            // SAFETY: This is a read-only identity check against the window's
            // user data. It avoids any destructive operation on a recycled
            // handle that no longer belongs to this object.
            let current = unsafe { popup_state_pointer(self.hwnd) } as *const RefCell<PopupState>;
            std::ptr::eq(current, expected)
        }

        fn state_pointer(&self) -> *const RefCell<PopupState> {
            self.state.as_deref().map_or(std::ptr::null(), |state| {
                state as *const RefCell<PopupState>
            })
        }

        fn release_class_lease(&mut self) -> Result<(), PopupError> {
            let Some(mut class_lease) = self.class_lease.take() else {
                return Ok(());
            };
            class_lease
                .release()
                .map_err(|error| PopupError::win32("class unregister", error))
        }
    }

    impl Drop for CandidatePopupWindow {
        fn drop(&mut self) {
            if !self.hwnd.is_invalid() {
                let hwnd = self.hwnd;
                let state_pointer = self.state_pointer();
                match unsafe { detach_popup_state(hwnd, state_pointer) } {
                    DetachOutcome::NotOwned | DetachOutcome::AlreadyDetached => {
                        self.state = None;
                    }
                    DetachOutcome::Failed => {
                        if let Some(state) = self.state.take() {
                            // The window may still reference this state, and
                            // the association could not be cleared. Leaking it
                            // is the only safe fallback.
                            std::mem::forget(state);
                        }
                    }
                    DetachOutcome::Detached => {
                        // SAFETY: Ownership was confirmed and the state pointer
                        // was cleared, so destroying the confirmed window
                        // cannot race a stale WndProc state pointer.
                        let _ = unsafe { DestroyWindow(hwnd) };
                        self.state = None;
                    }
                }
                self.hwnd = HWND::default();
            } else {
                self.state = None;
            }

            let _ = self.release_class_lease();
        }
    }

    unsafe extern "system" fn popup_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        #[cfg(test)]
        MESSAGE_COUNTS.with(|counts| {
            let mut counts = counts.borrow_mut();
            if counts.0 == hwnd {
                use windows::Win32::UI::WindowsAndMessaging::{
                    WM_SHOWWINDOW, WM_WINDOWPOSCHANGING,
                };
                match message {
                    WM_SHOWWINDOW => counts.1 += 1,
                    WM_WINDOWPOSCHANGING => counts.2 += 1,
                    WM_PAINT => counts.3 += 1,
                    _ => {}
                }
            }
        });
        match catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: Windows invokes this procedure only for live windows of
            // the registered popup class. The raw state pointer is non-owning
            // and remains valid until the owning struct is dropped.
            unsafe { handle_popup_message(hwnd, message, wparam, lparam) }
        })) {
            Ok(result) => result,
            Err(_) => {
                // SAFETY: This fallback uses the same live window arguments.
                // Letting DefWindowProcW handle the message keeps panics from
                // unwinding across the Windows ABI.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
    }

    unsafe fn handle_popup_message(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_NCCREATE => {
                if lparam.0 != 0 {
                    let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
                    // SAFETY: The creation parameter is the non-owning state
                    // pointer supplied by `CandidatePopupWindow::new`.
                    unsafe {
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                    }
                }
                // SAFETY: Forward the live creation message to the default
                // procedure so window creation can complete normally.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
            WM_LBUTTONDOWN => LRESULT(0),
            windows::Win32::UI::WindowsAndMessaging::WM_WINDOWPOSCHANGED => {
                if let Some(state) = unsafe { popup_state_pointer(hwnd).as_ref() } {
                    if let Ok(mut state) = state.try_borrow_mut() {
                        state.presented = None;
                    }
                }
                // SAFETY: Forward our live window's position change.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
            windows::Win32::UI::WindowsAndMessaging::WM_MOUSEWHEEL => {
                let mut client = RECT::default();
                // SAFETY: Live window and writable rectangle.
                if unsafe { GetClientRect(hwnd, &mut client) }.is_ok() {
                    let data = unsafe { popup_state_pointer(hwnd).as_ref() }
                        .and_then(|s| s.try_borrow().ok())
                        .and_then(|s| {
                            s.snapshot
                                .clone()
                                .map(|snapshot| (snapshot, s.dpi, s.ai_status, s.scroll_offset))
                        });
                    if let Some((snapshot, dpi, status, offset)) = data {
                        let mut layout = PopupLayout::new(
                            &snapshot,
                            dpi,
                            PopupSize::new(client.right, client.bottom),
                            status,
                        );
                        if let Some(offset) = offset {
                            layout.set_scroll(offset);
                        }
                        let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
                        layout.set_scroll(
                            layout
                                .scroll
                                .saturating_sub(delta * layout.line_height / 120),
                        );
                        if let Some(state) = unsafe { popup_state_pointer(hwnd).as_ref() } {
                            if let Ok(mut state) = state.try_borrow_mut() {
                                state.scroll_offset = Some(layout.scroll);
                                state.presented = None;
                            }
                        }
                        // SAFETY: Invalidating our own nonactivating window only.
                        let _ = unsafe { InvalidateRect(hwnd, None, TRUE) };
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let point = point_from_lparam(lparam);
                if settings_at(hwnd, point) {
                    let queued = unsafe { popup_state_pointer(hwnd).as_ref() }
                        .and_then(|s| s.try_borrow_mut().ok())
                        .map(|mut s| {
                            s.pending_settings =
                                s.snapshot.clone().map(|snapshot| (point, snapshot));
                        })
                        .is_some();
                    if queued
                        && unsafe {
                            PostMessageW(
                                hwnd,
                                WM_FLOWER_SETTINGS,
                                WPARAM::default(),
                                LPARAM::default(),
                            )
                        }
                        .is_err()
                    {
                        if let Some(s) = unsafe { popup_state_pointer(hwnd).as_ref() } {
                            if let Ok(mut s) = s.try_borrow_mut() {
                                s.pending_settings = None;
                            }
                        }
                    }
                } else if let Some(action) = popup_action_at(hwnd, point) {
                    let _ = queue_popup_action(hwnd, action);
                }
                LRESULT(0)
            }
            WM_FLOWER_SETTINGS => {
                let pending = unsafe { popup_state_pointer(hwnd).as_ref() }
                    .and_then(|s| s.try_borrow_mut().ok())
                    .and_then(|mut s| s.pending_settings.take());
                if let Some((point, snapshot)) = pending {
                    if settings_at(hwnd, point) {
                        let sink = unsafe { popup_state_pointer(hwnd).as_ref() }
                            .and_then(|s| s.try_borrow().ok())
                            .filter(|s| s.snapshot.as_ref() == Some(&snapshot))
                            .and_then(|s| s.settings_sink.clone());
                        if let Some(sink) = sink {
                            sink();
                        }
                    }
                }
                LRESULT(0)
            }
            WM_FLOWER_DISPATCH_ACTION => {
                dispatch_one_popup_action(hwnd);
                LRESULT(0)
            }
            windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == AI_IDLE_TIMER => {
                let callback = unsafe { popup_state_pointer(hwnd).as_ref() }
                    .and_then(|state| state.try_borrow().ok())
                    .and_then(|state| state.idle_sink.clone());
                if let Some(callback) = callback {
                    callback();
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let render_data = popup_render_data(hwnd);
                paint_popup(hwnd, render_data.as_ref());
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_SYSCOLORCHANGE => {
                // System palettes are sampled during painting, so a system
                // color change only needs a repaint.
                // SAFETY: The window is live and the null rectangle invalidates
                // the complete client area.
                let _ = unsafe { InvalidateRect(hwnd, None, TRUE) };
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let new_dpi = (wparam.0 & 0xffff) as u32;
                let snapshot = {
                    // SAFETY: The user data slot is either null or points to
                    // the live RefCell retained by the owning Rust struct.
                    unsafe { popup_state_pointer(hwnd).as_ref() }.and_then(|state| {
                        if let Ok(mut state) = state.try_borrow_mut() {
                            state.dpi = new_dpi.max(1);
                            state.scroll_offset = None;
                            state.presented = None;
                            state
                                .snapshot
                                .clone()
                                .map(|snapshot| (snapshot, state.ai_status))
                        } else {
                            None
                        }
                    })
                };

                if lparam.0 != 0 {
                    // SAFETY: WM_DPICHANGED supplies a pointer to a RECT that is
                    // valid for the duration of this message.
                    let suggested = unsafe { *(lparam.0 as *const RECT) };
                    if let Some((snapshot, status)) = snapshot.as_ref() {
                        let work_area = monitor_work_area(rect_from_windows(suggested));
                        if let Some(work_area) = work_area {
                            let layout = PopupLayout::new(
                                snapshot,
                                new_dpi,
                                PopupSize::new(work_area.width(), work_area.height()),
                                *status,
                            );
                            let requested = PopupRect::new(
                                suggested.left,
                                suggested.top,
                                suggested.left.saturating_add(layout.size.width),
                                suggested.top.saturating_add(layout.size.height),
                            );
                            if let Some(placement) = clamp_candidate_popup(requested, work_area) {
                                // SAFETY: The window is live. The suggested
                                // rectangle is applied without activation.
                                let _ = unsafe {
                                    SetWindowPos(
                                        hwnd,
                                        HWND::default(),
                                        placement.left,
                                        placement.top,
                                        placement.width(),
                                        placement.height(),
                                        SWP_NOZORDER | SWP_NOACTIVATE,
                                    )
                                };
                            }
                        }
                    }
                }

                // SAFETY: The window is live and the null rectangle invalidates
                // the complete client area after the DPI change.
                let _ = unsafe { InvalidateRect(hwnd, None, TRUE) };
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let _ = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, AI_IDLE_TIMER)
                };
                // SAFETY: The user data slot is either null or points to the
                // live RefCell retained by the owning Rust struct.
                if let Some(state) = unsafe { popup_state_pointer(hwnd).as_ref() } {
                    if let Ok(mut state) = state.try_borrow_mut() {
                        state.pending_actions.clear();
                        state.pending_settings = None;
                        state.action_sink = None;
                        state.idle_sink = None;
                    }
                }
                // SAFETY: The window is being destroyed. Clear the non-owning
                // state pointer before forwarding destruction to the default
                // procedure; the Box remains owned by CandidatePopupWindow.
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
                // SAFETY: Forward the live destruction message and its original
                // arguments to the default procedure.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
            _ => {
                // SAFETY: Forward unhandled live messages to the default
                // procedure.
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
    }

    unsafe fn popup_state_pointer(hwnd: HWND) -> *mut RefCell<PopupState> {
        // SAFETY: This reads the window's user data slot for a live window.
        let value = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        value as *mut RefCell<PopupState>
    }

    fn point_from_lparam(lparam: LPARAM) -> PopupPoint {
        let packed = lparam.0 as u32;
        PopupPoint {
            x: i32::from((packed as u16) as i16),
            y: i32::from(((packed >> 16) as u16) as i16),
        }
    }

    fn rect_from_windows(rect: RECT) -> PopupRect {
        PopupRect::new(rect.left, rect.top, rect.right, rect.bottom)
    }

    fn monitor_work_area(rect: PopupRect) -> Option<PopupRect> {
        let native = RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        // SAFETY: `native` is valid for the duration of the query.
        let monitor = unsafe { MonitorFromRect(&native, MONITOR_DEFAULTTONEAREST) };
        if monitor.is_invalid() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: The monitor handle is live, and `info` is writable with its
        // required size initialized.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return None;
        }
        Some(rect_from_windows(info.rcWork))
    }

    fn popup_action_at(hwnd: HWND, point: PopupPoint) -> Option<CandidatePopupAction> {
        validated_frame_at(hwnd, |frame| {
            frame.candidates.hit_test(&frame.snapshot, point)
        })
    }

    fn settings_at(hwnd: HWND, point: PopupPoint) -> bool {
        if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd) }.as_bool() {
            return false;
        }
        validated_frame_at(hwnd, |frame| {
            frame
                .candidates
                .settings_button
                .filter(|r| r.contains(point))
        })
        .is_some()
    }

    fn validated_frame_at<T>(
        hwnd: HWND,
        hit: impl FnOnce(&PresentedCandidates) -> Option<T>,
    ) -> Option<T> {
        let mut client = RECT::default();
        let mut window = RECT::default();
        // SAFETY: `hwnd` is live and `client` is valid writable storage.
        if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
            return None;
        }
        if unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut window) }
            .is_err()
        {
            return None;
        }

        // SAFETY: The user data slot is either null or points to the live
        // RefCell retained by the owning Rust struct.
        let state = unsafe { popup_state_pointer(hwnd).as_ref()? };
        let state = state.try_borrow().ok()?;
        let frame = state.presented.as_ref()?;
        if state.snapshot.as_ref() != Some(&frame.snapshot)
            || state.dpi != frame.dpi
            || state.ai_status != frame.status
            || state.scroll_offset != frame.offset
            || rect_from_windows(client) != frame.client
            || rect_from_windows(window) != frame.window
        {
            return None;
        }
        // Only geometry from an actually presented frame authorizes mouse input.
        hit(frame)
    }

    fn queue_popup_action(hwnd: HWND, action: CandidatePopupAction) -> bool {
        let queued = {
            // SAFETY: The user data slot is either null or points to the live
            // RefCell retained by the owning Rust struct.
            let Some(state) = (unsafe { popup_state_pointer(hwnd).as_ref() }) else {
                return false;
            };
            let Ok(mut state) = state.try_borrow_mut() else {
                return false;
            };
            if state.action_sink.is_none() {
                false
            } else {
                state.pending_actions.push_back(action);
                true
            }
        };
        if !queued {
            return false;
        }

        // SAFETY: The window is live. A custom message defers the callback until
        // WM_LBUTTONUP handling has completely returned.
        unsafe {
            PostMessageW(
                hwnd,
                WM_FLOWER_DISPATCH_ACTION,
                WPARAM::default(),
                LPARAM::default(),
            )
        }
        .is_ok()
    }

    fn dispatch_one_popup_action(hwnd: HWND) {
        let (action, sink, has_more) = {
            // SAFETY: The user data slot is either null or points to the live
            // RefCell retained by the owning Rust struct.
            let Some(state) = (unsafe { popup_state_pointer(hwnd).as_ref() }) else {
                return;
            };
            let Ok(mut state) = state.try_borrow_mut() else {
                return;
            };
            let action = state.pending_actions.pop_front();
            let sink = state.action_sink.clone();
            let has_more = !state.pending_actions.is_empty();
            (action, sink, has_more)
        };

        if let (Some(action), Some(sink)) = (action, sink) {
            // The RefCell borrow has ended. The sink only starts asynchronous
            // TSF work and must not perform an edit while inside WndProc.
            sink(action);
        }
        if has_more {
            // SAFETY: The window is live; defer the next queued action.
            let _ = unsafe {
                PostMessageW(
                    hwnd,
                    WM_FLOWER_DISPATCH_ACTION,
                    WPARAM::default(),
                    LPARAM::default(),
                )
            };
        }
    }

    unsafe fn detach_popup_state(
        hwnd: HWND,
        expected: *const RefCell<PopupState>,
    ) -> DetachOutcome {
        if expected.is_null() || hwnd.is_invalid() {
            return DetachOutcome::NotOwned;
        }

        // SAFETY: This reads the live window's user data slot. The expected
        // pointer comparison avoids writing to a recycled, unrelated HWND.
        let current = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if current != expected as isize {
            return if current == 0 {
                DetachOutcome::AlreadyDetached
            } else {
                DetachOutcome::NotOwned
            };
        }

        // SAFETY: The slot still contains exactly the state pointer owned by
        // this object. Clearing it prevents a later WndProc from observing the
        // state after the Box is released.
        if unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) == expected as isize } {
            DetachOutcome::Detached
        } else {
            DetachOutcome::Failed
        }
    }

    fn popup_render_data(
        hwnd: HWND,
    ) -> Option<(CandidatePopupSnapshot, u32, Theme, Option<AiStatus>)> {
        // SAFETY: The user data slot is either null or points to the live
        // RefCell retained by the owning Rust struct.
        let state = unsafe { popup_state_pointer(hwnd).as_ref()? };
        let state = state.try_borrow().ok()?;
        let snapshot = state.snapshot.clone()?;
        let dpi = state.dpi.max(1);
        let theme = state.theme;

        // The borrow ends here, before any paint or other Win32 call.
        Some((snapshot, dpi, theme, state.ai_status))
    }

    fn paint_popup(
        hwnd: HWND,
        render_data: Option<&(CandidatePopupSnapshot, u32, Theme, Option<AiStatus>)>,
    ) {
        clear_presented_candidates(hwnd);
        let mut paint = PAINTSTRUCT::default();
        // SAFETY: `hwnd` is live and `paint` is valid writable storage.
        let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
        if hdc.is_invalid() {
            return;
        }

        let fallback = paint.rcPaint;
        let _paint_guard = PaintGuard { hwnd, paint };

        let mut client = RECT::default();
        // SAFETY: `hwnd` is live and `client` is valid writable storage.
        if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
            fill_rect(hdc, &fallback, COLORREF(system_popup_palette().background));
            return;
        }
        paint_popup_contents(hwnd, hdc, client, render_data);
    }

    fn clear_presented_candidates(hwnd: HWND) {
        if let Some(state) = unsafe { popup_state_pointer(hwnd).as_ref() } {
            if let Ok(mut state) = state.try_borrow_mut() {
                state.presented = None;
                state.pending_settings = None;
            }
        }
    }

    /// A bounded eight-tooth vector silhouette with a genuinely empty hub.
    /// Regions need no selected GDI objects or font and never touch adjacent rows.
    fn paint_settings_gear(hdc: HDC, rect: PopupRect, dpi: u32, palette: PopupPalette) -> bool {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::{
            CombineRgn, CreateEllipticRgn, CreatePolygonRgn, FillRgn, ALTERNATE, ERROR, RGN_DIFF,
        };
        let size = scaled(18, dpi);
        if size < 8 || rect.width() < size + 2 || rect.height() < size + 2 {
            return false;
        }
        let left = rect.left + (rect.width() - size) / 2;
        let top = rect.top + (rect.height() - size) / 2;
        let radius = f64::from(size) / 2.0;
        let points: Vec<POINT> = (0..64)
            .map(|i| {
                let angle = (f64::from(i) - 0.5) * std::f64::consts::TAU / 64.0;
                let r = radius * if i % 8 < 4 { 1.0 } else { 0.76 };
                POINT {
                    x: (f64::from(left) + radius + angle.cos() * r).round() as i32,
                    y: (f64::from(top) + radius + angle.sin() * r).round() as i32,
                }
            })
            .collect();
        // Every handle is owned locally; even failed construction/combination is cleaned up.
        unsafe {
            let gear = CreatePolygonRgn(&points, ALTERNATE);
            let inset = (size * 5 / 16).max(1);
            let hub = CreateEllipticRgn(
                left + inset,
                top + inset,
                left + size - inset,
                top + size - inset,
            );
            let brush = CreateSolidBrush(COLORREF(palette.text));
            let painted = !gear.is_invalid()
                && !hub.is_invalid()
                && !brush.is_invalid()
                && CombineRgn(gear, gear, hub, RGN_DIFF).0 != ERROR
                && FillRgn(hdc, gear, brush).as_bool();
            if !brush.is_invalid() {
                let _ = DeleteObject(brush);
            }
            if !hub.is_invalid() {
                let _ = DeleteObject(hub);
            }
            if !gear.is_invalid() {
                let _ = DeleteObject(gear);
            }
            painted
        }
    }

    // Shared by WM_PAINT and offscreen failure tests with a real popup state.
    fn paint_popup_contents(
        hwnd: HWND,
        hdc: HDC,
        client: RECT,
        render_data: Option<&(CandidatePopupSnapshot, u32, Theme, Option<AiStatus>)>,
    ) {
        clear_presented_candidates(hwnd);
        let mut window = RECT::default();
        if unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut window) }
            .is_err()
        {
            return;
        }
        let Some((snapshot, dpi, theme, status)) = render_data else {
            fill_rect(hdc, &client, COLORREF(system_popup_palette().background));
            return;
        };
        let palette = popup_palette(*theme, system_popup_palette());
        fill_rect(hdc, &client, COLORREF(palette.background));
        let dpi = *dpi;
        let font = PaintFont::new(hdc, dpi);

        // SAFETY: The paint device context is live for the duration of this
        // function, and the font, colors, and text buffers stay alive for each
        // corresponding call.
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
        }

        let mut layout = PopupLayout::new(
            snapshot,
            dpi,
            PopupSize::new(client.right - client.left, client.bottom - client.top),
            *status,
        );
        let offset = unsafe { popup_state_pointer(hwnd).as_ref() }
            .and_then(|s| s.try_borrow().ok())
            .and_then(|s| s.scroll_offset);
        if let Some(offset) = offset {
            layout.set_scroll(offset);
        }
        let presented =
            font.is_some() && paint_candidate_body(hdc, snapshot, dpi, palette, &layout);
        let settings_painted = presented
            && layout
                .candidates
                .settings_button
                .map_or(true, |rect| paint_settings_gear(hdc, rect, dpi, palette));

        if let (Some(status), Some(rect)) = (status, layout.status) {
            let padding = scaled(8, dpi).min(rect.width() / 2).max(0);
            let mut text_rect = windows_rect(PopupRect::new(
                rect.left + padding,
                rect.top,
                rect.right - padding,
                rect.bottom,
            ));
            let (text, _) =
                fitted_candidate_text(hdc, status.label(), text_rect.right - text_rect.left);
            set_text_color(hdc, COLORREF(palette.text));
            draw_text(
                hdc,
                &text,
                &mut text_rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        }
        if presented && settings_painted {
            if let Some(state) = unsafe { popup_state_pointer(hwnd).as_ref() } {
                if let Ok(mut state) = state.try_borrow_mut() {
                    if state.snapshot.as_ref() == Some(snapshot)
                        && state.dpi == dpi
                        && state.ai_status == *status
                        && state.scroll_offset == offset
                    {
                        state.presented = Some(PresentedCandidates {
                            snapshot: snapshot.clone(),
                            dpi,
                            status: *status,
                            offset,
                            client: rect_from_windows(client),
                            window: rect_from_windows(window),
                            candidates: layout.candidates,
                        });
                    }
                }
            }
        }
        drop(font);
    }

    fn paint_candidate_body(
        hdc: HDC,
        snapshot: &CandidatePopupSnapshot,
        dpi: u32,
        palette: PopupPalette,
        layout: &PopupLayout,
    ) -> bool {
        // Clip both text and highlight before painting the separately owned status.
        let saved = save_candidate_dc(hdc);
        if saved == 0 {
            return false;
        }
        let clipped = clip_candidate_dc(hdc, layout.viewport);
        // ERROR means no clipping guarantee. NULLREGION is a valid empty frame,
        // but likewise cannot authorize any candidate mouse target.
        if clipped == 0 || clipped == 1 {
            let _ = unsafe { RestoreDC(hdc, saved) };
            return false;
        }
        for (index, (row, lines)) in layout.full_rows.iter().zip(&layout.lines).enumerate() {
            let row = PopupRect::new(
                row.left,
                row.top - layout.scroll,
                row.right,
                row.bottom - layout.scroll,
            );
            if row.bottom <= layout.viewport.top || row.top >= layout.viewport.bottom {
                continue;
            }
            let width = lines
                .iter()
                .filter_map(|s| measured_width(hdc, s))
                .max()
                .unwrap_or(0);
            let highlighted = snapshot.highlighted == Some(index);
            if highlighted {
                let visible = PopupRect::new(
                    row.left,
                    row.top.max(layout.viewport.top),
                    row.right,
                    row.bottom.min(layout.viewport.bottom),
                );
                let highlight = candidate_highlight_rect(visible, width, dpi);
                let painted = fill_candidate_highlight(
                    hdc,
                    &highlight,
                    dpi,
                    COLORREF(palette.highlight),
                    COLORREF(palette.background),
                );
                set_text_color(
                    hdc,
                    COLORREF(if painted {
                        palette.highlight_text
                    } else {
                        palette.text
                    }),
                );
            } else {
                set_text_color(hdc, COLORREF(palette.text));
            }

            for (line_index, line) in lines.iter().enumerate() {
                let top = row.top + line_index as i32 * layout.line_height;
                let mut line_rect = windows_rect(PopupRect::new(
                    row.left,
                    top,
                    row.right,
                    (top + layout.line_height).min(row.bottom),
                ));
                draw_text(
                    hdc,
                    line,
                    &mut line_rect,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                );
            }
        }
        let _ = unsafe { RestoreDC(hdc, saved) };
        if layout.max_scroll > 0 {
            // A slim track and thumb advertise internal scrolling without engine paging.
            let track = PopupRect::new(
                (layout.viewport.right - scaled(3, dpi)).max(0),
                layout.viewport.top,
                layout.viewport.right,
                layout.viewport.bottom,
            );
            fill_rect(hdc, &windows_rect(track), COLORREF(palette.background));
            let thumb_height = (track.height() / 4).max(1);
            let top = track.top
                + ((i64::from(track.height() - thumb_height) * i64::from(layout.scroll))
                    / i64::from(layout.max_scroll)) as i32;
            fill_rect(
                hdc,
                &windows_rect(PopupRect::new(
                    track.left,
                    top,
                    track.right,
                    top + thumb_height,
                )),
                COLORREF(palette.highlight),
            );
        }
        true
    }

    #[cfg(test)]
    thread_local! {
        static CANDIDATE_CLIP_FAULT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    }

    fn save_candidate_dc(hdc: HDC) -> i32 {
        #[cfg(test)]
        if CANDIDATE_CLIP_FAULT.with(|fault| fault.get() == 1) {
            return 0;
        }
        // SAFETY: The caller owns a live paint or memory DC.
        unsafe { SaveDC(hdc) }
    }

    fn clip_candidate_dc(hdc: HDC, viewport: PopupRect) -> i32 {
        #[cfg(test)]
        match CANDIDATE_CLIP_FAULT.with(|fault| fault.get()) {
            2 => {
                // Simulate an error after a DC mutation to verify restoration.
                unsafe {
                    IntersectClipRect(hdc, 0, 0, 1, 1);
                }
                return 0;
            }
            3 => {
                unsafe {
                    IntersectClipRect(hdc, 0, 0, 0, 0);
                }
                return 1;
            }
            _ => {}
        }
        // SAFETY: The DC is live and SaveDC succeeded.
        unsafe {
            IntersectClipRect(
                hdc,
                viewport.left,
                viewport.top,
                viewport.right,
                viewport.bottom,
            )
        }
        .0
    }

    // Uses the currently selected paint font, not the engine's paging estimate.
    fn measured_width(hdc: HDC, text: &str) -> Option<i32> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return Some(0);
        }
        let mut size = SIZE::default();
        // SAFETY: The live DC and UTF-16 slice remain valid for this call.
        (unsafe { GetTextExtentPoint32W(hdc, &wide, &mut size) }.as_bool() && size.cx >= 0)
            .then_some(size.cx)
    }

    fn fitted_candidate_text(hdc: HDC, text: &str, available: i32) -> (String, i32) {
        fit_candidate_text(text, available, |text| measured_width(hdc, text))
    }

    fn candidate_highlight_rect(cell: PopupRect, measured_width: i32, dpi: u32) -> RECT {
        let width = if measured_width > 0 {
            measured_width.saturating_add(scaled(6, dpi))
        } else {
            0
        };
        RECT {
            right: cell.left.saturating_add(width).min(cell.right),
            ..windows_rect(cell)
        }
    }

    fn fill_candidate_highlight(
        hdc: HDC,
        rect: &RECT,
        dpi: u32,
        color: COLORREF,
        background: COLORREF,
    ) -> bool {
        let Some(width) = rect.right.checked_sub(rect.left) else {
            return false;
        };
        let Some(height) = rect.bottom.checked_sub(rect.top) else {
            return false;
        };
        if width <= 0 || height <= 0 {
            return false;
        }
        let diameter = scaled(8, dpi).max(1).min(width).min(height);
        // Bound transient storage to 16 MiB even for malformed/oversized rectangles.
        let Some(count) = (width as usize)
            .checked_mul(height as usize)
            .filter(|count| *count <= 4 * 1024 * 1024)
        else {
            return false;
        };
        let mut pixels = Vec::<u32>::new();
        if pixels.try_reserve_exact(count).is_err() {
            return false;
        }
        // COLORREF is 0x00BBGGRR; a little-endian BI_RGB word is 0x00RRGGBB.
        let blend = |coverage: u32| {
            let channel = |shift: u32| -> u32 {
                let fg = (color.0 >> shift) & 255;
                let bg = (background.0 >> shift) & 255;
                (fg * coverage + bg * (64 - coverage) + 32) / 64
            };
            (channel(0) << 16) | (channel(8) << 8) | channel(16)
        };
        let solid = blend(64);
        let radius = f64::from(diameter) / 2.0;
        for y in 0..height {
            for x in 0..width {
                // Reflect to one corner so all four coverage masks are identical.
                let cx = x.min(width - 1 - x);
                let cy = y.min(height - 1 - y);
                if f64::from(cx) >= radius || f64::from(cy) >= radius {
                    pixels.push(solid);
                    continue;
                }
                let mut coverage = 0;
                for sy in 0..8 {
                    for sx in 0..8 {
                        let dx = radius - (f64::from(cx) + (f64::from(sx) + 0.5) / 8.0);
                        let dy = radius - (f64::from(cy) + (f64::from(sy) + 0.5) / 8.0);
                        if dx * dx + dy * dy <= radius * radius {
                            coverage += 1;
                        }
                    }
                }
                pixels.push(blend(coverage));
            }
        }
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
        // SAFETY: pixels contains every scanline of the top-down 32-bit DIB.
        (unsafe {
            SetDIBitsToDevice(
                hdc,
                rect.left,
                rect.top,
                width as u32,
                height as u32,
                0,
                0,
                0,
                height as u32,
                pixels.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
            )
        }) == height
    }

    fn fit_candidate_text(
        text: &str,
        available: i32,
        measure: impl Fn(&str) -> Option<i32>,
    ) -> (String, i32) {
        let available = available.max(0);
        let Some(width) = measure(text).filter(|width| *width >= 0) else {
            return (String::new(), 0);
        };
        if width <= available {
            return (text.to_owned(), width);
        }
        // Only truncate display text, at Unicode scalar boundaries (never UTF-16 halves).
        // Measure each final string, including its ellipsis, with the same font.
        let mut ends: Vec<usize> = text.char_indices().map(|(offset, _)| offset).collect();
        ends.push(text.len());
        let mut low = 0;
        let mut high = ends.len();
        let mut best = (String::new(), 0);
        while low < high {
            let middle = low + (high - low) / 2;
            let display = format!("{}...", &text[..ends[middle]]);
            let Some(width) = measure(&display).filter(|width| *width >= 0) else {
                return (String::new(), 0);
            };
            if width <= available {
                best = (display, width);
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        best
    }

    fn system_popup_palette() -> PopupPalette {
        // SAFETY: These queries have no pointer arguments and read the current
        // process's system color table. They do not read Flower's config.
        unsafe {
            PopupPalette {
                background: GetSysColor(COLOR_WINDOW),
                text: GetSysColor(COLOR_WINDOWTEXT),
                highlight: GetSysColor(COLOR_HIGHLIGHT),
                highlight_text: GetSysColor(COLOR_HIGHLIGHTTEXT),
            }
        }
    }

    struct PaintGuard {
        hwnd: HWND,
        paint: PAINTSTRUCT,
    }

    impl Drop for PaintGuard {
        fn drop(&mut self) {
            // SAFETY: BeginPaint succeeded for this live window, and this guard
            // releases that paint exactly once.
            let _ = unsafe { EndPaint(self.hwnd, &self.paint) };
        }
    }

    struct PaintFont {
        hdc: HDC,
        font: HFONT,
        previous: HGDIOBJ,
    }

    impl PaintFont {
        fn new(hdc: HDC, dpi: u32) -> Option<Self> {
            Self::with_height(hdc, dpi, 18)
        }

        fn with_height(hdc: HDC, dpi: u32, height: i32) -> Option<Self> {
            // SAFETY: CreateFontW has no pointer parameters beyond the static
            // null-terminated face name, which remains valid for the call.
            let font = unsafe {
                CreateFontW(
                    -scaled(height, dpi),
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
                return None;
            }

            // SAFETY: The paint device context is live and the font handle was
            // just created. The previous object is restored in Drop.
            let previous = unsafe { SelectObject(hdc, font) };
            Some(Self {
                hdc,
                font,
                previous,
            })
        }
    }

    impl Drop for PaintFont {
        fn drop(&mut self) {
            // SAFETY: This guard owns the selected GDI font for a live paint
            // device context. Restore the previous object before deleting it.
            unsafe {
                if !self.previous.is_invalid() {
                    let _ = SelectObject(self.hdc, self.previous);
                }
                if !self.font.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ::from(self.font));
                }
            }
        }
    }

    fn draw_text(
        hdc: HDC,
        text: &str,
        rect: &mut RECT,
        format: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
    ) {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }

        // SAFETY: Both the paint device context and rectangle are live, and
        // `wide` remains valid for the duration of the call.
        unsafe {
            DrawTextW(hdc, &mut wide, rect, format);
        }
    }

    fn fill_rect(hdc: HDC, rect: &RECT, color: COLORREF) {
        // SAFETY: CreateSolidBrush has no pointer arguments.
        let brush: HBRUSH = unsafe { CreateSolidBrush(color) };
        if brush.is_invalid() {
            return;
        }

        // SAFETY: The device context and rectangle are live. The brush is
        // deleted below after all uses.
        unsafe {
            let _ = FillRect(hdc, rect, brush);
            let _ = DeleteObject(HGDIOBJ::from(brush));
        }
    }

    fn set_text_color(hdc: HDC, color: COLORREF) {
        // SAFETY: The paint device context remains live until EndPaint.
        unsafe {
            SetTextColor(hdc, color);
        }
    }

    fn windows_rect(rect: PopupRect) -> RECT {
        RECT {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }

    fn scaled(value: i32, dpi: u32) -> i32 {
        let dpi = if dpi == 0 { BASE_DPI } else { dpi.min(480) };
        ((i64::from(value) * i64::from(dpi) + i64::from(BASE_DPI / 2)) / i64::from(BASE_DPI)) as i32
    }
    #[cfg(test)]
    mod candidate_paint_tests {
        use super::*;
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, GetPixel, HBITMAP,
        };

        struct FrameSurface {
            dc: HDC,
            bitmap: HBITMAP,
            previous: HGDIOBJ,
            client: RECT,
        }

        impl FrameSurface {
            fn new(hwnd: HWND) -> Self {
                let mut client = RECT::default();
                unsafe {
                    GetClientRect(hwnd, &mut client).unwrap();
                }
                let dc = unsafe { CreateCompatibleDC(None) };
                assert!(!dc.is_invalid());
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: client.right,
                        biHeight: -client.bottom,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bits = std::ptr::null_mut();
                let bitmap =
                    unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                        .unwrap();
                let previous = unsafe { SelectObject(dc, bitmap) };
                Self {
                    dc,
                    bitmap,
                    previous,
                    client,
                }
            }

            fn paint(&self, hwnd: HWND) {
                let data = popup_render_data(hwnd);
                // Exactly the production contents path, including presented-frame
                // invalidation/publication through the real HWND's PopupState.
                paint_popup_contents(hwnd, self.dc, self.client, data.as_ref());
            }
        }

        impl Drop for FrameSurface {
            fn drop(&mut self) {
                unsafe {
                    let _ = SelectObject(self.dc, self.previous);
                    let _ = DeleteObject(self.bitmap);
                    let _ = DeleteDC(self.dc);
                }
            }
        }

        fn paint_native_test_frame(hwnd: HWND) {
            FrameSurface::new(hwnd).paint(hwnd);
        }

        #[test]
        fn settings_button_gear_pixels_dpi_themes_and_failure() {
            for dpi in [96, 144, 192] {
                for palette in [super::super::LIGHT_PALETTE, super::super::DARK_PALETTE] {
                    let popup = CandidatePopupWindow::new().unwrap();
                    unsafe {
                        SetWindowPos(
                            popup.hwnd,
                            HWND::default(),
                            0,
                            0,
                            96,
                            96,
                            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                        )
                        .unwrap();
                    }
                    let surface = FrameSurface::new(popup.hwnd);
                    fill_rect(surface.dc, &surface.client, COLORREF(palette.background));
                    let button = PopupRect::new(8, 8, 8 + scaled(28, dpi), 8 + scaled(28, dpi));
                    assert!(paint_settings_gear(surface.dc, button, dpi, palette));
                    unsafe {
                        windows::Win32::Graphics::Gdi::GdiFlush().unwrap();
                    }
                    let mut ink = 0;
                    for y in 0..96 {
                        for x in 0..96 {
                            let color = unsafe { GetPixel(surface.dc, x, y) }.0;
                            if color != palette.background {
                                assert_eq!(color, palette.text);
                                assert!(x > button.left && x < button.right - 1);
                                assert!(y > button.top && y < button.bottom - 1);
                                ink += 1;
                            }
                        }
                    }
                    assert!(ink > scaled(40, dpi));
                    let cx = (button.left + button.right) / 2;
                    let cy = (button.top + button.bottom) / 2;
                    assert_eq!(
                        unsafe { GetPixel(surface.dc, cx, cy) }.0,
                        palette.background
                    );
                    assert!(!paint_settings_gear(HDC::default(), button, dpi, palette));
                    fill_rect(surface.dc, &surface.client, COLORREF(palette.background));
                    assert!(!paint_settings_gear(
                        surface.dc,
                        PopupRect::new(0, 0, 1, 1),
                        dpi,
                        palette,
                    ));
                    assert_eq!(unsafe { GetPixel(surface.dc, 0, 0) }.0, palette.background);
                }
            }
        }

        #[test]
        fn settings_button_native_deferred_callback_and_stale_frames() {
            use std::cell::Cell;
            use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
            let mut popup = CandidatePopupWindow::new().unwrap();
            let mut snapshot = long_snapshot();
            popup.update(&snapshot).unwrap();
            let calls = Rc::new(Cell::new(0));
            let seen = calls.clone();
            popup
                .set_settings_action_sink(Some(Rc::new(move || seen.set(seen.get() + 1))))
                .unwrap();
            let actions = Rc::new(Cell::new(0));
            let seen = actions.clone();
            popup
                .set_action_sink(Some(Rc::new(move |_| seen.set(seen.get() + 1))))
                .unwrap();
            unsafe {
                SetWindowPos(
                    popup.hwnd,
                    HWND::default(),
                    0,
                    0,
                    240,
                    160,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
                .unwrap();
            }
            paint_native_test_frame(popup.hwnd);
            let button = popup
                .state
                .as_ref()
                .unwrap()
                .borrow()
                .presented
                .as_ref()
                .unwrap()
                .candidates
                .settings_button
                .unwrap();
            let point = PopupPoint {
                x: button.left + 2,
                y: button.top + 2,
            };
            let position = LPARAM(((point.y as u32) << 16 | point.x as u32) as isize);
            assert!(settings_at(popup.hwnd, point));
            assert_eq!(popup_action_at(popup.hwnd, point), None);
            unsafe {
                SendMessageW(popup.hwnd, WM_LBUTTONUP, WPARAM(0), position);
            }
            assert_eq!(calls.get(), 0);
            unsafe {
                SendMessageW(popup.hwnd, WM_FLOWER_SETTINGS, WPARAM(0), LPARAM(0));
            }
            assert_eq!(calls.get(), 1);
            unsafe {
                SendMessageW(popup.hwnd, WM_FLOWER_SETTINGS, WPARAM(0), LPARAM(0));
            }
            assert_eq!(calls.get(), 1);
            assert_eq!(actions.get(), 0);
            snapshot.revision += 1;
            popup.update(&snapshot).unwrap();
            assert!(!settings_at(popup.hwnd, point));
            paint_native_test_frame(popup.hwnd);
            assert!(settings_at(popup.hwnd, point));
            // Scroll identity and window dimensions invalidate even a previously painted button.
            popup.state.as_ref().unwrap().borrow_mut().scroll_offset = Some(1);
            assert!(!settings_at(popup.hwnd, point));
            paint_native_test_frame(popup.hwnd);
            assert!(settings_at(popup.hwnd, point));
            unsafe {
                SetWindowPos(
                    popup.hwnd,
                    HWND::default(),
                    0,
                    0,
                    241,
                    160,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
                .unwrap();
            }
            assert!(!settings_at(popup.hwnd, point));
            paint_native_test_frame(popup.hwnd);
            CANDIDATE_CLIP_FAULT.with(|fault| fault.set(1));
            paint_native_test_frame(popup.hwnd);
            CANDIDATE_CLIP_FAULT.with(|fault| fault.set(0));
            assert!(!settings_at(popup.hwnd, point));
            paint_native_test_frame(popup.hwnd);
            unsafe {
                SendMessageW(popup.hwnd, WM_LBUTTONUP, WPARAM(0), position);
            }
            popup.hide();
            assert!(!settings_at(popup.hwnd, point));
            unsafe {
                SendMessageW(popup.hwnd, WM_FLOWER_SETTINGS, WPARAM(0), LPARAM(0));
            }
            assert_eq!(calls.get(), 1);
            assert_eq!(actions.get(), 0);
        }

        #[test]
        fn settings_button_layout_dpi_narrow_and_candidate_boundaries() {
            let snapshot = long_snapshot();
            for dpi in [96, 144, 192, 288] {
                for width in [1, 20, 40, 120, 240, 1120] {
                    let layout = PopupLayout::new(
                        &snapshot,
                        dpi,
                        PopupSize::new(width, 400),
                        Some(AiStatus::Waiting),
                    );
                    assert!(layout.size.width <= width);
                    assert!(layout.size.width <= scaled(560, dpi));
                    assert_eq!(layout.candidates.footer, None);
                    if let Some(button) = layout.candidates.settings_button {
                        assert_eq!(button.width(), scaled(28, dpi));
                        assert!(button.right <= layout.size.width);
                        assert!(layout.viewport.right <= button.left);
                        assert!(layout.status.unwrap().right <= button.left);
                        for row in &layout.candidates.candidates {
                            assert!(row.right <= button.left);
                        }
                        assert_eq!(
                            layout.candidates.hit_test(
                                &snapshot,
                                PopupPoint {
                                    x: button.left,
                                    y: button.top
                                }
                            ),
                            None
                        );
                    }
                }
            }
        }

        #[test]
        fn clip_failures_disable_real_state_hits_until_successful_frame() {
            use windows::Win32::Graphics::Gdi::{GdiFlush, GetClipBox};
            struct ResetFault;
            impl Drop for ResetFault {
                fn drop(&mut self) {
                    CANDIDATE_CLIP_FAULT.with(|fault| fault.set(0));
                }
            }
            let _reset = ResetFault;
            let mut popup = CandidatePopupWindow::new().unwrap();
            let mut snapshot = long_snapshot();
            popup.update(&snapshot).unwrap();
            popup.set_ai_status(AiStatus::Waiting).unwrap();
            popup.set_theme(Theme::Light).unwrap();
            unsafe {
                SetWindowPos(
                    popup.hwnd,
                    HWND::default(),
                    0,
                    0,
                    240,
                    160,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
                .unwrap();
            }
            // A partial first row is deliberately scrolled across the viewport edge.
            popup.state.as_ref().unwrap().borrow_mut().scroll_offset = Some(11);
            let surface = FrameSurface::new(popup.hwnd);
            let point = PopupPoint { x: 8, y: 10 };
            assert_eq!(popup_action_at(popup.hwnd, point), None);
            surface.paint(popup.hwnd);
            assert!(matches!(
                popup_action_at(popup.hwnd, point),
                Some(CandidatePopupAction::Select { .. })
            ));
            let mut expected_status = Vec::new();
            for fault in [1, 2, 3] {
                CANDIDATE_CLIP_FAULT.with(|value| value.set(fault));
                let mut before = RECT::default();
                unsafe {
                    GetClipBox(surface.dc, &mut before);
                }
                surface.paint(popup.hwnd);
                assert!(unsafe { GdiFlush() }.as_bool());
                let mut after = RECT::default();
                unsafe {
                    GetClipBox(surface.dc, &mut after);
                }
                assert_eq!(rect_from_windows(before), rect_from_windows(after));
                assert!(
                    popup.state.as_ref().unwrap().borrow().presented.is_none(),
                    "fault {fault}"
                );
                for y in 0..138 {
                    for x in 0..240 {
                        assert_eq!(
                            unsafe { GetPixel(surface.dc, x, y) }.0,
                            0xffffff,
                            "fault {fault}: candidate pixels cannot escape or remain visible"
                        );
                        assert_eq!(popup_action_at(popup.hwnd, PopupPoint { x, y }), None);
                    }
                }
                let mut status_pixels = Vec::new();
                for y in 138..160 {
                    for x in 0..240 {
                        status_pixels.push(unsafe { GetPixel(surface.dc, x, y) }.0);
                        assert_eq!(popup_action_at(popup.hwnd, PopupPoint { x, y }), None);
                    }
                }
                assert!(
                    status_pixels.iter().any(|p| *p != 0xffffff),
                    "status still drawn after restoring DC"
                );
                if fault == 1 {
                    expected_status = status_pixels;
                } else {
                    assert_eq!(
                        status_pixels, expected_status,
                        "no status pollution at fault {fault}"
                    );
                }
                CANDIDATE_CLIP_FAULT.with(|value| value.set(0));
                surface.paint(popup.hwnd);
                assert!(matches!(
                    popup_action_at(popup.hwnd, point),
                    Some(CandidatePopupAction::Select {
                        revision: 71,
                        page: 3,
                        index: 0
                    })
                ));
            }
            // A successful old frame cannot authorize a new identity or geometry,
            // even if the geometry is subsequently restored to its old dimensions.
            snapshot.revision += 1;
            popup.update(&snapshot).unwrap();
            assert_eq!(popup_action_at(popup.hwnd, point), None);
            let surface = FrameSurface::new(popup.hwnd);
            surface.paint(popup.hwnd);
            assert!(popup_action_at(popup.hwnd, point).is_some());
            for width in [241, surface.client.right] {
                unsafe {
                    SetWindowPos(
                        popup.hwnd,
                        HWND::default(),
                        0,
                        0,
                        width,
                        surface.client.bottom,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                    .unwrap();
                }
                assert_eq!(popup_action_at(popup.hwnd, point), None);
            }
            surface.paint(popup.hwnd);
            assert!(popup_action_at(popup.hwnd, point).is_some());
            popup.destroy().unwrap();
        }

        fn long_snapshot() -> CandidatePopupSnapshot {
            CandidatePopupSnapshot {
                revision: 71,
                preedit: "chang".into(),
                candidates: vec![
                    crate::engine::Candidate::new(
                        "完整候选文字需要显示直到最后一个尾字".repeat(4),
                        "",
                    ),
                    crate::engine::Candidate::new(
                        "中英English😀𠮷混合".repeat(7),
                        crate::ai::AI_COMMENT,
                    ),
                    crate::engine::Candidate::new("短", ""),
                ],
                highlighted: Some(0),
                page: 3,
                page_count: 9,
            }
        }

        #[test]
        fn long_candidates_gdi_wrap_preserves_tail_scalar_and_ai_marker() {
            let snapshot = long_snapshot();
            for dpi in [96, 144, 192, 480] {
                let dc = MeasureDc::new().unwrap();
                let _font = PaintFont::new(dc.0, dpi).unwrap();
                for logical_width in [160, 560] {
                    let bounds = PopupSize::new(scaled(logical_width, dpi), scaled(2400, dpi));
                    let layout = PopupLayout::new(&snapshot, dpi, bounds, Some(AiStatus::Waiting));
                    assert_eq!(layout.full_rows.len(), snapshot.candidates.len());
                    assert!(layout.lines[0].len() > 1);
                    for (i, lines) in layout.lines.iter().enumerate() {
                        let full = format!("{}. {}", i + 1, snapshot.candidates[i].display_text());
                        assert_eq!(lines.concat(), full, "no lost tail or inserted ellipsis");
                        assert_eq!(
                            lines.len() as i32 * layout.line_height,
                            layout.full_rows[i].height()
                        );
                        for line in lines {
                            assert!(
                                measured_width(dc.0, line).unwrap()
                                    <= layout.full_rows[i].width() - scaled(6, dpi)
                            );
                        }
                        assert!(lines
                            .iter()
                            .skip(1)
                            .all(|s| !s.starts_with(&format!("{}. ", i + 1))));
                    }
                    assert!(layout.lines[0].last().unwrap().ends_with('字'));
                    assert!(layout.lines[1].concat().ends_with("✓ AI"));
                    for (index, row) in layout.candidates.candidates.iter().enumerate() {
                        for y in row.top..row.bottom {
                            assert_eq!(
                                layout
                                    .candidates
                                    .hit_test(&snapshot, PopupPoint { x: row.left, y }),
                                Some(CandidatePopupAction::Select {
                                    revision: 71,
                                    page: 3,
                                    index
                                })
                            );
                        }
                    }
                    let status = layout.status.unwrap();
                    assert_eq!(
                        layout.candidates.hit_test(
                            &snapshot,
                            PopupPoint {
                                x: 20,
                                y: status.top
                            }
                        ),
                        None
                    );
                    assert!(
                        layout.size.width <= bounds.width && layout.size.height <= bounds.height
                    );
                    let caret = PopupRect::new(999, 899, 1000, 900);
                    let work = PopupRect::new(0, 0, bounds.width, bounds.height);
                    let placed = position_candidate_popup(caret, layout.size, work).unwrap();
                    assert!(placed.left >= 0 && placed.top >= 0);
                    assert!(placed.right <= work.right && placed.bottom <= work.bottom);
                }
            }
        }

        #[test]
        fn internal_scroll_reaches_every_line_without_changing_snapshot_or_status() {
            let mut snapshot = long_snapshot();
            let original = snapshot.clone();
            for dpi in [96, 144, 192, 480] {
                let bounds = PopupSize::new(scaled(240, dpi), scaled(160, dpi));
                let mut layout = PopupLayout::new(&snapshot, dpi, bounds, Some(AiStatus::Waiting));
                assert!(layout.max_scroll > 0);
                for (index, row) in layout.full_rows.clone().iter().enumerate() {
                    for y in (row.top..row.bottom).step_by(layout.line_height as usize) {
                        layout.set_scroll(y - layout.viewport.top);
                        let visible_y = y - layout.scroll;
                        assert!(
                            visible_y >= layout.viewport.top && visible_y < layout.viewport.bottom
                        );
                        assert_eq!(
                            layout.candidates.hit_test(
                                &snapshot,
                                PopupPoint {
                                    x: row.left,
                                    y: visible_y
                                }
                            ),
                            Some(CandidatePopupAction::Select {
                                revision: 71,
                                page: 3,
                                index
                            })
                        );
                    }
                }
                layout.set_scroll(i32::MAX);
                assert_eq!(layout.scroll, layout.max_scroll);
                assert_eq!(
                    layout.candidates.hit_test(
                        &snapshot,
                        PopupPoint {
                            x: 10,
                            y: layout.status.unwrap().top
                        }
                    ),
                    None
                );
                assert_eq!(snapshot, original);
                snapshot.highlighted = Some(2);
                let highlighted = PopupLayout::new(&snapshot, dpi, bounds, Some(AiStatus::Waiting));
                let row = highlighted.candidates.candidates[2];
                assert_eq!(row.height(), highlighted.line_height);
                snapshot.highlighted = original.highlighted;
                for height in [1, 7, 20, 40] {
                    let tiny = PopupLayout::new(
                        &snapshot,
                        dpi,
                        PopupSize::new(20, height),
                        Some(AiStatus::Waiting),
                    );
                    assert!(tiny.candidates.candidates.is_empty());
                    assert_eq!(
                        tiny.candidates
                            .hit_test(&snapshot, PopupPoint { x: 1, y: 1 }),
                        None
                    );
                    assert!(tiny.size.width <= 20 && tiny.size.height <= height);
                }
            }
        }

        #[test]
        fn native_wheel_and_hit_testing_share_scroll_without_engine_page_change() {
            use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_MOUSEWHEEL};
            let mut popup = CandidatePopupWindow::new().unwrap();
            let snapshot = long_snapshot();
            popup.update(&snapshot).unwrap();
            popup.set_ai_status(AiStatus::Waiting).unwrap();
            unsafe {
                SetWindowPos(
                    popup.hwnd,
                    HWND::default(),
                    0,
                    0,
                    240,
                    160,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
                .unwrap();
            }
            let mut layout = PopupLayout::new(
                &snapshot,
                96,
                PopupSize::new(240, 160),
                Some(AiStatus::Waiting),
            );
            assert_eq!(layout.scroll, 0);
            unsafe {
                SendMessageW(
                    popup.hwnd,
                    WM_MOUSEWHEEL,
                    WPARAM(((-120i16) as u16 as usize) << 16),
                    LPARAM(0),
                );
            }
            let state = popup.state.as_ref().unwrap().borrow();
            assert_eq!(state.snapshot.as_ref(), Some(&snapshot));
            let offset = state.scroll_offset.unwrap();
            assert_eq!(offset, layout.line_height);
            drop(state);
            layout.set_scroll(offset);
            assert_eq!(
                popup_action_at(popup.hwnd, PopupPoint { x: 8, y: 10 }),
                None
            );
            paint_native_test_frame(popup.hwnd);
            for y in layout.viewport.top..layout.viewport.bottom {
                let point = PopupPoint {
                    x: scaled(8, 96),
                    y,
                };
                assert_eq!(
                    popup_action_at(popup.hwnd, point),
                    layout.candidates.hit_test(&snapshot, point)
                );
            }
            assert_eq!(
                popup_action_at(popup.hwnd, PopupPoint { x: 20, y: 150 }),
                None
            );
            let mut next = snapshot.clone();
            next.highlighted = Some(2);
            popup.update(&next).unwrap();
            assert!(popup
                .state
                .as_ref()
                .unwrap()
                .borrow()
                .scroll_offset
                .is_none());
            popup.destroy().unwrap();
        }

        #[test]
        fn native_same_monitor_refresh_has_no_visibility_flips() {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, MSG, PM_REMOVE,
            };
            let pump = |hwnd| unsafe {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, hwnd, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&message);
                }
            };
            let mut popup = CandidatePopupWindow::new().unwrap();
            let mut snapshot = long_snapshot();
            popup.update_snapshot(&snapshot).unwrap();
            let caret = PopupRect::new(10, 10, 11, 30);
            popup.show_at(caret).unwrap();
            pump(popup.hwnd);
            for step in 0..6 {
                MESSAGE_COUNTS.with(|c| *c.borrow_mut() = (popup.hwnd, 0, 0, 0));
                snapshot.revision += 1;
                if step == 2 {
                    snapshot.candidates.truncate(1);
                }
                popup.update_snapshot(&snapshot).unwrap();
                assert!(popup.is_visible());
                assert!(popup.state.as_ref().unwrap().borrow().presented.is_none());
                popup
                    .show_at(PopupRect::new(10 + step, 10, 11 + step, 30))
                    .unwrap();
                pump(popup.hwnd);
                MESSAGE_COUNTS.with(|c| {
                    let counts = c.borrow();
                    println!(
                        "snapshot step {step}: visibility={}, geometry={}, paint={}",
                        counts.1, counts.2, counts.3
                    );
                    assert_eq!(counts.1, 0, "same-monitor refresh toggled visibility");
                    assert!(counts.2 <= 1, "refresh committed geometry more than once");
                    assert!(counts.3 > 0);
                });
                MESSAGE_COUNTS.with(|c| *c.borrow_mut() = (popup.hwnd, 0, 0, 0));
                popup
                    .set_ai_status(if step % 2 == 0 {
                        AiStatus::Waiting
                    } else {
                        AiStatus::Checking
                    })
                    .unwrap();
                pump(popup.hwnd);
                MESSAGE_COUNTS.with(|c| {
                    assert_eq!(c.borrow().1, 0);
                    assert!(c.borrow().2 <= 1);
                });
                MESSAGE_COUNTS.with(|c| *c.borrow_mut() = (popup.hwnd, 0, 0, 0));
                popup
                    .show_at(PopupRect::new(10 + step, 10, 11 + step, 30))
                    .unwrap();
                popup
                    .set_ai_status(if step % 2 == 0 {
                        AiStatus::Waiting
                    } else {
                        AiStatus::Checking
                    })
                    .unwrap();
                pump(popup.hwnd);
                MESSAGE_COUNTS.with(|c| {
                    assert_eq!(c.borrow().1, 0);
                    assert_eq!(
                        c.borrow().2,
                        0,
                        "unchanged geometry should not be submitted"
                    );
                });
            }
            popup.hide();
            pump(popup.hwnd);
            MESSAGE_COUNTS
                .with(|c| assert_eq!(c.borrow().1, 1, "real hide must remain observable"));
            MESSAGE_COUNTS.with(|c| *c.borrow_mut() = (popup.hwnd, 0, 0, 0));
            snapshot.revision += 1;
            popup.update_snapshot(&snapshot).unwrap();
            popup.set_ai_status(AiStatus::Waiting).unwrap();
            pump(popup.hwnd);
            assert!(!popup.is_visible());
            MESSAGE_COUNTS.with(|c| {
                assert_eq!(c.borrow().1, 0);
                assert_eq!(c.borrow().2, 0);
            });
            MESSAGE_COUNTS.with(|c| *c.borrow_mut() = (HWND::default(), 0, 0, 0));
            popup.destroy().unwrap();
        }

        #[test]
        fn native_visible_refresh_and_highlight_remain_in_work_area() {
            use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
            let mut popup = CandidatePopupWindow::new().unwrap();
            let mut snapshot = long_snapshot();
            snapshot.candidates = (0..9).map(|_| snapshot.candidates[0].clone()).collect();
            popup.update(&snapshot).unwrap();
            popup.set_ai_status(AiStatus::Waiting).unwrap();
            popup.show_at(PopupRect::new(10, 10, 11, 30)).unwrap();
            snapshot.highlighted = Some(8);
            snapshot.revision += 1;
            popup.update(&snapshot).unwrap();
            let mut rect = RECT::default();
            unsafe {
                GetWindowRect(popup.hwnd, &mut rect).unwrap();
            }
            let work = monitor_work_area(rect_from_windows(rect)).unwrap();
            assert!(rect.left >= work.left && rect.top >= work.top);
            assert!(rect.right <= work.right && rect.bottom <= work.bottom);
            let state = popup.state.as_ref().unwrap().borrow();
            let layout = PopupLayout::new(
                &snapshot,
                state.dpi,
                PopupSize::new(rect.right - rect.left, rect.bottom - rect.top),
                state.ai_status,
            );
            drop(state);
            let row = layout.candidates.candidates[8];
            assert!(row.height() > 0);
            paint_native_test_frame(popup.hwnd);
            assert_eq!(
                popup_action_at(
                    popup.hwnd,
                    PopupPoint {
                        x: row.left,
                        y: row.top
                    }
                ),
                Some(CandidatePopupAction::Select {
                    revision: snapshot.revision,
                    page: 3,
                    index: 8
                })
            );
            popup.destroy().unwrap();
        }

        #[test]
        fn offscreen_production_body_paints_last_glyph_without_status_pollution() {
            use windows::Win32::Graphics::Gdi::GdiFlush;
            struct Surface {
                dc: HDC,
                bitmap: HBITMAP,
                previous: HGDIOBJ,
            }
            impl Drop for Surface {
                fn drop(&mut self) {
                    unsafe {
                        let _ = SelectObject(self.dc, self.previous);
                        let _ = DeleteObject(self.bitmap);
                        let _ = DeleteDC(self.dc);
                    }
                }
            }
            let mut snapshot = long_snapshot();
            snapshot.highlighted = None;
            for dpi in [96, 144, 192, 480] {
                let layout = PopupLayout::new(
                    &snapshot,
                    dpi,
                    PopupSize::new(scaled(560, dpi), scaled(1600, dpi)),
                    Some(AiStatus::Waiting),
                );
                assert_eq!(layout.max_scroll, 0);
                let width = layout.size.width;
                let height = layout.size.height;
                let dc = unsafe { CreateCompatibleDC(None) };
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
                let bitmap =
                    unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                        .unwrap();
                let previous = unsafe { SelectObject(dc, bitmap) };
                let _surface = Surface {
                    dc,
                    bitmap,
                    previous,
                };
                let _font = PaintFont::new(dc, dpi).unwrap();
                unsafe {
                    SetBkMode(dc, TRANSPARENT);
                }
                fill_rect(
                    dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: width,
                        bottom: height,
                    },
                    COLORREF(0xffffff),
                );
                paint_candidate_body(dc, &snapshot, dpi, super::super::LIGHT_PALETTE, &layout);
                assert!(unsafe { GdiFlush() }.as_bool());
                // The final Chinese scalar is not merely retained in a string: it has ink
                // in its measured final-glyph box on the production offscreen drawing.
                let line = layout.lines[0].last().unwrap();
                let (tail, _) = line.char_indices().last().unwrap();
                let x = layout.full_rows[0].left + measured_width(dc, &line[..tail]).unwrap();
                let right = layout.full_rows[0].left + measured_width(dc, line).unwrap();
                let bottom = layout.full_rows[0].bottom;
                let mut ink = 0;
                for y in bottom - layout.line_height..bottom {
                    for x in x..right {
                        if unsafe { GetPixel(dc, x, y) }.0 != 0xffffff {
                            ink += 1;
                        }
                    }
                }
                assert!(ink > 0, "last glyph has visible pixels at {dpi} DPI");
                let status = layout.status.unwrap();
                for y in status.top..status.bottom {
                    for x in 0..width {
                        assert_eq!(unsafe { GetPixel(dc, x, y) }.0, 0xffffff);
                    }
                }
                let save_bitmap = |suffix: &str| {
                    let Some(directory) = std::env::var_os("FLOWER_POPUP_TEST_ARTIFACTS") else {
                        return;
                    };
                    let path = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&path).unwrap();
                    let pixels = unsafe {
                        std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize)
                    };
                    let mut bmp = Vec::new();
                    bmp.extend_from_slice(b"BM");
                    bmp.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
                    bmp.extend_from_slice(&[0; 4]);
                    bmp.extend_from_slice(&54u32.to_le_bytes());
                    bmp.extend_from_slice(&40u32.to_le_bytes());
                    bmp.extend_from_slice(&width.to_le_bytes());
                    bmp.extend_from_slice(&(-height).to_le_bytes());
                    bmp.extend_from_slice(&1u16.to_le_bytes());
                    bmp.extend_from_slice(&32u16.to_le_bytes());
                    bmp.extend_from_slice(&[0; 24]);
                    bmp.extend_from_slice(pixels);
                    std::fs::write(path.join(format!("long-candidates-{dpi}{suffix}.bmp")), bmp)
                        .unwrap();
                };
                save_bitmap("");
                let mut selected = snapshot.clone();
                selected.highlighted = Some(0);
                fill_rect(
                    dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: width,
                        bottom: height,
                    },
                    COLORREF(0xffffff),
                );
                paint_candidate_body(dc, &selected, dpi, super::super::LIGHT_PALETTE, &layout);
                assert!(unsafe { GdiFlush() }.as_bool());
                let max_width = layout.lines[0]
                    .iter()
                    .map(|s| measured_width(dc, s).unwrap())
                    .max()
                    .unwrap();
                let x = layout.full_rows[0].left + max_width + scaled(3, dpi);
                for line in 0..layout.lines[0].len() {
                    let y = layout.full_rows[0].top
                        + line as i32 * layout.line_height
                        + layout.line_height / 2;
                    assert_eq!(
                        unsafe { GetPixel(dc, x, y) },
                        COLORREF(super::super::LIGHT_PALETTE.highlight)
                    );
                }
                save_bitmap("-highlight");
            }
        }

        #[test]
        fn ai_status_layout_preserves_candidates_and_excludes_status_hits() {
            let snapshot = CandidatePopupSnapshot {
                revision: 1,
                preedit: String::new(),
                candidates: vec![crate::engine::Candidate::new("你好", "nihao")],
                highlighted: None,
                page: 0,
                page_count: 0,
            };
            for dpi in [96, 144, 192, 480] {
                let bounds = PopupSize::new(4000, 4000);
                let plain = PopupLayout::new(&snapshot, dpi, bounds, None);
                let original = CandidatePopupLayout::new(&snapshot, dpi, bounds);
                assert_eq!(plain.candidates.candidates, original.candidates);
                assert_eq!(plain.candidates.size, original.size);
                assert_eq!(plain.size.width, original.size.width + scaled(32, dpi));
                assert_eq!(plain.size.height, original.size.height);
                assert_eq!(plain.candidates.footer, None);
                assert!(plain.candidates.settings_button.is_some());
                assert!(plain.status.is_none());
                let status = PopupLayout::new(&snapshot, dpi, bounds, Some(AiStatus::Waiting));
                assert_eq!(status.candidates, plain.candidates);
                assert_eq!(status.size.width, plain.size.width);
                assert_eq!(status.size.height, plain.size.height + scaled(22, dpi));
                let row = status.status.unwrap();
                assert_eq!(row.top, plain.size.height);
                assert_eq!(
                    status
                        .candidates
                        .hit_test(&snapshot, PopupPoint { x: 8, y: row.top }),
                    None
                );
                for width in [1, 7, 32, 560] {
                    for height in [1, 7, 22, 45, 100] {
                        let layout = PopupLayout::new(
                            &snapshot,
                            dpi,
                            PopupSize::new(width, height),
                            Some(AiStatus::Checking),
                        );
                        assert!(layout.size.width <= width && layout.size.height <= height);
                        let row = layout.status.unwrap();
                        assert!(row.top >= 0 && row.bottom <= height && row.right <= width);
                        assert!(layout
                            .candidates
                            .candidates
                            .iter()
                            .all(|cell| cell.bottom <= row.top));
                    }
                }
            }
        }

        #[test]
        fn candidate_gdi_selected_font_fits_actual_display_including_ellipsis() {
            // A memory DC only: no desktop input, window activation or user document.
            let hdc = unsafe { CreateCompatibleDC(None) };
            assert!(!hdc.is_invalid());
            for dpi in [96, 144, 192, 480] {
                let font = PaintFont::new(hdc, dpi).expect("selected production font");
                let text = "1. 很长的候选文字😀";
                let original = text.to_owned();
                let full = measured_width(hdc, text).unwrap();
                assert!(full > 0);
                let roomy = full + scaled(30, dpi);
                assert_eq!(
                    fitted_candidate_text(hdc, text, roomy),
                    (text.to_owned(), full)
                );
                for available in [0, 1, full / 2, full - 1, full, roomy] {
                    let (display, width) = fitted_candidate_text(hdc, text, available);
                    assert!(width >= 0 && width <= available);
                    assert_eq!(measured_width(hdc, &display), Some(width));
                    let cell = PopupRect::new(17, 8, 17 + available, 38);
                    let highlight = candidate_highlight_rect(cell, width, dpi);
                    assert_eq!(highlight.left, cell.left);
                    assert_eq!(highlight.top, cell.top);
                    assert_eq!(highlight.bottom, cell.bottom);
                    let expected = if width > 0 {
                        width.saturating_add(scaled(6, dpi)).min(available)
                    } else {
                        0
                    };
                    assert_eq!(highlight.right, cell.left + expected);
                    assert!(highlight.right <= cell.right);
                    if available == roomy {
                        assert!(highlight.right < cell.right);
                    }
                    if !display.is_empty() && available < full {
                        assert!(display.ends_with("..."));
                        assert!(text.starts_with(display.trim_end_matches('.')));
                    }
                }
                assert_eq!(text, original);
                drop(font);
            }
            assert!(unsafe { DeleteDC(hdc) }.as_bool());
        }

        #[test]
        fn candidate_measurement_failure_is_conservative() {
            assert_eq!(
                fit_candidate_text("1. 是", 50, |_| None),
                (String::new(), 0)
            );
            assert_eq!(
                fit_candidate_text("1. 是", 50, |_| Some(-1)),
                (String::new(), 0)
            );
            assert_eq!(
                fit_candidate_text("1. 是", 50, |text| (text == "1. 是").then_some(100)),
                (String::new(), 0)
            );
            let cell = PopupRect::new(10, 8, 20, 38);
            assert_eq!(candidate_highlight_rect(cell, i32::MAX, 96).right, 20);
            assert_eq!(candidate_highlight_rect(cell, -1, 96).right, 10);
        }

        #[test]
        fn candidate_highlight_padding_scales_and_stays_inside_its_cell() {
            for dpi in [0, 96, 144, 192, 480, u32::MAX] {
                let cell = PopupRect::new(10, 8, 210, 38);
                let rect = candidate_highlight_rect(cell, 30, dpi);
                assert_eq!(rect.right, (40 + scaled(6, dpi)).min(210));
                assert_eq!(rect.left, cell.left);
                assert_eq!(candidate_highlight_rect(cell, 0, dpi).right, cell.left);
                let narrow = PopupRect::new(10, 8, 11, 38);
                assert_eq!(
                    candidate_highlight_rect(narrow, 30, dpi).right,
                    narrow.right
                );
            }
        }

        #[test]
        fn candidate_highlight_gdi_has_round_corners_and_does_not_paint_next_cell() {
            struct Surface {
                dc: HDC,
                bitmap: HBITMAP,
                previous: HGDIOBJ,
            }
            impl Drop for Surface {
                fn drop(&mut self) {
                    unsafe {
                        let _ = SelectObject(self.dc, self.previous);
                        let _ = DeleteObject(self.bitmap);
                        let _ = DeleteDC(self.dc);
                    }
                }
            }
            let dc = unsafe { CreateCompatibleDC(None) };
            assert!(!dc.is_invalid());
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 100,
                    biHeight: -60,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) }
                .unwrap_or_else(|error| {
                    let _ = unsafe { DeleteDC(dc) };
                    panic!("create offscreen bitmap: {error}");
                });
            let previous = unsafe { SelectObject(dc, bitmap) };
            let surface = Surface {
                dc,
                bitmap,
                previous,
            };
            for background in [
                COLORREF(0x00ff_ffff),
                COLORREF(0x0020_2020),
                COLORREF(0x0038_7124),
            ] {
                let highlight = COLORREF(0x00d7_7800);
                for dpi in [96, 144, 192, 480] {
                    fill_rect(
                        surface.dc,
                        &RECT {
                            left: 0,
                            top: 0,
                            right: 100,
                            bottom: 60,
                        },
                        background,
                    );
                    let cell = PopupRect::new(10, 10, 90, 40);
                    let rect = candidate_highlight_rect(cell, 30, dpi);
                    assert!(fill_candidate_highlight(
                        surface.dc, &rect, dpi, highlight, background,
                    ));
                    assert_eq!(unsafe { GetPixel(surface.dc, 10, 10) }, background);
                    assert_eq!(unsafe { GetPixel(surface.dc, 25, 25) }, highlight);
                    assert_eq!(unsafe { GetPixel(surface.dc, rect.right, 25) }, background);
                    assert_eq!(unsafe { GetPixel(surface.dc, 90, 25) }, background);
                    let mut mixed = std::collections::BTreeSet::new();
                    for y in 0..60 {
                        for x in 0..100 {
                            let pixel = unsafe { GetPixel(surface.dc, x, y) };
                            if x < rect.left || x >= rect.right || y < rect.top || y >= rect.bottom
                            {
                                assert_eq!(pixel, background, "outside {x},{y} at {dpi} DPI");
                            } else {
                                assert_eq!(pixel, unsafe {
                                    GetPixel(surface.dc, rect.right - 1 - (x - rect.left), y)
                                });
                                assert_eq!(pixel, unsafe {
                                    GetPixel(surface.dc, x, rect.bottom - 1 - (y - rect.top))
                                });
                                if pixel != background && pixel != highlight {
                                    mixed.insert(pixel.0);
                                    for shift in [0, 8, 16] {
                                        let fg = (highlight.0 >> shift) & 255;
                                        let bg = (background.0 >> shift) & 255;
                                        let actual = (pixel.0 >> shift) & 255;
                                        assert!((fg.min(bg)..=fg.max(bg)).contains(&actual));
                                    }
                                }
                            }
                        }
                    }
                    assert!(mixed.len() > 1, "multiple AA coverage levels at {dpi} DPI");
                    assert!(!fill_candidate_highlight(
                        surface.dc,
                        &RECT::default(),
                        dpi,
                        highlight,
                        background,
                    ));
                    assert!(fill_candidate_highlight(
                        surface.dc,
                        &RECT {
                            left: 1,
                            top: 1,
                            right: 2,
                            bottom: 2
                        },
                        dpi,
                        highlight,
                        background,
                    ));
                    for (width, height) in [(1, 1), (1, 3), (2, 2), (3, 5), (5, 2)] {
                        let tiny = RECT {
                            left: 2,
                            top: 2,
                            right: 2 + width,
                            bottom: 2 + height,
                        };
                        assert!(fill_candidate_highlight(
                            surface.dc, &tiny, dpi, highlight, background
                        ));
                        for y in 0..height {
                            for x in 0..width {
                                let pixel = unsafe { GetPixel(surface.dc, 2 + x, 2 + y) };
                                assert_eq!(pixel, unsafe {
                                    GetPixel(surface.dc, 2 + width - 1 - x, 2 + y)
                                });
                                assert_eq!(pixel, unsafe {
                                    GetPixel(surface.dc, 2 + x, 2 + height - 1 - y)
                                });
                            }
                        }
                    }
                    for invalid in [
                        RECT {
                            left: 2,
                            top: 2,
                            right: 1,
                            bottom: 3,
                        },
                        RECT {
                            left: 2,
                            top: 2,
                            right: 3,
                            bottom: 1,
                        },
                        RECT {
                            left: i32::MIN,
                            top: 0,
                            right: i32::MAX,
                            bottom: 1,
                        },
                        RECT {
                            left: 0,
                            top: 0,
                            right: 100_000,
                            bottom: 100_000,
                        },
                    ] {
                        assert!(!fill_candidate_highlight(
                            surface.dc, &invalid, dpi, highlight, background
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows_impl::{CandidatePopupWindow, PopupError};

#[cfg(test)]
mod tests {
    use super::{popup_palette, PopupPalette, DARK_PALETTE, LIGHT_PALETTE};
    use crate::settings::Theme;

    #[test]
    fn target_budget_uses_target_dpi_and_clamps_or_falls_back() {
        use super::target_candidate_width_budget as budget;
        // Same physical work area, two possible destination screens.
        assert_eq!(budget(800, 96), 512);
        assert_eq!(budget(800, 192), 352);
        // There is deliberately no old-window DPI input: going in either
        // direction must produce the destination's budget.
        for (target_dpi, expected) in [(192, 352), (96, 512), (192, 352)] {
            assert_eq!(budget(800, target_dpi), expected);
        }
        assert_eq!(budget(1120, 192), 512);
        assert_eq!(budget(1, 192), 1);
        assert_eq!(budget(0, 192), 512);
        assert_eq!(budget(800, 0), 512);
        assert_eq!(budget(i32::MAX, 96), 512);
    }

    fn system_palette() -> PopupPalette {
        PopupPalette {
            background: 1,
            text: 2,
            highlight: 3,
            highlight_text: 4,
        }
    }

    #[test]
    fn fixed_themes_select_their_palette() {
        assert_eq!(popup_palette(Theme::Light, system_palette()), LIGHT_PALETTE);
        assert_eq!(popup_palette(Theme::Dark, system_palette()), DARK_PALETTE);
    }

    #[test]
    fn system_theme_uses_supplied_system_colors() {
        assert_eq!(
            popup_palette(Theme::System, system_palette()),
            system_palette()
        );
    }
}
