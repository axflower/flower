#![cfg_attr(not(target_os = "windows"), forbid(unsafe_code))]
//! Windows Text Services Framework boundary for Flower.
//!
//! The TSF/COM module is compiled only for Windows. It exports the in-process
//! server entry points, a class factory for the Flower text service, and a
//! text service that maintains per-context engine sessions and applies
//! composition updates through synchronous TSF edit sessions.

pub const TEXT_SERVICE_DESCRIPTION: &str = crate::registration::FLOWER_TEXT_SERVICE_DESCRIPTION;

pub fn is_supported_platform() -> bool {
    cfg!(target_os = "windows")
}

#[cfg(target_os = "windows")]
pub const FLOWER_TEXT_SERVICE_CLSID: windows::core::GUID =
    windows::core::GUID::from_u128(crate::registration::FLOWER_TEXT_SERVICE_CLSID_U128);

#[cfg(target_os = "windows")]
#[allow(non_snake_case)]
mod windows_exports {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::ptr;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread::ThreadId;
    use std::time::{Duration, Instant};

    use crate::ai::{AiInput, Request, Response};

    use crate::ai::{AiStatus, ScopeBlockReason};
    use crate::engine::{
        map_windows_virtual_key, CompositionUpdate, EngineRouter, KeyEvent, Modifiers,
    };
    use crate::engine::{promote_order, Lexicon, RankingOrder};
    use crate::platform::candidate::{
        deferred_action_is_valid, edit_publish_decision, AcceptedCandidateAction,
        CandidatePopupAction, DeferredActionState, DeferredActionTicket, EditPublishDecision,
        PopupRect,
    };
    use crate::platform::composition::{CompositionController, CompositionHost};
    use crate::platform::diagnostics::{record, record_result, Stage};
    use crate::platform::{CandidatePopupModel, CandidatePopupWindow, PopupError};
    use crate::settings::{Settings, SettingsStore};
    use crate::user_dictionary::{DictionaryStore, LearningWorker};
    use windows::core::{implement, AsImpl, Error, IUnknown, Interface, Result, GUID, HRESULT};
    use windows::Win32::Foundation::{
        BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_INVALIDARG, E_POINTER,
        E_UNEXPECTED, FALSE, LPARAM, POINT, RECT, S_FALSE, S_OK, TRUE, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, GetKeyboardState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::TextServices::{
        ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext,
        ITfContextComposition, ITfContextView, ITfEditSession, ITfEditSession_Impl,
        ITfInsertAtSelection, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfRange,
        ITfTextInputProcessor, ITfTextInputProcessor_Impl, ITfThreadMgr, TF_AE_NONE, TF_ANCHOR_END,
        TF_ANCHOR_START, TF_ES_ASYNC, TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC, TF_IAS_QUERYONLY,
        TF_SELECTION, TF_SELECTIONSTYLE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetGUIThreadInfo, GetWindowThreadProcessId, IsChild, GUITHREADINFO,
    };
    use windows_core::{ComObjectInterface, IUnknownImpl};

    use super::FLOWER_TEXT_SERVICE_CLSID;

    static OBJECT_COUNT: AtomicU32 = AtomicU32::new(0);
    static SERVER_LOCK_COUNT: AtomicU32 = AtomicU32::new(0);

    struct ObjectRegistration;

    impl ObjectRegistration {
        fn new() -> Self {
            OBJECT_COUNT.fetch_add(1, Ordering::SeqCst);
            Self
        }
    }

    impl Drop for ObjectRegistration {
        fn drop(&mut self) {
            let _ = OBJECT_COUNT.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn release_server_lock() -> Result<()> {
        let mut current = SERVER_LOCK_COUNT.load(Ordering::SeqCst);

        while current > 0 {
            match SERVER_LOCK_COUNT.compare_exchange_weak(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }

        Err(E_UNEXPECTED.into())
    }

    fn can_unload() -> bool {
        OBJECT_COUNT.load(Ordering::SeqCst) == 0 && SERVER_LOCK_COUNT.load(Ordering::SeqCst) == 0
    }

    type ActivationToken = u64;

    static NEXT_ACTIVATION_TOKEN: AtomicU64 = AtomicU64::new(1);

    fn next_activation_token() -> ActivationToken {
        NEXT_ACTIVATION_TOKEN.fetch_add(1, Ordering::SeqCst)
    }

    /// A popup window owned by the thread that created it.
    ///
    /// The registration guard keeps `DllCanUnloadNow` false for as long as the
    /// window state exists, including while it is leased out of thread-local
    /// storage for a Win32 call.
    struct ThreadPopup {
        window: CandidatePopupWindow,
        _registration: ObjectRegistration,
    }

    thread_local! {
        static THREAD_POPUPS: RefCell<HashMap<ActivationToken, ThreadPopup>> =
            RefCell::new(HashMap::new());
    }

    fn register_thread_popup(token: ActivationToken, window: CandidatePopupWindow) -> bool {
        let popup = ThreadPopup {
            window,
            _registration: ObjectRegistration::new(),
        };
        match THREAD_POPUPS.try_with(|popups| popups.borrow_mut().insert(token, popup)) {
            Ok(displaced) => {
                // Drop outside the RefCell borrow so a displaced window never
                // runs Win32 destruction while thread-local storage is
                // borrowed.
                drop(displaced);
                true
            }
            Err(_) => false,
        }
    }

    /// Leases a window from the owning thread. The RefCell borrow ends before
    /// the caller performs any Win32 calls, and a non-owner thread is never
    /// given access to an `HWND` created elsewhere.
    fn take_thread_popup(token: ActivationToken, owner_thread: ThreadId) -> Option<ThreadPopup> {
        if std::thread::current().id() != owner_thread {
            return None;
        }

        THREAD_POPUPS
            .try_with(|popups| popups.borrow_mut().remove(&token))
            .ok()
            .flatten()
    }

    fn return_thread_popup(token: ActivationToken, owner_thread: ThreadId, popup: ThreadPopup) {
        if std::thread::current().id() != owner_thread {
            drop(popup);
            return;
        }

        let displaced = THREAD_POPUPS.try_with(|popups| popups.borrow_mut().insert(token, popup));
        drop(displaced);
    }

    fn close_thread_popup(token: ActivationToken, owner_thread: ThreadId) {
        let Some(mut popup) = take_thread_popup(token, owner_thread) else {
            return;
        };

        popup.window.hide();
        let _ = popup.window.destroy();
        drop(popup);
    }

    #[implement(IClassFactory)]
    struct ClassFactory {
        _registration: ObjectRegistration,
    }

    impl ClassFactory {
        fn new() -> Self {
            Self {
                _registration: ObjectRegistration::new(),
            }
        }
    }

    impl IClassFactory_Impl for ClassFactory_Impl {
        fn CreateInstance(
            &self,
            outer: Option<&IUnknown>,
            interface_id: *const GUID,
            object: *mut *mut c_void,
        ) -> Result<()> {
            if object.is_null() || interface_id.is_null() {
                return Err(E_POINTER.into());
            }

            // SAFETY: The null check above establishes that the caller supplied
            // the required output pointer.
            unsafe {
                object.write(ptr::null_mut());
            }

            if outer.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }

            let service: IUnknown = TextService::new().into();

            // SAFETY: `interface_id` and `object` are non-null output pointers,
            // and `service` is a live COM identity. The query adds a reference
            // for the caller; dropping `service` releases only our local one.
            unsafe {
                service.query(interface_id, object).ok()?;
            }

            Ok(())
        }

        fn LockServer(&self, lock: BOOL) -> Result<()> {
            if lock.as_bool() {
                SERVER_LOCK_COUNT.fetch_add(1, Ordering::SeqCst);
                Ok(())
            } else {
                release_server_lock()
            }
        }
    }

    type ContextKey = usize;
    type TerminationUi = Rc<RefCell<Option<(std::rc::Weak<UiState>, std::rc::Weak<UiData>)>>>;

    #[derive(Clone)]
    struct ActiveComposition {
        handle: ITfComposition,
        terminated: Rc<Cell<bool>>,
        ui: TerminationUi,
    }

    impl std::ops::Deref for ActiveComposition {
        type Target = ITfComposition;

        fn deref(&self) -> &Self::Target {
            &self.handle
        }
    }

    #[implement(ITfCompositionSink)]
    struct CompositionSink {
        terminated: Rc<Cell<bool>>,
        ui: TerminationUi,
        owner_thread: ThreadId,
        _registration: ObjectRegistration,
    }

    impl ITfCompositionSink_Impl for CompositionSink_Impl {
        fn OnCompositionTerminated(
            &self,
            _edit_cookie: u32,
            _composition: Option<&ITfComposition>,
        ) -> Result<()> {
            if std::thread::current().id() != self.owner_thread {
                return Err(E_UNEXPECTED.into());
            }
            self.terminated.set(true);
            let binding = self.ui.borrow().clone();
            if let Some((ui, data)) = binding {
                if let (Some(ui), Some(data)) = (ui.upgrade(), data.upgrade()) {
                    if data.alive.get() {
                        ui.end();
                    }
                }
            }
            Ok(())
        }
    }

    #[derive(Clone)]
    struct ContextEntry {
        context: ITfContext,
        router: EngineRouter,
        composition: Option<ActiveComposition>,
        controller: CompositionController,
        popup_model: CandidatePopupModel,
    }

    impl ContextEntry {
        fn composition_terminated(&self) -> bool {
            self.composition
                .as_ref()
                .is_some_and(|composition| composition.terminated.get())
        }

        fn new(context: &ITfContext, settings: &Settings, lexicon: &Lexicon) -> Option<Self> {
            let mut router =
                EngineRouter::from_lexicon(lexicon.clone(), settings.candidate_page_size).ok()?;
            router.set_candidate_width(Some(crate::engine::paging::DEFAULT_CANDIDATE_WIDTH));
            Some(Self {
                context: context.clone(),
                router,
                composition: None,
                controller: CompositionController::new(),
                popup_model: CandidatePopupModel::new(),
            })
        }
    }

    struct Activation {
        ui: Rc<UiState>,
        _activation_flags: u32,
        thread_manager: ITfThreadMgr,
        keystroke_manager: ITfKeystrokeMgr,
        client_id: u32,
        contexts: HashMap<ContextKey, ContextEntry>,
        /// Unique identity for this activation and its thread-local window.
        token: ActivationToken,
        /// Thread that created the service and owns the popup window.
        owner_thread: ThreadId,
        /// Incremented on every focus transition so stale key work cannot
        /// republish or show a popup after focus changed.
        focus_epoch: u64,
        /// 当前 TSF 服务是否拥有前台焦点。
        foreground: bool,
        shown_context: Option<ContextKey>,
        /// True while a key callback owns the synchronous edit session.
        /// Recursive key callbacks pass through instead of starting another.
        processing: bool,
        english: bool,
        shift: ShiftGesture,
        context_generation: Rc<Cell<u64>>,
        _thread_events: Option<ThreadEvents>,
        /// Immutable settings captured when this activation started.
        settings: Settings,
        lexicon: Lexicon,
        rankings: Arc<Vec<RankingOrder>>,
        adaptive_ranking_enabled: bool,
        learning: Option<LearningWorker>,
        _learning_registration: Option<ObjectRegistration>,
        local_data_error: bool,
        ai: Option<AiScheduler>,
        ai_unavailable: AiStatus,
    }

    type SharedActivation = Rc<RefCell<Option<Activation>>>;

    include!("ui_element.rs");

    trait AiTransport {
        fn submit(&self, request: Request) -> bool;
        fn try_result(&self) -> Option<Response>;
        fn shutdown(&mut self);
    }

    impl AiTransport for crate::ai::ipc::AiClient {
        fn submit(&self, request: Request) -> bool {
            self.submit(request)
        }
        fn try_result(&self) -> Option<Response> {
            self.try_result()
        }
        fn shutdown(&mut self) {
            self.shutdown();
        }
    }

    struct AiScheduler {
        transport: Box<dyn AiTransport>,
        // Drop calls shutdown/join before this independent DLL count is released.
        _registration: ObjectRegistration,
        last_input: Instant,
        next_id: u64,
        attempted: bool,
        pending: Option<AiPending>,
        status: AiStatus,
    }

    impl Drop for AiScheduler {
        fn drop(&mut self) {
            self.transport.shutdown();
        }
    }

    impl AiScheduler {
        fn new(transport: Box<dyn AiTransport>) -> Self {
            Self {
                transport,
                _registration: ObjectRegistration::new(),
                last_input: Instant::now(),
                next_id: 0,
                attempted: true,
                pending: None,
                status: AiStatus::Waiting,
            }
        }

        fn input(&mut self) {
            self.last_input = Instant::now();
            self.pending = None;
            self.attempted = false;
            self.status = AiStatus::Waiting;
        }

        fn invalidate(&mut self) {
            self.pending = None;
            self.attempted = true;
            self.status = AiStatus::Waiting;
        }
    }

    fn ai_status(active: &Activation) -> AiStatus {
        if !active.settings.ai_correction_enabled {
            AiStatus::Disabled
        } else {
            active
                .ai
                .as_ref()
                .map_or(active.ai_unavailable, |ai| ai.status)
        }
    }

    fn refresh_ai_status(shared: &SharedActivation, token: ActivationToken, owner: ThreadId) {
        let status = shared
            .try_borrow()
            .ok()
            .and_then(|state| state.as_ref().filter(|a| a.token == token).map(ai_status));
        if let Some(status) = status {
            if let Some(mut popup) = take_thread_popup(token, owner) {
                if !ui_allows(shared, token) {
                    popup.window.hide();
                }
                let _ = popup.window.set_ai_status(status);
                return_thread_popup(token, owner, popup);
            }
        }
    }

    fn set_ai_status(shared: &SharedActivation, ticket: &AiTicket, status: AiStatus) {
        if let Ok(mut state) = shared.try_borrow_mut() {
            if let Some(ai) = state
                .as_mut()
                .filter(|a| a.token == ticket.token)
                .and_then(|a| a.ai.as_mut())
            {
                if ai
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.ticket.id == ticket.id)
                {
                    ai.status = status;
                }
            }
        }
    }

    #[derive(Clone)]
    struct AiTicket {
        id: u64,
        token: ActivationToken,
        owner: ThreadId,
        epoch: u64,
        generation: u64,
        key: ContextKey,
        revision: u64,
    }

    struct AiPending {
        ticket: AiTicket,
        queued: Instant,
        dispatched: Option<Instant>,
        input: Option<AiInput>,
        reading: bool,
    }

    /// Resolve our DLL by an address in this module, never the host executable.
    fn protected_ai_helper() -> std::result::Result<std::path::PathBuf, AiStatus> {
        use std::os::windows::ffi::OsStringExt;
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        };
        let mut module = HMODULE::default();
        let mut path = vec![0u16; 32768];
        // SAFETY: The address belongs to this loaded DLL; no refcount change.
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                PCWSTR(protected_ai_helper as *const () as *const u16),
                &mut module,
            )
            .map_err(|_| AiStatus::ModulePathFailed)?;
            let len = GetModuleFileNameW(module, &mut path) as usize;
            if len == 0 || len >= path.len() {
                return Err(AiStatus::ModulePathFailed);
            }
            path.truncate(len);
        }
        let dll = std::path::PathBuf::from(std::ffi::OsString::from_wide(&path));
        ai_helper_at_module_path(&dll)
    }

    fn ai_helper_at_module_path(
        dll: &std::path::Path,
    ) -> std::result::Result<std::path::PathBuf, AiStatus> {
        let parent = dll.parent().ok_or(AiStatus::UnsupportedPath)?;
        let directory = crate::platform::install_path::normalize_runtime_directory(parent)
            .map_err(|_| AiStatus::UnsupportedPath)?;
        crate::platform::install_path::check_install_path(&directory)
            .map_err(|_| AiStatus::UnsafeDirectory)?;
        let helper = directory.join("flower-settings.exe");
        if !helper.is_file() {
            return Err(AiStatus::MissingHelper);
        }
        if !directory.join("ai").is_dir() {
            return Err(AiStatus::MissingAssets);
        }
        Ok(helper)
    }

    #[cfg(test)]
    #[test]
    fn native_ai_canonical_module_path_does_not_bypass_acl() {
        let module = std::env::current_exe().unwrap().canonicalize().unwrap();
        let normal =
            crate::platform::install_path::normalize_runtime_directory(module.parent().unwrap())
                .unwrap();
        assert_eq!(
            ai_helper_at_module_path(&module),
            ai_helper_at_module_path(&normal.join(module.file_name().unwrap())),
            "verbatim and ordinary spellings must reach the same security gate"
        );
        assert_eq!(
            ai_helper_at_module_path(&module),
            Err(AiStatus::UnsafeDirectory),
            "the user-writable test payload is not a protected installation"
        );
    }

    fn ai_ticket_valid(active: &Activation, ticket: &AiTicket) -> bool {
        active.token == ticket.token
            && active.owner_thread == ticket.owner
            && std::thread::current().id() == ticket.owner
            && active.foreground
            && !active.processing
            && !active.english
            && active.focus_epoch == ticket.epoch
            && active.context_generation.get() == ticket.generation
            && active.shown_context == Some(ticket.key)
            && active.contexts.get(&ticket.key).is_some_and(|entry| {
                entry.controller.is_active()
                    && entry.composition.is_some()
                    && !entry.composition_terminated()
                    && entry.popup_model.snapshot().is_some_and(|snapshot| {
                        snapshot.revision == ticket.revision
                            && snapshot.page == 0
                            && snapshot.highlighted == Some(0)
                    })
            })
            && active.ai.as_ref().is_some_and(|ai| {
                ai.pending
                    .as_ref()
                    .is_some_and(|p| p.ticket.id == ticket.id)
            })
    }

    fn clear_ai_pending(shared: &SharedActivation, ticket: &AiTicket) {
        if let Ok(mut state) = shared.try_borrow_mut() {
            if let Some(ai) = state
                .as_mut()
                .filter(|a| a.token == ticket.token)
                .and_then(|a| a.ai.as_mut())
            {
                if ai
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.ticket.id == ticket.id)
                {
                    ai.pending = None;
                    if ai.status == AiStatus::Checking {
                        ai.status = AiStatus::Waiting;
                    }
                }
            }
        }
    }

    #[implement(ITfEditSession)]
    struct AiReadSession {
        activation: std::rc::Weak<RefCell<Option<Activation>>>,
        ticket: AiTicket,
        context: ITfContext,
        manager: ITfThreadMgr,
        // None is the send gate; Some is the result publication gate.
        response: Option<Response>,
        _registration: ObjectRegistration,
    }

    impl ITfEditSession_Impl for AiReadSession_Impl {
        fn DoEditSession(&self, cookie: u32) -> Result<()> {
            let this = self.get_impl();
            if std::thread::current().id() != this.ticket.owner {
                return Err(E_UNEXPECTED.into());
            }
            let Some(shared) = this.activation.upgrade() else {
                return Ok(());
            };
            let valid = focused_context_key(&this.manager) == Some(this.ticket.key)
                && shared.try_borrow().ok().is_some_and(|state| {
                    state
                        .as_ref()
                        .is_some_and(|a| ai_ticket_valid(a, &this.ticket))
                });
            // No source text is constructed before the read-cookie privacy gate.
            if !valid {
                clear_ai_pending(&shared, &this.ticket);
                return Ok(());
            }
            let allow_undeclared = shared.try_borrow().ok().and_then(|state| {
                state
                    .as_ref()
                    .map(|a| a.settings.ai_undeclared_scope_compatibility)
            });
            let Some(allow_undeclared) = allow_undeclared else {
                clear_ai_pending(&shared, &this.ticket);
                return Ok(());
            };
            // Release the activation borrow before calling application COM code.
            if let Err(reason) = check_ai_scope(&this.context, cookie, allow_undeclared) {
                set_ai_status(&shared, &this.ticket, AiStatus::ScopeBlocked(reason));
                clear_ai_pending(&shared, &this.ticket);
                refresh_ai_status(&shared, this.ticket.token, this.ticket.owner);
                return Ok(());
            }
            if this.response.is_none() {
                let Ok(mut state) = shared.try_borrow_mut() else {
                    clear_ai_pending(&shared, &this.ticket);
                    return Ok(());
                };
                let Some(active) = state.as_mut().filter(|a| ai_ticket_valid(a, &this.ticket))
                else {
                    return Ok(());
                };
                let ai = active.ai.as_mut().unwrap();
                if ai.pending.as_ref().is_some_and(|pending| {
                    pending.queued.elapsed() > Duration::from_millis(crate::ai::RESULT_TIMEOUT_MS)
                }) {
                    ai.pending = None;
                    ai.status = AiStatus::EditFailed;
                    return Ok(());
                }
                let input = active
                    .contexts
                    .get(&this.ticket.key)
                    .and_then(|e| e.router.ai_input());
                let ai = active.ai.as_mut().unwrap();
                let Some(input) = input else {
                    ai.pending = None;
                    ai.status = AiStatus::Ineligible;
                    return Ok(());
                };
                if ai.transport.submit(Request {
                    raw: input.raw.clone(),
                    reading: input.reading.clone(),
                    id: this.ticket.id,
                    text: input.source.clone(),
                }) {
                    if let Some(pending) = ai.pending.as_mut() {
                        pending.dispatched = Some(Instant::now());
                        pending.input = Some(input);
                        pending.reading = false;
                        ai.status = AiStatus::Checking;
                    }
                } else {
                    ai.pending = None;
                    ai.status = AiStatus::Failed;
                }
                return Ok(());
            }
            let response = this.response.as_ref().unwrap();
            let composition = {
                let Ok(state) = shared.try_borrow() else {
                    return Ok(());
                };
                state
                    .as_ref()
                    .and_then(|a| a.contexts.get(&this.ticket.key))
                    .and_then(|e| e.composition.clone())
            };
            let caret = composition
                .as_ref()
                .and_then(|c| composition_caret(&this.context, c, cookie));
            let snapshot = {
                let Ok(mut state) = shared.try_borrow_mut() else {
                    return Ok(());
                };
                let Some(active) = state.as_mut().filter(|a| ai_ticket_valid(a, &this.ticket))
                else {
                    drop(state);
                    clear_ai_pending(&shared, &this.ticket);
                    return Ok(());
                };
                let pending = active.ai.as_mut().unwrap().pending.take().unwrap();
                active.ai.as_mut().unwrap().status = AiStatus::Waiting;
                if response.id != pending.ticket.id
                    || !pending.dispatched.is_some_and(|at| {
                        at.elapsed() <= Duration::from_millis(crate::ai::RESULT_TIMEOUT_MS)
                    })
                {
                    active.ai.as_mut().unwrap().status = AiStatus::Failed;
                    return Ok(());
                }
                let entry = active.contexts.get_mut(&this.ticket.key).unwrap();
                let Some(input) = pending.input else {
                    return Ok(());
                };
                if entry.router.ai_input().as_ref() != Some(&input) {
                    return Ok(());
                }
                let Some(corrected) = response.corrected.as_ref() else {
                    active.ai.as_mut().unwrap().status = AiStatus::NoSuggestion;
                    return Ok(());
                };
                let update = if response.reading.is_empty() {
                    entry.router.apply_ai_candidate(&input, corrected)
                } else {
                    entry
                        .router
                        .apply_ai_spelling_candidate(&input, corrected, &response.reading)
                };
                let Some(update) = update else {
                    return Ok(());
                };
                // AI never passes through CompositionController or a write cookie.
                entry.popup_model.apply_update(&update);
                let snapshot = entry.popup_model.snapshot().cloned();
                active.ai.as_mut().unwrap().status = AiStatus::Suggested;
                snapshot
            };
            if let Some(snapshot) = snapshot {
                if let Some(ui) = activation_ui(&shared, this.ticket.token) {
                    ui.publish(
                        &this.context,
                        Some(&snapshot),
                        caret.map(|r| PopupRect::new(r.left, r.top, r.right, r.bottom)),
                        this.ticket.token,
                        this.ticket.owner,
                    );
                }
                if let Some(mut popup) = take_thread_popup(this.ticket.token, this.ticket.owner) {
                    popup.window.cancel_pending_actions();
                    if popup.window.update(&snapshot).is_ok() {
                        if let Some(caret) = caret.filter(|_| ui_allows(&shared, this.ticket.token))
                        {
                            let _ = popup.window.show_at(PopupRect::new(
                                caret.left,
                                caret.top,
                                caret.right,
                                caret.bottom,
                            ));
                        } else {
                            popup.window.hide();
                        }
                    }
                    if let Some(ui) = activation_ui(&shared, this.ticket.token) {
                        ui.set_shown(popup.window.is_visible());
                    }
                    return_thread_popup(this.ticket.token, this.ticket.owner, popup);
                }
            }
            refresh_ai_status(&shared, this.ticket.token, this.ticket.owner);
            Ok(())
        }
    }

    /// Timer callback only queues read sessions. It never writes host text.
    fn ai_idle(shared: &SharedActivation, token: ActivationToken, owner: ThreadId) {
        if std::thread::current().id() != owner {
            return;
        }
        let manager = {
            let Ok(state) = shared.try_borrow() else {
                return;
            };
            let Some(active) = state.as_ref().filter(|a| a.token == token) else {
                return;
            };
            active.thread_manager.clone()
        };
        // Query COM outside the activation borrow, including when no result is
        // ready. A missing top context must release the retained source now.
        let focused = focused_context_key(&manager);
        let work = (|| {
            let mut state = shared.try_borrow_mut().ok()?;
            let active = state.as_mut().filter(|a| a.token == token)?;
            if focused.is_none() || focused != active.shown_context {
                active.ai.as_mut()?.invalidate();
                return None;
            }
            if active.processing {
                return None;
            }
            let invalid = active
                .ai
                .as_ref()
                .and_then(|ai| ai.pending.as_ref())
                .is_some_and(|pending| !ai_ticket_valid(active, &pending.ticket));
            if invalid {
                active.ai.as_mut()?.invalidate();
                return None;
            }
            let ai = active.ai.as_mut()?;
            if let Some(pending) = ai.pending.as_ref() {
                if pending.dispatched.is_none()
                    && pending.queued.elapsed()
                        > Duration::from_millis(crate::ai::RESULT_TIMEOUT_MS)
                {
                    ai.pending = None;
                    ai.status = AiStatus::EditFailed;
                    return None;
                }
                if pending.dispatched.is_some_and(|at| {
                    at.elapsed() > Duration::from_millis(crate::ai::RESULT_TIMEOUT_MS)
                }) {
                    ai.pending = None;
                    ai.status = AiStatus::Failed;
                    return None;
                }
                if pending.reading {
                    return None;
                }
                let response = ai.transport.try_result()?;
                let pending = ai.pending.as_mut()?;
                if response.id != pending.ticket.id {
                    ai.pending = None;
                    ai.status = AiStatus::Failed;
                    return None;
                }
                pending.reading = true;
                let ticket = pending.ticket.clone();
                let context = active.contexts.get(&ticket.key)?.context.clone();
                return Some((
                    ticket,
                    context,
                    active.thread_manager.clone(),
                    active.client_id,
                    Some(response),
                ));
            }
            // Drain late/duplicate responses before accepting a new request.
            let _ = ai.transport.try_result();
            if ai.attempted
                || ai.last_input.elapsed() < Duration::from_millis(crate::ai::DEBOUNCE_MS)
                || !active.foreground
                || active.english
            {
                return None;
            }
            let key = active.shown_context?;
            let entry = active.contexts.get(&key)?;
            if !entry.controller.is_active()
                || entry.composition.is_none()
                || entry.composition_terminated()
            {
                return None;
            }
            let snapshot = entry.popup_model.snapshot()?;
            if snapshot.page != 0 || snapshot.highlighted != Some(0) {
                return None;
            }
            let revision = snapshot.revision;
            ai.next_id = ai.next_id.checked_add(1)?;
            ai.attempted = true;
            ai.status = AiStatus::Checking;
            let ticket = AiTicket {
                id: ai.next_id,
                token,
                owner,
                epoch: active.focus_epoch,
                generation: active.context_generation.get(),
                key,
                revision,
            };
            ai.pending = Some(AiPending {
                ticket: ticket.clone(),
                queued: Instant::now(),
                dispatched: None,
                input: None,
                reading: true,
            });
            Some((
                ticket,
                entry.context.clone(),
                active.thread_manager.clone(),
                active.client_id,
                None,
            ))
        })();
        refresh_ai_status(shared, token, owner);
        let Some((ticket, context, manager, client, response)) = work else {
            return;
        };
        if focused_context_key(&manager) != Some(ticket.key) {
            clear_ai_pending(shared, &ticket);
            return;
        }
        let session: ITfEditSession = AiReadSession {
            activation: Rc::downgrade(shared),
            ticket: ticket.clone(),
            context: context.clone(),
            manager,
            response,
            _registration: ObjectRegistration::new(),
        }
        .into();
        // SAFETY: TSF owns the read cookie and schedules this on the owning STA.
        let status =
            unsafe { context.RequestEditSession(client, &session, TF_ES_ASYNC | TF_ES_READ) };
        if !matches!(status, Ok(hr) if hr.is_ok()) {
            set_ai_status(shared, &ticket, AiStatus::EditFailed);
            clear_ai_pending(shared, &ticket);
            refresh_ai_status(shared, token, owner);
        }
    }

    /// Tracks only a standalone physical Shift gesture; test callbacks never mutate it.
    #[derive(Default)]
    struct ShiftGesture {
        pressed: Option<(u32, ContextKey, u64)>,
        context_generation: u64,
    }

    fn shift_identity(key: usize, lparam: LPARAM) -> Option<u32> {
        match key {
            0xA0 => Some(0x2A),
            0xA1 => Some(0x36),
            0x10 => Some(((lparam.0 as usize >> 16) & 0xFF) as u32),
            _ => None,
        }
    }

    fn command_modifiers_down() -> bool {
        [VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .into_iter()
            .any(virtual_key_is_down)
    }

    fn keyboard_has_other_down(state: &[u8; 256]) -> bool {
        state[0xA0] & state[0xA1] & 0x80 != 0
            || state.iter().enumerate().any(|(key, value)| {
                key >= 8 && !matches!(key, 0x10 | 0xA0 | 0xA1) && value & 0x80 != 0
            })
    }

    fn other_keys_down() -> bool {
        let mut state = [0; 256];
        // Failure is not evidence of a standalone gesture.
        unsafe { GetKeyboardState(&mut state) }.is_err() || keyboard_has_other_down(&state)
    }

    #[implement(windows::Win32::UI::TextServices::ITfThreadMgrEventSink)]
    struct FocusInvalidator {
        generation: Rc<Cell<u64>>,
        ui: Option<std::rc::Weak<UiState>>,
        owner_thread: ThreadId,
        _registration: ObjectRegistration,
    }

    impl FocusInvalidator {
        fn invalidate(&self) -> Result<()> {
            if std::thread::current().id() != self.owner_thread {
                return Err(E_UNEXPECTED.into());
            }
            self.generation.set(self.generation.get().wrapping_add(1));
            if let Some(ui) = self.ui.as_ref().and_then(|ui| ui.upgrade()) {
                ui.end();
            }
            Ok(())
        }
    }

    impl windows::Win32::UI::TextServices::ITfThreadMgrEventSink_Impl for FocusInvalidator_Impl {
        fn OnInitDocumentMgr(
            &self,
            _: Option<&windows::Win32::UI::TextServices::ITfDocumentMgr>,
        ) -> Result<()> {
            Ok(())
        }
        fn OnUninitDocumentMgr(
            &self,
            _: Option<&windows::Win32::UI::TextServices::ITfDocumentMgr>,
        ) -> Result<()> {
            self.get_impl().invalidate()
        }
        fn OnSetFocus(
            &self,
            _: Option<&windows::Win32::UI::TextServices::ITfDocumentMgr>,
            _: Option<&windows::Win32::UI::TextServices::ITfDocumentMgr>,
        ) -> Result<()> {
            self.get_impl().invalidate()
        }
        fn OnPushContext(&self, _: Option<&ITfContext>) -> Result<()> {
            self.get_impl().invalidate()
        }
        fn OnPopContext(&self, _: Option<&ITfContext>) -> Result<()> {
            self.get_impl().invalidate()
        }
    }

    struct ThreadEvents {
        source: windows::Win32::UI::TextServices::ITfSource,
        cookie: u32,
    }

    impl ThreadEvents {
        #[cfg(test)]
        fn subscribe(manager: &ITfThreadMgr, generation: Rc<Cell<u64>>) -> Result<Self> {
            Self::subscribe_ui(manager, generation, None)
        }

        fn subscribe_ui(
            manager: &ITfThreadMgr,
            generation: Rc<Cell<u64>>,
            ui: Option<std::rc::Weak<UiState>>,
        ) -> Result<Self> {
            use windows::Win32::UI::TextServices::ITfThreadMgrEventSink;
            let source: windows::Win32::UI::TextServices::ITfSource = manager.cast()?;
            let sink: ITfThreadMgrEventSink = FocusInvalidator {
                generation,
                ui,
                owner_thread: std::thread::current().id(),
                _registration: ObjectRegistration::new(),
            }
            .into();
            let cookie = unsafe { source.AdviseSink(&ITfThreadMgrEventSink::IID, &sink) }?;
            Ok(Self { source, cookie })
        }
    }

    impl Drop for ThreadEvents {
        fn drop(&mut self) {
            let _ = unsafe { self.source.UnadviseSink(self.cookie) };
        }
    }

    impl ShiftGesture {
        fn down(
            &mut self,
            shift: Option<u32>,
            repeat: bool,
            command: bool,
            context: ContextKey,
            epoch: u64,
        ) {
            if repeat && shift.is_some() {
                if command {
                    self.pressed = None;
                }
                return;
            }
            self.pressed = match (shift, self.pressed, command) {
                (Some(key), None, false) => Some((key, context, epoch)),
                _ => None,
            };
        }

        fn matches(&self, key: u32, context: ContextKey, epoch: u64, command: bool) -> bool {
            !command && self.pressed == Some((key, context, epoch))
        }
    }

    fn claim_promotion(claim: &Cell<bool>, allowed: bool) -> bool {
        allowed && !claim.replace(true)
    }

    #[test]
    fn shift_gesture_rejects_chords_repeat_rearming_and_stale_focus() {
        let mut shift = ShiftGesture::default();
        shift.down(Some(42), false, false, 1, 2);
        assert!(shift.matches(42, 1, 2, false));
        assert!(!shift.matches(54, 1, 2, false));
        assert!(!shift.matches(42, 2, 2, false));
        assert!(!shift.matches(42, 1, 3, false));
        assert!(!shift.matches(42, 1, 2, true));
        shift.down(Some(42), true, false, 1, 2);
        assert!(shift.matches(42, 1, 2, false));
        shift.down(None, false, false, 1, 2); // Shift+letter cancels.
        shift.down(Some(42), true, false, 1, 2); // Auto-repeat cannot re-arm.
        assert!(shift.pressed.is_none());
        shift.down(Some(42), false, true, 1, 2);
        assert!(shift.pressed.is_none());
        shift.down(Some(42), false, false, 1, 2);
        shift.down(Some(54), false, false, 1, 2); // Both Shift keys are a chord.
        assert!(shift.pressed.is_none());
    }

    #[test]
    fn shift_keyboard_snapshot_rejects_preheld_letters_and_both_shifts() {
        let mut keys = [0u8; 256];
        keys[0xA0] = 0x80;
        assert!(!keyboard_has_other_down(&keys));
        keys[b'A' as usize] = 0x80;
        assert!(keyboard_has_other_down(&keys));
        keys[b'A' as usize] = 0;
        keys[0xA1] = 0x80;
        assert!(keyboard_has_other_down(&keys));
        keys[0xA1] = 0;
        keys[0x14] = 1; // Caps Lock toggle alone is not a held key.
        assert!(!keyboard_has_other_down(&keys));
        keys[VK_CONTROL.0 as usize] = 0x80;
        assert!(keyboard_has_other_down(&keys));
    }

    #[cfg(test)]
    #[test]
    fn unsafe_or_duplicate_completion_never_claims_a_second_promotion() {
        let claim = Cell::new(false);
        assert!(!claim_promotion(&claim, false));
        assert!(!claim.get());
        assert!(claim_promotion(&claim, true));
        assert!(!claim_promotion(&claim, true));
    }

    fn sync_idle_rankings(
        shared: &SharedActivation,
        token: ActivationToken,
        router: &mut EngineRouter,
    ) {
        if router.is_composing() {
            return;
        }
        if let Ok(activation) = shared.try_borrow() {
            if let Some(active) = activation.as_ref().filter(|a| a.token == token) {
                router.set_ranking_orders(Arc::clone(&active.rankings));
            }
        }
    }

    fn context_key(context: &ITfContext) -> Result<ContextKey> {
        // Only IID_IUnknown has COM's canonical identity guarantee. Callers
        // retain their context reference for the entire lifetime of this key.
        // A failed query must never become a raw-interface or sentinel key.
        Ok(context.cast::<IUnknown>()?.as_raw() as usize)
    }

    /// 查询当前 TSF 焦点文档的 top context。
    ///
    /// 调用方不得同时借用候选激活状态。`None` 表示查询失败或当前没有焦点
    /// context，二者都必须使延迟鼠标动作失效。
    fn focused_context_key(thread_manager: &ITfThreadMgr) -> Option<ContextKey> {
        // SAFETY: The thread manager is retained for the activation lifetime.
        // `GetFocus` and `GetTop` return counted COM references.
        unsafe {
            let document = thread_manager.GetFocus().ok()?;
            let context = document.GetTop().ok()?;
            context_key(&context).ok()
        }
    }

    fn virtual_key_is_down(virtual_key: VIRTUAL_KEY) -> bool {
        // SAFETY: GetKeyState has no pointer arguments and reports the current
        // thread's keyboard state.
        unsafe { GetKeyState(virtual_key.0 as i32) < 0 }
    }

    fn map_key_event(wparam: WPARAM) -> Option<KeyEvent> {
        let modifiers = Modifiers {
            shift: virtual_key_is_down(VK_SHIFT),
            control: virtual_key_is_down(VK_CONTROL),
            alt: virtual_key_is_down(VK_MENU),
            meta: virtual_key_is_down(VK_LWIN) || virtual_key_is_down(VK_RWIN),
        };

        if modifiers.is_command() {
            return None;
        }

        map_windows_virtual_key(wparam.0 as u32, modifiers)
    }

    /// Maps a popup construction failure to the HRESULT reported by `Activate`.
    fn popup_activation_hresult(error: &PopupError) -> HRESULT {
        match error {
            PopupError::Win32 { source, .. } => source.code(),
            PopupError::NoSnapshot | PopupError::Destroyed => E_UNEXPECTED,
        }
    }

    struct TfCompositionHost {
        context: ITfContext,
        composition: Option<ActiveComposition>,
        edit_cookie: u32,
    }

    impl TfCompositionHost {
        fn new(
            context: ITfContext,
            composition: Option<ActiveComposition>,
            edit_cookie: u32,
        ) -> Self {
            Self {
                context,
                composition,
                edit_cookie,
            }
        }

        fn into_composition(self) -> Option<ActiveComposition> {
            self.composition
        }

        fn text_range(&mut self) -> Result<ITfRange> {
            let Some(composition) = self.composition.clone() else {
                return Err(E_UNEXPECTED.into());
            };
            if composition.terminated.get() {
                return Err(E_UNEXPECTED.into());
            }

            // SAFETY: The composition is a live TSF object for this context.
            match unsafe { composition.GetRange() } {
                Ok(range) => Ok(range),
                Err(error) => {
                    self.end_composition_best_effort();
                    Err(error)
                }
            }
        }

        fn move_selection_to_end(&self, range: &ITfRange) -> Result<()> {
            self.move_selection(range, None)
        }

        fn move_selection(&self, range: &ITfRange, offset: Option<usize>) -> Result<()> {
            let caret = unsafe { range.Clone() }?;
            if let Some(offset) = offset {
                let offset = i32::try_from(offset).map_err(|_| Error::from(E_UNEXPECTED))?;
                unsafe { caret.Collapse(self.edit_cookie, TF_ANCHOR_START) }?;
                let mut shifted = 0;
                unsafe {
                    caret.ShiftEnd(self.edit_cookie, offset, &mut shifted, std::ptr::null())
                }?;
                if shifted != offset {
                    return Err(E_UNEXPECTED.into());
                }
            }
            unsafe { caret.Collapse(self.edit_cookie, TF_ANCHOR_END) }?;
            let selection = TF_SELECTION {
                range: std::mem::ManuallyDrop::new(Some(caret)),
                style: TF_SELECTIONSTYLE {
                    ase: TF_AE_NONE,
                    fInterimChar: FALSE,
                },
            };
            let result = unsafe {
                self.context
                    .SetSelection(self.edit_cookie, std::slice::from_ref(&selection))
            };
            drop(std::mem::ManuallyDrop::into_inner(selection.range));
            result
        }

        fn end_composition(&mut self) -> Result<()> {
            let Some(composition) = self.composition.take() else {
                return Ok(());
            };

            // SAFETY: The composition is live and this edit session owns a
            // read/write lock through `edit_cookie`.
            if let Err(error) = unsafe { composition.EndComposition(self.edit_cookie) } {
                self.composition = Some(composition);
                return Err(error);
            }

            Ok(())
        }

        fn end_composition_best_effort(&mut self) {
            let Some(composition) = self.composition.take() else {
                return;
            };

            // SAFETY: The composition was live before cleanup began. Cleanup is
            // best effort, and the original failure remains the reported error.
            let _ = unsafe { composition.EndComposition(self.edit_cookie) };
        }
    }

    fn validate_preedit_caret(preedit: &str, caret: Option<usize>) -> Result<()> {
        let Some(caret) = caret else { return Ok(()) };
        let mut units = 0;
        if caret == 0 {
            return Ok(());
        }
        for character in preedit.chars() {
            units += character.len_utf16();
            if units == caret {
                return Ok(());
            }
            if units > caret {
                break;
            }
        }
        Err(E_UNEXPECTED.into())
    }

    impl CompositionHost for TfCompositionHost {
        type Error = Error;

        fn begin(&mut self, preedit: &str, caret_utf16: Option<usize>) -> Result<()> {
            validate_preedit_caret(preedit, caret_utf16)?;
            let insert: Result<ITfInsertAtSelection> = self.context.cast();
            record_result(Stage::QueryInsert, &insert);
            let insert = insert?;
            // SAFETY: A query-only insertion returns a live range at the
            // selection without changing document text.
            let range =
                unsafe { insert.InsertTextAtSelection(self.edit_cookie, TF_IAS_QUERYONLY, &[]) };
            record_result(Stage::QueryInsertionRange, &range);
            let range = range?;
            let composition_manager: ITfContextComposition = self.context.cast()?;
            let terminated = Rc::new(Cell::new(false));
            let ui = Rc::new(RefCell::new(None));
            let sink: ITfCompositionSink = CompositionSink {
                terminated: Rc::clone(&terminated),
                ui: Rc::clone(&ui),
                owner_thread: std::thread::current().id(),
                _registration: ObjectRegistration::new(),
            }
            .into();

            // SAFETY: The queried range and context are live in the current
            // read/write edit session.
            let composition =
                unsafe { composition_manager.StartComposition(self.edit_cookie, &range, &sink) };
            record_result(Stage::StartComposition, &composition);
            let composition = ActiveComposition {
                handle: composition?,
                terminated,
                ui,
            };
            self.composition = Some(composition.clone());

            let text: Vec<u16> = preedit.encode_utf16().collect();
            // SAFETY: The new composition belongs to this edit session and the
            // UTF-16 buffer remains alive for the duration of the call.
            let set_text = unsafe { composition.GetRange() }.and_then(|range| {
                // SAFETY: `range` is live and `text` is a valid UTF-16 buffer.
                unsafe { range.SetText(self.edit_cookie, 0, &text) }?;
                self.move_selection(&range, caret_utf16)
            });
            record_result(Stage::SetInitialText, &set_text);

            if let Err(error) = set_text {
                self.end_composition_best_effort();
                return Err(error);
            }

            Ok(())
        }

        fn replace(&mut self, preedit: &str, caret_utf16: Option<usize>) -> Result<()> {
            validate_preedit_caret(preedit, caret_utf16)?;
            let range = self.text_range()?;
            let text: Vec<u16> = preedit.encode_utf16().collect();
            // SAFETY: The range belongs to the active composition, and the
            // UTF-16 buffer remains alive for the duration of the call.
            unsafe { range.SetText(self.edit_cookie, 0, &text) }?;
            self.move_selection(&range, caret_utf16)
        }

        fn commit(&mut self, text: &str) -> Result<()> {
            let text: Vec<u16> = text.encode_utf16().collect();
            if self.composition.is_none() {
                let insert: ITfInsertAtSelection = self.context.cast()?;
                // SAFETY: This is an explicit key commit in the current write
                // session. No surrounding document text is read.
                let range = unsafe {
                    insert.InsertTextAtSelection(
                        self.edit_cookie,
                        windows::Win32::UI::TextServices::INSERT_TEXT_AT_SELECTION_FLAGS(0),
                        &text,
                    )
                }?;
                return self.move_selection_to_end(&range);
            }
            let range = self.text_range()?;
            // SAFETY: The range belongs to the active composition, and the
            // UTF-16 buffer remains alive for the duration of the call.
            unsafe { range.SetText(self.edit_cookie, 0, &text) }?;
            self.move_selection_to_end(&range)?;
            self.end_composition()
        }

        fn cancel(&mut self) -> Result<()> {
            let range = self.text_range()?;
            // SAFETY: The range belongs to the active composition and this edit
            // session owns its read/write cookie. Clearing it removes only the
            // preedit inserted by Flower before ending the composition.
            unsafe { range.SetText(self.edit_cookie, 0, &[]) }?;
            self.move_selection_to_end(&range)?;
            self.end_composition()
        }
    }

    #[derive(Clone)]
    struct EditSessionOutcome {
        composition: Option<ActiveComposition>,
        controller: CompositionController,
        caret: Option<RECT>,
        learning_allowed: bool,
    }

    fn learning_scope_value_is_safe(scope: i32) -> bool {
        matches!(scope, 0 | 3 | 50 | 52 | 53 | 54 | 55 | 57 | 58 | 68)
    }

    #[derive(Clone, Copy)]
    enum ScopePolicy {
        Learning,
        Ai,
    }

    fn scope_policy(
        scopes: &[windows::Win32::UI::TextServices::InputScope],
        _policy: ScopePolicy,
    ) -> std::result::Result<(), ScopeBlockReason> {
        use windows::Win32::UI::TextServices::*;
        if scopes.iter().any(|s| {
            [
                IS_PASSWORD,
                IS_PRIVATE,
                IS_NUMERIC_PASSWORD,
                IS_NUMERIC_PIN,
                IS_ALPHANUMERIC_PIN,
                IS_ALPHANUMERIC_PIN_SET,
            ]
            .contains(s)
        }) {
            return Err(ScopeBlockReason::Sensitive);
        }
        if scopes.iter().all(|s| learning_scope_value_is_safe(s.0)) {
            Ok(())
        } else {
            Err(ScopeBlockReason::Unsupported)
        }
    }

    #[cfg(test)]
    #[test]
    fn learning_scope_policy_rejects_password_private_and_unknown_values() {
        for scope in [0, 3, 50, 52, 53, 54, 55, 57, 58, 68] {
            assert!(learning_scope_value_is_safe(scope));
        }
        for scope in [-1, 31, 61, 63, 64, 65, 66, 999] {
            assert!(!learning_scope_value_is_safe(scope));
        }
        use windows::Win32::UI::TextServices::InputScope;
        assert_eq!(
            scope_policy(&[InputScope(3)], ScopePolicy::Learning),
            Ok(())
        );
        assert!(learning_scope_value_is_safe(3));
        for scope in [0, 3, 50, 52, 53, 54, 55, 57, 58, 68] {
            assert_eq!(scope_policy(&[InputScope(scope)], ScopePolicy::Ai), Ok(()));
        }
        assert_eq!(
            scope_policy(&[InputScope(999), InputScope(61)], ScopePolicy::Ai),
            Err(ScopeBlockReason::Sensitive)
        );
        assert_eq!(
            scope_policy(&[InputScope(0), InputScope(999)], ScopePolicy::Ai),
            Err(ScopeBlockReason::Unsupported)
        );
    }

    #[cfg(test)]
    fn scope_allows_learning(context: &ITfContext, cookie: u32) -> bool {
        check_scope(context, cookie, ScopePolicy::Learning).is_ok()
    }

    fn check_scope(
        context: &ITfContext,
        cookie: u32,
        policy: ScopePolicy,
    ) -> std::result::Result<(), ScopeBlockReason> {
        let result = application_scope(context, cookie, policy);
        match result {
            Err(reason)
                if native_scope_fallback_allowed(policy, reason) && native_edit_scope(context) =>
            {
                Ok(())
            }
            _ => result,
        }
    }

    fn native_scope_fallback_allowed(_policy: ScopePolicy, reason: ScopeBlockReason) -> bool {
        matches!(
            reason,
            ScopeBlockReason::Property
                | ScopeBlockReason::Value
                | ScopeBlockReason::Undeclared
                | ScopeBlockReason::Empty
        )
    }

    fn check_ai_scope(
        context: &ITfContext,
        cookie: u32,
        allow_undeclared: bool,
    ) -> std::result::Result<(), ScopeBlockReason> {
        check_undeclared_scope(context, cookie, ScopePolicy::Ai, allow_undeclared)
    }

    fn check_learning_scope(
        context: &ITfContext,
        cookie: u32,
        allow_undeclared: bool,
    ) -> std::result::Result<(), ScopeBlockReason> {
        check_undeclared_scope(context, cookie, ScopePolicy::Learning, allow_undeclared)
    }

    fn check_undeclared_scope(
        context: &ITfContext,
        cookie: u32,
        policy: ScopePolicy,
        allow_undeclared: bool,
    ) -> std::result::Result<(), ScopeBlockReason> {
        let result = check_scope(context, cookie, policy);
        if allow_undeclared
            && matches!(
                result,
                Err(ScopeBlockReason::Undeclared | ScopeBlockReason::Empty)
            )
        {
            unsafe {
                if let Ok(window) = context.GetActiveView().and_then(|view| view.GetWnd()) {
                    if checked_edit_window(
                        window,
                        || context.GetActiveView().and_then(|view| view.GetWnd()).ok(),
                        true,
                    ) {
                        return Ok(());
                    }
                }
            }
        }
        result
    }

    fn native_edit_scope(context: &ITfContext) -> bool {
        // Called only on the edit-session thread, never cached between sending
        // and publication. No document text or window title is inspected.
        unsafe {
            let Ok(view) = context.GetActiveView() else {
                return false;
            };
            let Ok(window) = view.GetWnd() else {
                return false;
            };
            native_edit_window(window, || {
                context.GetActiveView().and_then(|v| v.GetWnd()).ok()
            })
        }
    }

    unsafe fn native_edit_window(
        window: windows::Win32::Foundation::HWND,
        current_view: impl Fn() -> Option<windows::Win32::Foundation::HWND>,
    ) -> bool {
        checked_edit_window(window, current_view, false)
    }

    unsafe fn checked_edit_window(
        window: windows::Win32::Foundation::HWND,
        current_view: impl Fn() -> Option<windows::Win32::Foundation::HWND>,
        allow_custom: bool,
    ) -> bool {
        use windows::Win32::Foundation::{GetLastError, SetLastError, WIN32_ERROR};
        use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
        use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
        use windows::Win32::UI::WindowsAndMessaging::*;
        let focus = GetFocus();
        let identity = || {
            let owned = |hwnd| {
                let mut process = 0;
                IsWindow(hwnd).as_bool()
                    && GetWindowThreadProcessId(hwnd, Some(&mut process)) == GetCurrentThreadId()
                    && process == GetCurrentProcessId()
            };
            owned(window)
                && owned(focus)
                && GetFocus() == focus
                && (window == focus || IsChild(window, focus).as_bool())
        };
        if !identity() {
            return false;
        }
        let native_class = || {
            let mut class = [0u16; 128];
            let count = GetClassNameW(focus, &mut class);
            if count <= 0 {
                return None;
            }
            let class = String::from_utf16_lossy(&class[..count as usize]);
            Some(
                [
                    "Edit",
                    "RichEdit20A",
                    "RichEdit20W",
                    "RICHEDIT50A",
                    "RICHEDIT50W",
                    "RichEditD2DPT",
                ]
                .iter()
                .any(|name| class.eq_ignore_ascii_case(name)),
            )
        };
        let attributes = || {
            if !matches!(native_class(), Some(native) if native || allow_custom) {
                return false;
            }
            SetLastError(WIN32_ERROR(0));
            let style = GetWindowLongW(focus, GWL_STYLE);
            if style == 0 && GetLastError() != WIN32_ERROR(0) {
                return false;
            }
            style as u32 & (0x20 | 0x800 | WS_DISABLED.0) == 0
        };
        if !attributes() {
            return false;
        }
        let password_clear = match native_class() {
            Some(true) => native_password_clear(focus),
            Some(false) => allow_custom,
            None => false,
        };
        // Querying the view can call application COM code; recheck identity and
        // attributes after it, as well as after the native class procedure.
        password_clear && current_view() == Some(window) && identity() && attributes()
    }

    unsafe fn native_password_clear(focus: windows::Win32::Foundation::HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            CallWindowProcW, GetClassLongPtrW, GCLP_WNDPROC, WNDPROC,
        };
        let procedure = GetClassLongPtrW(focus, GCLP_WNDPROC);
        if procedure == 0 {
            return false;
        }
        // The caller has checked the native class and current-thread ownership.
        // Use CallWindowProcW (not a direct function-pointer call), since Win32
        // may return a special internal procedure value. Bypass per-window
        // application subclass code for this read-only integer query. This
        // relies on the standard class procedure, not a hard real-time timeout.
        let procedure = std::mem::transmute::<usize, WNDPROC>(procedure);
        CallWindowProcW(procedure, focus, 0xD2, WPARAM(0), LPARAM(0)).0 == 0
    }

    fn application_scope(
        context: &ITfContext,
        cookie: u32,
        policy: ScopePolicy,
    ) -> std::result::Result<(), ScopeBlockReason> {
        use windows::Win32::UI::TextServices::{ITfInputScope, GUID_PROP_INPUTSCOPE};
        // Read the application's declaration over the actual selection inside
        // this edit session. Missing, unsupported, or mixed unsafe scopes deny
        // learning; never fall back to a different property channel.
        unsafe {
            let mut selection = [TF_SELECTION::default()];
            let mut fetched = 0;
            let result = context.GetSelection(cookie, u32::MAX, &mut selection, &mut fetched);
            let range = std::mem::ManuallyDrop::take(&mut selection[0].range);
            if result.is_err() || fetched != 1 {
                return Err(ScopeBlockReason::Selection);
            }
            let Some(range) = range else {
                return Err(ScopeBlockReason::Selection);
            };
            let property = context.GetAppProperty(&GUID_PROP_INPUTSCOPE);
            record_result(Stage::LearningAppProperty, &property);
            let Ok(property) = property else {
                return Err(ScopeBlockReason::Property);
            };
            let value = property.GetValue(cookie, &range);
            record_result(Stage::LearningScopeValue, &value);
            let Ok(value) = value else {
                return Err(ScopeBlockReason::Value);
            };
            // VARIANT's discriminant must be checked before querying its payload.
            let vt = value.as_raw().Anonymous.Anonymous.vt;
            if vt == 0 {
                return Err(ScopeBlockReason::Undeclared);
            }
            if vt != 13 {
                return Err(ScopeBlockReason::InvalidValue);
            }
            let Ok(unknown) = windows::core::IUnknown::try_from(&value) else {
                return Err(ScopeBlockReason::InvalidValue);
            };
            let Ok(scope) = unknown.cast::<ITfInputScope>() else {
                return Err(ScopeBlockReason::Interface);
            };
            let mut scopes = std::ptr::null_mut();
            let mut count = 0;
            let read = scope.GetInputScopes(&mut scopes, &mut count);
            struct ScopeAllocation(*mut windows::Win32::UI::TextServices::InputScope);
            impl Drop for ScopeAllocation {
                fn drop(&mut self) {
                    if !self.0.is_null() {
                        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(self.0.cast())) };
                    }
                }
            }
            let allocation = ScopeAllocation(scopes);
            read.map_err(|_| ScopeBlockReason::Read)?;
            if count == 0 {
                return Err(ScopeBlockReason::Empty);
            }
            if allocation.0.is_null() || count > 256 {
                return Err(ScopeBlockReason::InvalidList);
            }
            scope_policy(
                std::slice::from_raw_parts(allocation.0, count as usize),
                policy,
            )
        }
    }

    fn show_local_data_error() {
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
        unsafe {
            let _ = MessageBoxW(None,
            windows::core::w!("Flower 的本地词典加载或词频保存失败；已提交的文字不受影响。请打开 Flower 设置检查用户词典，原文件不会被重建或覆盖。"),
            windows::core::w!("Flower 本地词典"), MB_OK | MB_ICONWARNING);
        }
    }

    #[derive(Clone)]
    struct EditTarget {
        context_generation: Option<u64>,
        switch_to_english: bool,
        promotion_claim: Rc<Cell<bool>>,
        activation: SharedActivation,
        thread_manager: ITfThreadMgr,
        token: ActivationToken,
        owner_thread: ThreadId,
        key: ContextKey,
        focus_epoch: u64,
    }

    impl EditTarget {
        fn is_current(&self) -> bool {
            if std::thread::current().id() != self.owner_thread
                || focused_context_key(&self.thread_manager) != Some(self.key)
            {
                return false;
            }
            self.activation.try_borrow().ok().is_some_and(|state| {
                state.as_ref().is_some_and(|active| {
                    self.context_generation.map_or(true, |generation| {
                        generation == active.context_generation.get()
                    }) && active.token == self.token
                        && active.focus_epoch == self.focus_epoch
                        && active.foreground
                })
            })
        }
    }

    /// 异步鼠标编辑完成后发布状态所需的数据。
    ///
    /// 该结构不持有 `TextService` 的 COM 强引用，只持有一个可失效的激活状态
    /// 线程本地 `Rc<RefCell<_>>` 和 RAII processing guard，因此不会形成服务与
    /// 窗口回调环，也不会被错误线程共享。
    struct PopupActionCompletion {
        target: EditTarget,
        expected_revision: u64,
        entry: Option<ContextEntry>,
        update: Option<CompositionUpdate>,
        completed: bool,
        _processing: ProcessingGuard,
    }

    struct EditSessionState {
        composition: Option<ActiveComposition>,
        controller: CompositionController,
        outcome: Option<EditSessionOutcome>,
    }

    #[implement(ITfEditSession)]
    struct EditSession {
        key_preflight: Option<EditTarget>,
        context: ITfContext,
        update: CompositionUpdate,
        state: Mutex<EditSessionState>,
        completion: Mutex<Option<PopupActionCompletion>>,
        requires_preflight: bool,
        _registration: ObjectRegistration,
    }

    impl EditSession {
        fn new(
            context: ITfContext,
            update: CompositionUpdate,
            composition: Option<ActiveComposition>,
            controller: CompositionController,
        ) -> Self {
            Self {
                key_preflight: None,
                context,
                update,
                state: Mutex::new(EditSessionState {
                    composition,
                    controller,
                    outcome: None,
                }),
                completion: Mutex::new(None),
                requires_preflight: false,
                _registration: ObjectRegistration::new(),
            }
        }

        fn new_popup_action(
            context: ITfContext,
            update: CompositionUpdate,
            composition: Option<ActiveComposition>,
            controller: CompositionController,
            completion: PopupActionCompletion,
        ) -> Self {
            Self {
                key_preflight: None,
                context,
                update,
                state: Mutex::new(EditSessionState {
                    composition,
                    controller,
                    outcome: None,
                }),
                completion: Mutex::new(Some(completion)),
                requires_preflight: true,
                _registration: ObjectRegistration::new(),
            }
        }

        fn lock_state(&self) -> MutexGuard<'_, EditSessionState> {
            match self.state.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            }
        }

        fn take_outcome(&self) -> Option<EditSessionOutcome> {
            let mut state = self.lock_state();
            state.outcome.take()
        }

        fn take_completion(&self) -> Option<PopupActionCompletion> {
            match self.completion.lock() {
                Ok(mut guard) => guard.take(),
                Err(poisoned) => poisoned.into_inner().take(),
            }
        }
    }

    impl ITfEditSession_Impl for EditSession_Impl {
        fn DoEditSession(&self, edit_cookie: u32) -> Result<()> {
            record(Stage::DoEditSession, S_OK);
            let this = self.get_impl();
            if this
                .key_preflight
                .as_ref()
                .is_some_and(|target| !target.is_current())
            {
                return Err(E_UNEXPECTED.into());
            }
            let completion = this.take_completion();
            if this.requires_preflight && completion.is_none() {
                return Err(E_UNEXPECTED.into());
            }
            if completion
                .as_ref()
                .is_some_and(|completion| !completion.preflight())
            {
                if let Some(completion) = completion {
                    completion.invalidate();
                }
                return Err(E_UNEXPECTED.into());
            }

            let (composition, controller) = {
                let state = this.lock_state();
                (state.composition.clone(), state.controller)
            };
            if composition
                .as_ref()
                .is_some_and(|composition| composition.terminated.get())
            {
                return Err(E_UNEXPECTED.into());
            }
            // Copy the current activation setting, releasing the borrow before
            // application COM calls. Missing/stale snapshots grant no compatibility.
            let allow_undeclared = this
                .key_preflight
                .as_ref()
                .or_else(|| completion.as_ref().map(|completion| &completion.target))
                .is_some_and(|target| {
                    target.activation.try_borrow().ok().is_some_and(|state| {
                        state.as_ref().is_some_and(|active| {
                            active.token == target.token
                                && active.settings.ai_undeclared_scope_compatibility
                        })
                    })
                });
            let learning_allowed = this.update.commit.is_some()
                && check_learning_scope(&this.context, edit_cookie, allow_undeclared).is_ok();
            let mut host = TfCompositionHost::new(this.context.clone(), composition, edit_cookie);
            let mut controller = controller;
            let result = controller
                .apply_update(&mut host, &this.update)
                .and_then(|()| {
                    if controller.is_active()
                        && host
                            .composition
                            .as_ref()
                            .is_some_and(|composition| composition.terminated.get())
                    {
                        Err(E_UNEXPECTED.into())
                    } else {
                        Ok(())
                    }
                });
            record_result(Stage::ApplyUpdate, &result);
            let caret = if result.is_ok() && controller.is_active() {
                host.composition.as_ref().and_then(|composition| {
                    composition_caret(&this.context, composition, edit_cookie)
                })
            } else {
                None
            };
            let composition = host.into_composition();

            let mut state = this.lock_state();
            state.composition = composition.clone();
            let outcome = if result.is_ok() {
                Some(EditSessionOutcome {
                    composition,
                    controller,
                    caret,
                    learning_allowed,
                })
            } else {
                None
            };
            if result.is_ok() {
                state.controller = controller;
                state.outcome = outcome.clone();
            }
            drop(state);
            if let Some(completion) = completion {
                completion.finish(result.is_ok(), outcome);
            }
            result
        }
    }

    /// Returns the best-effort screen-space caret rectangle at the end of the
    /// composition preedit.
    ///
    /// This runs inside the synchronous TSF edit session, so the context view
    /// and composition range are valid. Failure only suppresses popup
    /// positioning; the document edit has already succeeded.
    fn composition_caret(
        context: &ITfContext,
        composition: &ITfComposition,
        edit_cookie: u32,
    ) -> Option<RECT> {
        // All TSF range and view queries stay inside the current edit session.
        // A layout miss must not suppress otherwise valid candidates forever.
        unsafe {
            let view: ITfContextView = context.GetActiveView().ok()?;
            if let Some(collapsed) = composition
                .GetRange()
                .ok()
                .and_then(|range| collapsed_range_end(&range, edit_cookie))
            {
                let mut rect = RECT::default();
                let mut clipped = BOOL::default();
                let result = view.GetTextExt(edit_cookie, &collapsed, &mut rect, &mut clipped);
                record_result(Stage::LayoutTextExt, &result);
                if result.is_ok() {
                    if clipped.as_bool() {
                        return None;
                    }
                    if valid_caret_rect(rect) {
                        return Some(rect);
                    }
                }
            }
            let viewport = view
                .GetScreenExt()
                .ok()
                .filter(|r| r.right > r.left && r.bottom > r.top)?;
            if let Some(rect) = gui_caret_for_view(&view, viewport) {
                record(Stage::AnchorGui, S_OK);
                return Some(rect);
            }
            record(Stage::AnchorView, S_OK);
            viewport_edge_anchor(viewport)
        }
    }

    fn valid_caret_rect(rect: RECT) -> bool {
        rect.right >= rect.left && rect.bottom > rect.top
    }

    fn viewport_edge_anchor(rect: RECT) -> Option<RECT> {
        (rect.right > rect.left && rect.bottom > rect.top).then(|| RECT {
            left: rect.left,
            right: rect.left,
            top: rect.bottom.saturating_sub(1),
            bottom: rect.bottom,
        })
    }

    #[cfg(test)]
    #[test]
    fn caret_geometry_accepts_zero_width_but_never_empty_or_inverted_viewports() {
        assert!(valid_caret_rect(RECT {
            left: -20,
            top: 10,
            right: -20,
            bottom: 26
        }));
        for rect in [
            RECT::default(),
            RECT {
                left: 20,
                top: 10,
                right: 19,
                bottom: 26,
            },
            RECT {
                left: 20,
                top: 10,
                right: 22,
                bottom: 10,
            },
        ] {
            assert!(!valid_caret_rect(rect));
        }
        assert!(viewport_edge_anchor(RECT::default()).is_none());
        assert!(viewport_edge_anchor(RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 200
        })
        .is_none());
        let anchor = viewport_edge_anchor(RECT {
            left: -1920,
            top: -200,
            right: -1000,
            bottom: 600,
        })
        .unwrap();
        assert_eq!(
            (anchor.left, anchor.right, anchor.top, anchor.bottom),
            (-1920, -1920, 599, 600)
        );
    }

    unsafe fn gui_caret_for_view(view: &ITfContextView, viewport: RECT) -> Option<RECT> {
        let window = view.GetWnd().ok()?;
        if window.0.is_null() {
            return None;
        }
        let thread = GetWindowThreadProcessId(window, None);
        if thread == 0 {
            return None;
        }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread, &mut info).ok()?;
        let caret_window = info.hwndCaret;
        if caret_window.0.is_null()
            || !(caret_window == window || IsChild(window, caret_window).as_bool())
            || !(info.hwndFocus == window || IsChild(window, info.hwndFocus).as_bool())
        {
            return None;
        }
        let mut top_left = POINT {
            x: info.rcCaret.left,
            y: info.rcCaret.top,
        };
        let mut bottom_right = POINT {
            x: info.rcCaret.right,
            y: info.rcCaret.bottom,
        };
        if !ClientToScreen(caret_window, &mut top_left).as_bool()
            || !ClientToScreen(caret_window, &mut bottom_right).as_bool()
        {
            return None;
        }
        let rect = RECT {
            left: top_left.x,
            top: top_left.y,
            right: bottom_right.x,
            bottom: bottom_right.y,
        };
        (valid_caret_rect(rect)
            && rect.left >= viewport.left
            && rect.right <= viewport.right
            && rect.top >= viewport.top
            && rect.bottom <= viewport.bottom)
            .then_some(rect)
    }

    /// Clones a range and collapses it to its end for caret positioning.
    fn collapsed_range_end(range: &ITfRange, edit_cookie: u32) -> Option<ITfRange> {
        // SAFETY: `range` is a live range owned by the current edit session.
        // `ITfRange::Clone` creates an independent TSF range object, so
        // collapsing the clone cannot modify the composition range. Failure to
        // clone or collapse only suppresses popup positioning.
        unsafe {
            let collapsed = range.Clone().ok()?;
            collapsed.Collapse(edit_cookie, TF_ANCHOR_END).ok()?;
            Some(collapsed)
        }
    }

    /// 请求一次同步键盘编辑并取回编辑会话结果。
    ///
    /// TSF `SetText` 与 `EndComposition` 不是单一事务：即使调用返回错误，
    /// 宿主文档也可能已经被部分修改。调用方只能恢复引擎侧快照，不能把
    /// `None` 解释为“文档保证未变”或执行自动回滚。
    fn request_checked_key_edit_session(
        context: &ITfContext,
        client_id: u32,
        update: CompositionUpdate,
        composition: Option<ActiveComposition>,
        controller: CompositionController,
        target: Option<EditTarget>,
    ) -> Option<EditSessionOutcome> {
        let mut edit = EditSession::new(context.clone(), update, composition, controller);
        edit.key_preflight = target;
        let session: ITfEditSession = edit.into();

        // SAFETY: The context is live and `session` remains alive until the
        // synchronous request returns.
        let requested = unsafe {
            context.RequestEditSession(client_id, &session, TF_ES_SYNC | TF_ES_READWRITE)
        };
        record_result(Stage::RequestKeyEdit, &requested);
        if let Ok(status) = requested.as_ref() {
            record(Stage::EditSessionResult, *status);
        }
        match requested {
            // SAFETY: `session` was created from `EditSession` above.
            Ok(result) if result.is_ok() => unsafe {
                <ITfEditSession as AsImpl<EditSession>>::as_impl(&session).take_outcome()
            },
            Ok(_) | Err(_) => None,
        }
    }

    fn dispatch_popup_action(
        shared: &SharedActivation,
        token: ActivationToken,
        owner_thread: ThreadId,
        action: CandidatePopupAction,
    ) {
        if std::thread::current().id() != owner_thread {
            return;
        }
        if !ui_allows(shared, token) {
            return;
        }

        let initial = {
            let Ok(activation) = shared.try_borrow() else {
                return;
            };
            let Some(active) = activation
                .as_ref()
                .filter(|active| active.token == token && active.foreground)
            else {
                return;
            };
            if active.processing {
                return;
            }
            let Some(key) = active.shown_context else {
                return;
            };
            let Some(entry) = active.contexts.get(&key) else {
                return;
            };
            let Some(snapshot) = entry.popup_model.snapshot() else {
                return;
            };
            if entry.popup_model.accept_action(&action).is_err() {
                return;
            }
            (
                active.thread_manager.clone(),
                key,
                active.focus_epoch,
                snapshot.revision,
            )
        };
        let (thread_manager, key, focus_epoch, expected_revision) = initial;

        // Query TSF focus without borrowing the thread-local activation.
        let focus_context_matches = focused_context_key(&thread_manager) == Some(key);
        if !focus_context_matches {
            clear_invalid_popup_action(shared, token, owner_thread, key);
            return;
        }

        let ticket = DeferredActionTicket {
            activation: token,
            focus_epoch,
            context: key,
            revision: expected_revision,
        };
        let prepared = {
            let Ok(mut activation) = shared.try_borrow_mut() else {
                return;
            };
            let Some(active) = activation.as_mut().filter(|active| active.token == token) else {
                return;
            };
            if active.processing {
                return;
            }
            let state = deferred_action_state(active, ticket, owner_thread, focus_context_matches);
            if !deferred_action_is_valid(ticket, state) {
                None
            } else {
                let Some(entry) = active.contexts.get(&key) else {
                    return;
                };
                let Ok(accepted) = entry.popup_model.accept_action(&action) else {
                    return;
                };
                let entry = entry.clone();
                if let Some(ai) = active.ai.as_mut() {
                    ai.invalidate();
                }
                active.processing = true;
                let processing = ProcessingGuard {
                    activation: Rc::clone(shared),
                    token,
                    owner_thread,
                };
                Some((
                    entry,
                    active.client_id,
                    thread_manager,
                    accepted,
                    processing,
                ))
            }
        };
        let Some((old_entry, client_id, thread_manager, accepted, processing)) = prepared else {
            clear_invalid_popup_action(shared, token, owner_thread, key);
            return;
        };

        let mut entry = old_entry;
        let update = match accepted {
            AcceptedCandidateAction::Select { index, .. } => entry.router.select_candidate(index),
            AcceptedCandidateAction::PreviousPage { .. } => entry.router.previous_page(),
            AcceptedCandidateAction::NextPage { .. } => entry.router.next_page(),
        };
        let update = match update {
            Ok(update) => update,
            Err(_) => {
                clear_invalid_popup_action(shared, token, owner_thread, key);
                return;
            }
        };

        let update_for_completion = update.clone();
        let context = entry.context.clone();
        let composition = entry.composition.take();
        let controller = entry.controller;
        let target = EditTarget {
            context_generation: None,
            switch_to_english: false,
            promotion_claim: Rc::new(Cell::new(false)),
            activation: Rc::clone(shared),
            thread_manager,
            token,
            owner_thread,
            key,
            focus_epoch,
        };
        let completion = PopupActionCompletion {
            target,
            expected_revision,
            entry: Some(entry),
            update: Some(update_for_completion),
            completed: false,
            _processing: processing,
        };
        let session: ITfEditSession = EditSession::new_popup_action(
            context.clone(),
            update,
            composition,
            controller,
            completion,
        )
        .into();

        // SAFETY: The context belongs to the retained activation entry and
        // remains live through the COM session reference. Async dispatch
        // prevents document edits from running inside the popup WndProc.
        let requested = unsafe {
            context.RequestEditSession(client_id, &session, TF_ES_ASYNC | TF_ES_READWRITE)
        };
        if !matches!(&requested, Ok(result) if result.is_ok()) {
            // SAFETY: `session` was created from `EditSession` above.
            if let Some(completion) = unsafe {
                <ITfEditSession as AsImpl<EditSession>>::as_impl(&session).take_completion()
            } {
                completion.invalidate();
            }
        }
    }

    fn deferred_action_state(
        active: &Activation,
        ticket: DeferredActionTicket,
        owner_thread: ThreadId,
        focus_context_matches: bool,
    ) -> DeferredActionState {
        DeferredActionState {
            owner_thread_matches: std::thread::current().id() == owner_thread,
            activation: Some(active.token),
            foreground: active.foreground,
            focus_epoch: active.focus_epoch,
            focus_context_matches,
            shown_context: active.shown_context,
            revision: active
                .contexts
                .get(&ticket.context)
                .and_then(|entry| entry.popup_model.snapshot())
                .map(|snapshot| snapshot.revision),
        }
    }

    fn clear_invalid_popup_action(
        shared: &SharedActivation,
        token: ActivationToken,
        owner_thread: ThreadId,
        key: ContextKey,
    ) {
        if std::thread::current().id() != owner_thread {
            return;
        }
        {
            let Ok(mut activation) = shared.try_borrow_mut() else {
                return;
            };
            if let Some(active) = activation
                .as_mut()
                .filter(|active| active.token == token && active.shown_context == Some(key))
            {
                active.shown_context = None;
            }
        }

        if let Some(ui) = activation_ui(shared, token) {
            ui.end();
        }

        let Some(mut popup) = take_thread_popup(token, owner_thread) else {
            return;
        };
        popup.window.cancel_pending_actions();
        popup.window.hide();
        let still_owned = shared.try_borrow().ok().is_some_and(
            |activation| matches!(activation.as_ref(), Some(active) if active.token == token),
        );
        if still_owned {
            return_thread_popup(token, owner_thread, popup);
        }
    }

    #[implement(ITfTextInputProcessorEx, ITfTextInputProcessor, ITfKeyEventSink)]
    struct TextService {
        activation: SharedActivation,
        stores: Option<(SettingsStore, DictionaryStore)>,
        owner_thread: ThreadId,
        _registration: ObjectRegistration,
    }

    impl TextService {
        fn new() -> Self {
            Self {
                activation: Rc::new(RefCell::new(None)),
                stores: None,
                owner_thread: std::thread::current().id(),
                _registration: ObjectRegistration::new(),
            }
        }

        fn owns_current_thread(&self) -> bool {
            std::thread::current().id() == self.owner_thread
        }

        /// Restores a context entry only when the activation that started the
        /// key is still active. The entry and any displaced entry are dropped
        /// after the activation borrow is released.
        fn restore_context(&self, token: ActivationToken, key: ContextKey, entry: ContextEntry) {
            restore_activation_context(&self.activation, token, key, entry);
        }
    }

    fn restore_activation_context(
        shared: &SharedActivation,
        token: ActivationToken,
        key: ContextKey,
        entry: ContextEntry,
    ) {
        let mut entry = Some(entry);
        let displaced = {
            let Ok(mut activation) = shared.try_borrow_mut() else {
                drop(entry);
                return;
            };
            activation
                .as_mut()
                .filter(|active| active.token == token)
                .and_then(|active| {
                    entry
                        .take()
                        .and_then(|entry| active.contexts.insert(key, entry))
                })
        };

        // TSF objects and popup state are released after the activation borrow.
        drop(entry);
        drop(displaced);
    }

    /// Clears an activation's in-flight edit flag when the callback exits.
    ///
    /// The flag is cleared only while the same activation token is still
    /// present, so a callback that outlived a Deactivate/Activate cycle cannot
    /// modify the replacement activation.
    struct ProcessingGuard {
        activation: SharedActivation,
        token: ActivationToken,
        owner_thread: ThreadId,
    }

    impl Drop for ProcessingGuard {
        fn drop(&mut self) {
            if std::thread::current().id() != self.owner_thread {
                return;
            }
            let token = self.token;
            let Ok(mut activation) = self.activation.try_borrow_mut() else {
                return;
            };
            if let Some(active) = activation.as_mut().filter(|active| active.token == token) {
                active.processing = false;
            }
        }
    }

    /// 将一次成功编辑的 router/popup 状态发布回当前激活。
    ///
    /// 键盘和鼠标共用此路径。Win32 调用发生在激活借用之外；重入按键由
    /// `ProcessingGuard` 的 processing 标志拒绝。
    fn publish_successful_edit(
        target: &EditTarget,
        mut entry: ContextEntry,
        update: &CompositionUpdate,
        outcome: EditSessionOutcome,
    ) {
        if std::thread::current().id() != target.owner_thread {
            drop(entry);
            return;
        }
        if outcome.controller.is_active()
            && outcome
                .composition
                .as_ref()
                .is_some_and(|composition| composition.terminated.get())
        {
            clear_invalid_popup_action(
                &target.activation,
                target.token,
                target.owner_thread,
                target.key,
            );
            return;
        }
        let learning_allowed = outcome.learning_allowed;
        let events = if update.commit.is_some() {
            entry.router.pending_promotions().to_vec()
        } else {
            Vec::new()
        };
        entry.composition = outcome.composition;
        entry.controller = outcome.controller;
        let caret = outcome.caret;

        let focus_context_matches = focused_context_key(&target.thread_manager) == Some(target.key);
        let (decision, shown_context) = {
            let Ok(activation) = target.activation.try_borrow() else {
                drop(entry);
                return;
            };
            let (current_token, current_epoch, shown_context) = match activation.as_ref() {
                Some(active) => (Some(active.token), active.focus_epoch, active.shown_context),
                None => (None, 0, None),
            };
            (
                edit_publish_decision(
                    current_token,
                    target.token,
                    current_epoch,
                    target.focus_epoch,
                ),
                shown_context,
            )
        };

        if decision == EditPublishDecision::Discard {
            // 新的激活不能接收旧 token 的编辑结果。
            drop(entry);
            return;
        }

        let allow_show = decision == EditPublishDecision::PublishAndShow && focus_context_matches;
        let mut popup = take_thread_popup(target.token, target.owner_thread);
        let mut visible = false;
        let mut display_update = update.clone();
        if entry.router.is_composing() {
            if let (Some(popup), Some(caret)) = (popup.as_ref(), caret) {
                if let Ok(width) = popup.window.candidate_width_budget(PopupRect::new(
                    caret.left,
                    caret.top,
                    caret.right,
                    caret.bottom,
                )) {
                    display_update = entry.router.set_candidate_width(Some(width));
                }
            }
        }
        entry.popup_model.apply_update(&display_update);

        if let Some(ui) = activation_ui(&target.activation, target.token) {
            if allow_show {
                ui.publish(
                    &entry.context,
                    entry.popup_model.snapshot(),
                    caret.map(|r| PopupRect::new(r.left, r.top, r.right, r.bottom)),
                    target.token,
                    target.owner_thread,
                );
                if let Some(composition) = &entry.composition {
                    let live = ui
                        .live
                        .borrow()
                        .as_ref()
                        .map(|live| Rc::downgrade(&live.data));
                    *composition.ui.borrow_mut() = live.map(|data| (Rc::downgrade(&ui), data));
                }
            } else {
                ui.end();
            }
        }

        if let Some(popup) = popup.as_mut() {
            if shown_context != Some(target.key) {
                popup.window.hide();
            }

            if entry
                .popup_model
                .snapshot()
                .is_some_and(|snapshot| popup.window.update(snapshot).is_ok())
            {
                let can_show_now = allow_show && ui_allows(&target.activation, target.token) && {
                    target
                        .activation
                        .try_borrow()
                        .ok()
                        .is_some_and(|activation| {
                            matches!(
                                activation.as_ref(),
                                Some(active)
                                    if active.token == target.token
                                        && active.focus_epoch == target.focus_epoch
                            )
                        })
                };
                visible = can_show_now
                    && caret.is_some_and(|caret| {
                        popup
                            .window
                            .show_at(PopupRect::new(
                                caret.left,
                                caret.top,
                                caret.right,
                                caret.bottom,
                            ))
                            .is_ok()
                    });
            }

            if !visible {
                popup.window.hide();
            }
        }

        if let Some(ui) = activation_ui(&target.activation, target.token) {
            ui.set_shown(visible);
        }

        // Publish the entry and popup visibility with one short borrow. Focus or
        // activation changes observed here win over the captured key state.
        let mut entry = Some(entry);
        let mut displaced = None;
        let mut keep_popup = false;
        let mut hide_now = visible;
        {
            let Ok(mut activation) = target.activation.try_borrow_mut() else {
                drop(entry);
                return;
            };
            if let Some(active) = activation
                .as_mut()
                .filter(|active| active.token == target.token)
            {
                keep_popup = true;
                let still_focused = active.foreground
                    && active.focus_epoch == target.focus_epoch
                    && focus_context_matches
                    && target.context_generation.map_or(true, |generation| {
                        generation == active.context_generation.get()
                    });
                if target.switch_to_english && still_focused {
                    active.english = true;
                }
                if !events.is_empty()
                    && claim_promotion(
                        &target.promotion_claim,
                        still_focused && learning_allowed && active.adaptive_ranking_enabled,
                    )
                {
                    let mut orders = active.rankings.as_ref().clone();
                    for event in &events {
                        promote_order(&mut orders, event);
                    }
                    if orders.iter().map(|o| o.candidates.len()).sum::<usize>() <= 100_000 {
                        active.rankings = Arc::new(orders);
                    } else {
                        active.local_data_error = true;
                    }
                    if let Some(worker) = active.learning.as_ref() {
                        if worker.submit_promotions(&events).is_err() {
                            active.local_data_error = true;
                        }
                    }
                }
                if still_focused
                    && entry.as_ref().is_some_and(|e| {
                        e.popup_model
                            .snapshot()
                            .is_some_and(|s| !s.candidates.is_empty())
                    })
                {
                    active.shown_context = Some(target.key);
                    hide_now = !visible;
                } else {
                    active.shown_context = None;
                }
                if let Some(mut entry) = entry.take() {
                    if let Some(ai) = active.ai.as_mut() {
                        ai.input();
                    }
                    if !entry.router.is_composing() {
                        entry
                            .router
                            .set_ranking_orders(Arc::clone(&active.rankings));
                    }
                    displaced = active.contexts.insert(target.key, entry);
                }
            }
        }
        drop(entry);
        drop(displaced);

        if let Some(popup) = popup {
            if hide_now {
                popup.window.hide();
                keep_popup = target
                    .activation
                    .try_borrow()
                    .ok()
                    .is_some_and(|activation| {
                        matches!(
                            activation.as_ref(),
                            Some(active) if active.token == target.token
                        )
                    });
            }

            if keep_popup {
                return_thread_popup(target.token, target.owner_thread, popup);
            } else {
                // Activation ended while the popup was leased; destroy outside
                // the activation borrow.
                drop(popup);
            }
        }
    }

    impl PopupActionCompletion {
        fn preflight(&self) -> bool {
            if self
                .entry
                .as_ref()
                .is_some_and(ContextEntry::composition_terminated)
            {
                return false;
            }
            if std::thread::current().id() != self.target.owner_thread {
                return false;
            }
            let focus_context_matches =
                focused_context_key(&self.target.thread_manager) == Some(self.target.key);
            let ticket = DeferredActionTicket {
                activation: self.target.token,
                focus_epoch: self.target.focus_epoch,
                context: self.target.key,
                revision: self.expected_revision,
            };
            let Ok(activation) = self.target.activation.try_borrow() else {
                return false;
            };
            let Some(active) = activation
                .as_ref()
                .filter(|active| active.token == self.target.token)
            else {
                return false;
            };
            if !active.processing || !active.ui.allowed() {
                return false;
            }
            deferred_action_is_valid(
                ticket,
                deferred_action_state(
                    active,
                    ticket,
                    self.target.owner_thread,
                    focus_context_matches,
                ),
            )
        }

        fn invalidate(mut self) {
            self.completed = true;
            clear_invalid_popup_action(
                &self.target.activation,
                self.target.token,
                self.target.owner_thread,
                self.target.key,
            );
        }

        fn finish(mut self, succeeded: bool, outcome: Option<EditSessionOutcome>) {
            self.completed = true;
            let Some(outcome) = outcome else {
                clear_invalid_popup_action(
                    &self.target.activation,
                    self.target.token,
                    self.target.owner_thread,
                    self.target.key,
                );
                return;
            };
            if !succeeded {
                clear_invalid_popup_action(
                    &self.target.activation,
                    self.target.token,
                    self.target.owner_thread,
                    self.target.key,
                );
                return;
            }
            let Some(entry) = self.entry.take() else {
                return;
            };
            let Some(update) = self.update.take() else {
                return;
            };
            publish_successful_edit(&self.target, entry, &update, outcome);
        }
    }

    impl Drop for PopupActionCompletion {
        fn drop(&mut self) {
            if !self.completed {
                clear_invalid_popup_action(
                    &self.target.activation,
                    self.target.token,
                    self.target.owner_thread,
                    self.target.key,
                );
            }
        }
    }

    impl Drop for TextService {
        fn drop(&mut self) {
            let active = self
                .activation
                .try_borrow_mut()
                .ok()
                .and_then(|mut activation| activation.take());

            if let Some(active) = active {
                active.ui.close();
                // Best effort: only the owning thread can destroy the window.
                close_thread_popup(active.token, active.owner_thread);
                // Release TSF references after the activation borrow is gone.
                drop(active);
            }
        }
    }

    impl TextService_Impl {
        fn activate_internal(
            &self,
            thread_manager: Option<&ITfThreadMgr>,
            client_id: u32,
            flags: u32,
        ) -> Result<()> {
            record(Stage::Activate, S_OK);
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            let Some(thread_manager) = thread_manager else {
                return Err(E_INVALIDARG.into());
            };

            {
                let activation = self
                    .get_impl()
                    .activation
                    .try_borrow()
                    .map_err(|_| Error::from(E_UNEXPECTED))?;
                if activation.is_some() {
                    return Err(E_UNEXPECTED.into());
                }
            }

            // Load configuration once per activation, before borrowing the
            // activation state and before creating its popup. Errors use
            // defaults without rewriting the configured file or surfacing a
            // host UI.
            let settings = self
                .get_impl()
                .stores
                .as_ref()
                .map(|(settings, _)| Ok(settings.clone()))
                .unwrap_or_else(SettingsStore::for_current_user)
                .and_then(|store| store.load())
                .unwrap_or_default();
            let mut lexicon = Lexicon::builtin().map_err(|_| Error::from(E_UNEXPECTED))?;
            let mut local_data_error = false;
            let mut learning = None;
            let mut rankings = Arc::new(Vec::new());
            let mut adaptive_ranking_enabled = false;
            match self
                .get_impl()
                .stores
                .as_ref()
                .map(|(_, dictionary)| Ok(dictionary.clone()))
                .unwrap_or_else(DictionaryStore::for_current_user)
                .and_then(|store| store.load().map(|data| (store, data)))
            {
                Ok((store, data)) => {
                    match data
                        .lexicon_tsv()
                        .ok()
                        .and_then(|tsv| lexicon.with_overlay(&tsv).ok())
                    {
                        Some(snapshot) => {
                            lexicon = snapshot;
                            adaptive_ranking_enabled = data.adaptive_ranking_enabled;
                            if adaptive_ranking_enabled {
                                rankings = Arc::new(data.rankings.clone());
                                match LearningWorker::start(store, data.generation) {
                                    Ok(worker) => learning = Some(worker),
                                    Err(_) => local_data_error = true,
                                }
                            }
                        }
                        None => local_data_error = true,
                    }
                }
                Err(_) => local_data_error = true,
            }
            let ui_manager: ITfUIElementMgr = thread_manager.cast()?;
            let keystroke_manager: ITfKeystrokeMgr = thread_manager.cast()?;
            let key_sink: ITfKeyEventSink =
                <Self as ComObjectInterface<ITfKeyEventSink>>::as_interface_ref(self).to_owned();

            let context_generation = Rc::new(Cell::new(0));
            let ui = Rc::new(UiState {
                manager: Some(ui_manager),
                ..UiState::default()
            });
            let thread_events = ThreadEvents::subscribe_ui(
                thread_manager,
                Rc::clone(&context_generation),
                Some(Rc::downgrade(&ui)),
            )?;
            let popup = CandidatePopupWindow::new();
            if let Err(error) = &popup {
                record(Stage::PopupCreate, popup_activation_hresult(error));
            } else {
                record(Stage::PopupCreate, S_OK);
            }
            let mut popup = match popup {
                Ok(popup) => popup,
                Err(error) => return Err(popup_activation_hresult(&error).into()),
            };
            if let Err(error) = popup.set_theme(settings.theme) {
                record(Stage::PopupTheme, popup_activation_hresult(&error));
                return Err(popup_activation_hresult(&error).into());
            }
            let token = next_activation_token();
            let owner_thread = std::thread::current().id();
            let mut ai_unavailable = AiStatus::Unavailable;
            let mut ai = if settings.ai_correction_enabled {
                match protected_ai_helper().and_then(|helper| {
                    crate::ai::ipc::AiClient::start(helper).map_err(|_| AiStatus::ClientFailed)
                }) {
                    Ok(client) => Some(AiScheduler::new(Box::new(client))),
                    Err(reason) => {
                        ai_unavailable = reason;
                        None
                    }
                }
            } else {
                None
            };
            if ai.is_some() {
                let weak = Rc::downgrade(&self.get_impl().activation);
                let sink: Rc<dyn Fn()> = Rc::new(move || {
                    if let Some(shared) = weak.upgrade() {
                        ai_idle(&shared, token, owner_thread);
                    }
                });
                if popup.set_idle_sink(Some(sink)).is_err() {
                    ai = None;
                    ai_unavailable = AiStatus::TimerFailed;
                    let _ = popup.set_idle_sink(None);
                }
            }
            let _ = popup.set_ai_status(if !settings.ai_correction_enabled {
                AiStatus::Disabled
            } else if ai.is_some() {
                AiStatus::Waiting
            } else {
                ai_unavailable
            });
            let action_activation = Rc::clone(&self.get_impl().activation);
            let action_sink: Rc<dyn Fn(CandidatePopupAction)> = Rc::new(move |action| {
                dispatch_popup_action(&action_activation, token, owner_thread, action);
            });
            if let Err(error) = popup.set_action_sink(Some(action_sink)) {
                record(Stage::PopupActionSink, popup_activation_hresult(&error));
                return Err(popup_activation_hresult(&error).into());
            }
            if !register_thread_popup(token, popup) {
                return Err(E_UNEXPECTED.into());
            }

            let published = {
                let Ok(mut activation) = self.get_impl().activation.try_borrow_mut() else {
                    close_thread_popup(token, owner_thread);
                    return Err(E_UNEXPECTED.into());
                };
                if activation.is_some() {
                    false
                } else {
                    *activation = Some(Activation {
                        ui,
                        _activation_flags: flags,
                        thread_manager: thread_manager.clone(),
                        keystroke_manager: keystroke_manager.clone(),
                        client_id,
                        contexts: HashMap::new(),
                        token,
                        owner_thread,
                        focus_epoch: 0,
                        foreground: true,
                        shown_context: None,
                        processing: false,
                        english: false,
                        shift: ShiftGesture::default(),
                        context_generation: Rc::clone(&context_generation),
                        _thread_events: Some(thread_events),
                        settings,
                        lexicon,
                        rankings,
                        adaptive_ranking_enabled,
                        _learning_registration: learning
                            .as_ref()
                            .map(|_| ObjectRegistration::new()),
                        learning,
                        local_data_error,
                        ai,
                        ai_unavailable,
                    });
                    true
                }
            };
            if !published {
                close_thread_popup(token, owner_thread);
                return Err(E_UNEXPECTED.into());
            }

            // SAFETY: Both interfaces belong to live COM objects. TSF retains
            // its own reference to `key_sink` after a successful advise.
            let advised =
                unsafe { keystroke_manager.AdviseKeyEventSink(client_id, &key_sink, TRUE) };
            record_result(Stage::AdviseKeySink, &advised);

            if let Err(error) = advised {
                let stale = {
                    let Ok(mut activation) = self.get_impl().activation.try_borrow_mut() else {
                        close_thread_popup(token, owner_thread);
                        return Err(error);
                    };
                    let should_rollback = matches!(
                        activation.as_ref(),
                        Some(active) if active.token == token
                    );
                    if should_rollback {
                        activation.take()
                    } else {
                        None
                    }
                };
                drop(stale);
                close_thread_popup(token, owner_thread);
                return Err(error);
            }

            record(Stage::ActivationReady, S_OK);
            Ok(())
        }
    }

    impl ITfTextInputProcessorEx_Impl for TextService_Impl {
        fn ActivateEx(
            &self,
            manager: Option<&ITfThreadMgr>,
            client: u32,
            flags: u32,
        ) -> Result<()> {
            self.activate_internal(manager, client, flags)
        }
    }

    impl ITfTextInputProcessor_Impl for TextService_Impl {
        fn Activate(&self, manager: Option<&ITfThreadMgr>, client: u32) -> Result<()> {
            self.activate_internal(manager, client, 0)
        }
        fn Deactivate(&self) -> Result<()> {
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            let active = {
                let Ok(mut activation) = self.get_impl().activation.try_borrow_mut() else {
                    return Err(E_UNEXPECTED.into());
                };
                activation.take()
            };
            let Some(mut active) = active else {
                return Ok(());
            };
            active.ui.close();

            // SAFETY: The keystroke manager was obtained from the thread manager
            // during activation and remains live in this state.
            let unadvised = unsafe {
                active
                    .keystroke_manager
                    .UnadviseKeyEventSink(active.client_id)
            };
            if let Err(error) = unadvised {
                let token = active.token;
                let owner_thread = active.owner_thread;
                let displaced = {
                    let Ok(mut activation) = self.get_impl().activation.try_borrow_mut() else {
                        close_thread_popup(token, owner_thread);
                        return Err(error);
                    };
                    if activation.is_none() {
                        activation.replace(active)
                    } else {
                        Some(active)
                    }
                };
                let restored = displaced.is_none();
                drop(displaced);
                if !restored {
                    // A newer activation won the race, so the failed
                    // deactivation state cannot be restored. Release its
                    // window rather than leaking a thread-local popup.
                    close_thread_popup(token, owner_thread);
                }
                return Err(error);
            }

            close_thread_popup(active.token, active.owner_thread);
            active.shown_context = None;
            active.contexts.clear();
            if let Some(worker) = active.learning.as_mut() {
                if worker.shutdown().is_err() {
                    active.local_data_error = true;
                }
            }
            let local_data_error = active.local_data_error;
            drop(active);
            if local_data_error {
                show_local_data_error();
            }
            Ok(())
        }
    }

    impl TextService_Impl {
        fn key_mode(&self, context: &ITfContext) -> Option<(u64, bool)> {
            let (manager, epoch, english) = {
                let state = self.get_impl().activation.try_borrow().ok()?;
                let active = state.as_ref().filter(|a| a.foreground && !a.processing)?;
                (
                    active.thread_manager.clone(),
                    active.focus_epoch,
                    active.english,
                )
            };
            (focused_context_key(&manager) == Some(context_key(context).ok()?))
                .then_some((epoch, english))
        }

        fn apply_engine_key(&self, context: &ITfContext, event: Option<KeyEvent>) -> Result<BOOL> {
            let key = context_key(context)?;

            let (
                entry,
                client_id,
                token,
                owner_thread,
                focus_epoch,
                thread_manager,
                settings,
                lexicon,
                context_generation,
            ) = {
                let Ok(mut activation) = self.get_impl().activation.try_borrow_mut() else {
                    return Ok(FALSE);
                };
                let Some(active) = activation.as_mut() else {
                    return Ok(FALSE);
                };
                if active.processing {
                    return Ok(FALSE);
                }
                let entry = active.contexts.remove(&key);
                let client_id = active.client_id;
                let token = active.token;
                let owner_thread = active.owner_thread;
                let focus_epoch = active.focus_epoch;
                let thread_manager = active.thread_manager.clone();
                let settings = active.settings.clone();
                active.processing = true;
                (
                    entry,
                    client_id,
                    token,
                    owner_thread,
                    focus_epoch,
                    thread_manager,
                    settings,
                    active.lexicon.clone(),
                    active.context_generation.get(),
                )
            };
            let _guard = ProcessingGuard {
                activation: Rc::clone(&self.get_impl().activation),
                token,
                owner_thread,
            };

            let was_existing = entry.is_some();
            let mut old_entry = entry.clone();
            let mut entry = match entry {
                Some(entry) => entry,
                None => match ContextEntry::new(context, &settings, &lexicon) {
                    Some(entry) => entry,
                    None => return Ok(FALSE),
                },
            };
            if entry.composition_terminated() {
                clear_invalid_popup_action(&self.get_impl().activation, token, owner_thread, key);
                let Ok(router) =
                    EngineRouter::from_lexicon(lexicon.clone(), settings.candidate_page_size)
                else {
                    return Ok(FALSE);
                };
                entry.router = router;
                entry
                    .router
                    .set_candidate_width(Some(crate::engine::paging::DEFAULT_CANDIDATE_WIDTH));
                entry.composition = None;
                entry.controller = CompositionController::new();
                entry.popup_model.hide();
                old_entry = Some(entry.clone());
            }

            sync_idle_rankings(&self.get_impl().activation, token, &mut entry.router);
            let update = match match event {
                Some(event) => entry.router.process_key(event),
                None => Ok(entry.router.commit_raw()),
            } {
                Ok(update) => update,
                Err(_) => {
                    if let Some(old_entry) = old_entry {
                        self.get_impl().restore_context(token, key, old_entry);
                    }
                    return Ok(FALSE);
                }
            };

            if !update.handled {
                if was_existing {
                    self.get_impl().restore_context(token, key, entry);
                }
                return Ok(FALSE);
            }

            let target = EditTarget {
                context_generation: event.is_none().then_some(context_generation),
                switch_to_english: event.is_none(),
                promotion_claim: Rc::new(Cell::new(false)),
                activation: Rc::clone(&self.get_impl().activation),
                thread_manager,
                token,
                owner_thread,
                key,
                focus_epoch,
            };
            let update_for_popup = update.clone();
            let outcome = request_checked_key_edit_session(
                &entry.context,
                client_id,
                update,
                entry.composition.take(),
                entry.controller,
                Some(target.clone()),
            );

            let Some(outcome) = outcome else {
                // The edit session did not report success. Restore the engine
                // and popup snapshots, but do not claim the TSF document is
                // unchanged: SetText may already have partially applied before
                // a later EndComposition failure.
                if let Some(old_entry) = old_entry {
                    self.get_impl().restore_context(token, key, old_entry);
                }
                return Ok(FALSE);
            };

            publish_successful_edit(&target, entry, &update_for_popup, outcome);

            Ok(TRUE)
        }
    }

    impl ITfKeyEventSink_Impl for TextService_Impl {
        fn OnSetFocus(&self, foreground: BOOL) -> Result<()> {
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            // Every focus transition invalidates key work that started under
            // the previous focus state. Focus loss additionally hides the
            // popup and clears its context.
            let popup = {
                let Ok(mut activation) = self.get_impl().activation.try_borrow_mut() else {
                    return Err(E_UNEXPECTED.into());
                };
                activation.as_mut().and_then(|active| {
                    active.focus_epoch = active.focus_epoch.wrapping_add(1);
                    if let Some(ai) = active.ai.as_mut() {
                        ai.invalidate();
                    }
                    active.foreground = foreground.as_bool();
                    active.shift.pressed = None;
                    if foreground.as_bool() {
                        None
                    } else {
                        active.shown_context = None;
                        Some((active.token, active.owner_thread))
                    }
                })
            };

            if let Some((token, owner_thread)) = popup {
                if let Some(ui) = activation_ui(&self.get_impl().activation, token) {
                    ui.end();
                }
                if let Some(mut popup) = take_thread_popup(token, owner_thread) {
                    popup.window.cancel_pending_actions();
                    popup.window.hide();

                    let still_owned = {
                        self.get_impl()
                            .activation
                            .try_borrow()
                            .ok()
                            .is_some_and(|activation| {
                                matches!(activation.as_ref(), Some(active) if active.token == token)
                            })
                    };
                    if still_owned {
                        return_thread_popup(token, owner_thread, popup);
                    } else {
                        // The activation ended while the window was leased;
                        // dropping destroys it outside the activation borrow.
                        drop(popup);
                    }
                }
            }

            Ok(())
        }

        fn OnTestKeyDown(
            &self,
            context: Option<&ITfContext>,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> Result<BOOL> {
            record(Stage::TestKeyDown, S_OK);
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            let Some(context) = context else {
                return Ok(FALSE);
            };
            let Some((_, english)) = self.key_mode(context) else {
                return Ok(FALSE);
            };
            // Route real chord keys through OnKeyDown so they can disarm Shift
            // even when they ultimately pass through in English mode.
            let pending = self
                .get_impl()
                .activation
                .try_borrow()
                .ok()
                .is_some_and(|state| {
                    state
                        .as_ref()
                        .is_some_and(|active| active.shift.pressed.is_some())
                });
            if shift_identity(wparam.0, lparam).is_some() || pending {
                return Ok(TRUE);
            }
            if english {
                return Ok(FALSE);
            }
            let Some(event) = map_key_event(wparam) else {
                return Ok(FALSE);
            };
            let key = context_key(context)?;

            let (router, page_size, lexicon, rankings) = {
                let Ok(activation) = self.get_impl().activation.try_borrow() else {
                    return Ok(FALSE);
                };
                let Some(active) = activation.as_ref() else {
                    return Ok(FALSE);
                };
                if active.processing {
                    return Ok(FALSE);
                }
                (
                    active
                        .contexts
                        .get(&key)
                        .filter(|entry| !entry.composition_terminated())
                        .map(|entry| entry.router.clone()),
                    active.settings.candidate_page_size,
                    active.lexicon.clone(),
                    Arc::clone(&active.rankings),
                )
            };
            let mut router = match router {
                Some(router) => router,
                None => match EngineRouter::from_lexicon(lexicon, page_size) {
                    Ok(mut router) => {
                        router.set_candidate_width(Some(
                            crate::engine::paging::DEFAULT_CANDIDATE_WIDTH,
                        ));
                        router
                    }
                    Err(_) => return Ok(FALSE),
                },
            };
            if !router.is_composing() {
                router.set_ranking_orders(rankings);
            }

            match router.test_key(event) {
                Ok(true) => Ok(TRUE),
                Ok(false) | Err(_) => Ok(FALSE),
            }
        }

        fn OnTestKeyUp(
            &self,
            context: Option<&ITfContext>,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> Result<BOOL> {
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            let (Some(context), Some(_shift)) = (context, shift_identity(wparam.0, lparam)) else {
                return Ok(FALSE);
            };
            if self.key_mode(context).is_none() {
                return Ok(FALSE);
            }
            let Ok(state) = self.get_impl().activation.try_borrow() else {
                return Ok(FALSE);
            };
            Ok(BOOL::from(state.as_ref().is_some_and(|active| {
                // Even a cancelled chord release must reach the real callback
                // to clear its pending gesture; only OnKeyUp decides to eat it.
                active.shift.pressed.is_some()
            })))
        }

        fn OnKeyDown(
            &self,
            context: Option<&ITfContext>,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> Result<BOOL> {
            record(Stage::KeyDown, S_OK);
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            let Some(context) = context else {
                return Ok(FALSE);
            };
            let shift = shift_identity(wparam.0, lparam);
            let Some((epoch, english)) = self.key_mode(context) else {
                return Ok(FALSE);
            };
            let key = context_key(context)?;
            {
                let Ok(mut state) = self.get_impl().activation.try_borrow_mut() else {
                    return Ok(FALSE);
                };
                let Some(active) = state
                    .as_mut()
                    .filter(|a| !a.processing && a.focus_epoch == epoch)
                else {
                    return Ok(FALSE);
                };
                if let Some(ai) = active.ai.as_mut() {
                    ai.invalidate();
                }
                let was_pressed = active.shift.pressed.is_some();
                active.shift.down(
                    shift,
                    lparam.0 as usize & (1 << 30) != 0,
                    command_modifiers_down() || other_keys_down(),
                    key,
                    epoch,
                );
                if !was_pressed && active.shift.pressed.is_some() {
                    active.shift.context_generation = active.context_generation.get();
                }
            }
            // Let native Shift reach the host so normal Shift shortcuts keep working.
            if shift.is_some() || english {
                return Ok(FALSE);
            }
            let Some(event) = map_key_event(wparam) else {
                return Ok(FALSE);
            };
            self.apply_engine_key(context, Some(event))
        }

        fn OnKeyUp(
            &self,
            context: Option<&ITfContext>,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> Result<BOOL> {
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            let Some(shift) = shift_identity(wparam.0, lparam) else {
                return Ok(FALSE);
            };
            let mode = context.and_then(|context| self.key_mode(context));
            // Query COM before borrowing activation state. Identity failure
            // invalidates the gesture, but its pending release must still clear.
            let key = context.and_then(|context| context_key(context).ok());
            let Ok(mut state) = self.get_impl().activation.try_borrow_mut() else {
                return Ok(FALSE);
            };
            let Some(active) = state.as_mut() else {
                return Ok(FALSE);
            };
            let valid = key.zip(mode).is_some_and(|(key, (epoch, _))| {
                active.foreground
                    && !active.processing
                    && active.focus_epoch == epoch
                    && active.shift.context_generation == active.context_generation.get()
                    && active.shift.matches(
                        shift,
                        key,
                        epoch,
                        command_modifiers_down() || other_keys_down(),
                    )
            });
            active.shift.pressed = None;
            if !valid {
                return Ok(FALSE);
            }
            let context = context.unwrap();
            let composing = active
                .contexts
                .get(&key.ok_or(Error::from(E_UNEXPECTED))?)
                .is_some_and(|entry| entry.router.is_composing());
            if active.english || !composing {
                active.english = !active.english;
                return Ok(TRUE);
            }
            drop(state);
            self.apply_engine_key(context, None)
        }

        fn OnPreservedKey(
            &self,
            _context: Option<&ITfContext>,
            _guid: *const GUID,
        ) -> Result<BOOL> {
            if !self.get_impl().owns_current_thread() {
                return Err(E_UNEXPECTED.into());
            }
            if let Ok(mut state) = self.get_impl().activation.try_borrow_mut() {
                if let Some(active) = state.as_mut() {
                    active.shift.pressed = None;
                }
            }
            Ok(FALSE)
        }
    }

    fn get_class_object(
        class_id: *const GUID,
        interface_id: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        if object.is_null() {
            return Err(E_POINTER.into());
        }

        // SAFETY: The null check above establishes that the COM output pointer
        // is writable.
        unsafe {
            object.write(ptr::null_mut());
        }

        if class_id.is_null() || interface_id.is_null() {
            return Err(E_POINTER.into());
        }

        // SAFETY: Both identifiers were checked for null above.
        if unsafe { *class_id } != FLOWER_TEXT_SERVICE_CLSID {
            return Err(CLASS_E_CLASSNOTAVAILABLE.into());
        }

        let factory: IUnknown = ClassFactory::new().into();

        // SAFETY: `interface_id` and `object` are non-null. The query transfers
        // one counted interface reference to the caller.
        unsafe {
            factory.query(interface_id, object).ok()?;
        }

        Ok(())
    }

    /// Reports whether the DLL has no live COM objects or server locks.
    #[no_mangle]
    pub extern "system" fn DllCanUnloadNow() -> HRESULT {
        if can_unload() {
            S_OK
        } else {
            S_FALSE
        }
    }

    /// Returns the Flower class factory for the requested COM interface.
    #[no_mangle]
    pub extern "system" fn DllGetClassObject(
        class_id: *const GUID,
        interface_id: *const GUID,
        object: *mut *mut c_void,
    ) -> HRESULT {
        match catch_unwind(AssertUnwindSafe(|| {
            get_class_object(class_id, interface_id, object)
        })) {
            Ok(Ok(())) => S_OK,
            Ok(Err(error)) => error.code(),
            Err(_) => E_UNEXPECTED,
        }
    }

    #[cfg(test)]
    mod native_tests {
        include!("compatibility_tests.rs");
        use super::*;
        // Native fixtures are known-good COM contexts; production callers must
        // propagate QueryInterface failure instead of using this assertion.
        fn context_key(context: &ITfContext) -> ContextKey {
            super::context_key(context).expect("fixture canonical IUnknown")
        }

        fn basic_service(host: &NativeDocument) -> (ITfTextInputProcessor, SharedActivation) {
            let service: ITfTextInputProcessor = TextService::new().into();
            let implementation =
                unsafe { <ITfTextInputProcessor as AsImpl<TextService>>::as_impl(&service) };
            let shared = Rc::clone(&implementation.activation);
            *shared.borrow_mut() = Some(Activation {
                ui: Rc::new(UiState::default()),
                _activation_flags: 0,
                thread_manager: host.manager.clone(),
                keystroke_manager: host.manager.cast().unwrap(),
                client_id: host.client,
                contexts: HashMap::new(),
                token: next_activation_token(),
                owner_thread: std::thread::current().id(),
                focus_epoch: 0,
                foreground: true,
                shown_context: None,
                processing: false,
                english: false,
                shift: ShiftGesture::default(),
                context_generation: Rc::new(Cell::new(0)),
                _thread_events: None,
                settings: Settings::default(),
                lexicon: Lexicon::from_tsv("ni\t你\t100\nhao\t好\t100\nni'hao\t你好\t200").unwrap(),
                rankings: Arc::new(Vec::new()),
                adaptive_ranking_enabled: true,
                learning: None,
                _learning_registration: None,
                local_data_error: false,
                ai: None,
                ai_unavailable: AiStatus::Unavailable,
            });
            (service, shared)
        }

        // Identity-only tear-offs exercise the exact QI/key and key prediction
        // paths. They are borrowed stack objects, never passed to a TSF edit
        // request or used to invoke an ITfContext method beyond IUnknown.
        #[repr(C)]
        struct IdentityAlias {
            vtable: *const windows::Win32::UI::TextServices::ITfContext_Vtbl,
            identity: IUnknown,
            context: ITfContext,
            fail: bool,
        }

        unsafe extern "system" fn alias_query(
            this: *mut c_void,
            iid: *const GUID,
            out: *mut *mut c_void,
        ) -> HRESULT {
            let alias = unsafe { &*(this as *const IdentityAlias) };
            unsafe { *out = ptr::null_mut() };
            if alias.fail || unsafe { *iid } != IUnknown::IID {
                return windows::Win32::Foundation::E_NOINTERFACE;
            }
            unsafe { *out = alias.identity.clone().into_raw() };
            S_OK
        }

        unsafe extern "system" fn alias_ref(_: *mut c_void) -> u32 {
            1
        }

        // Keep a complete, correctly typed context vtable even though this
        // fixture only exercises identity paths. Any accidental context call
        // forwards using the real context's `this`, never the tear-off address.
        macro_rules! alias_forward {
            ($name:ident, $field:ident, $($arg:ident: $ty:ty),*) => {
                unsafe extern "system" fn $name(this: *mut c_void, $($arg: $ty),*) -> HRESULT {
                    let context = unsafe { &(*(this as *const IdentityAlias)).context };
                    unsafe { (context.vtable().$field)(context.as_raw(), $($arg),*) }
                }
            };
        }
        alias_forward!(alias_edit, RequestEditSession, client: u32, edit: *mut c_void,
            flags: windows::Win32::UI::TextServices::TF_CONTEXT_EDIT_CONTEXT_FLAGS, result: *mut HRESULT);
        alias_forward!(alias_writing, InWriteSession, client: u32, result: *mut BOOL);
        alias_forward!(alias_get_selection, GetSelection, cookie: u32, index: u32, count: u32,
            selections: *mut TF_SELECTION, fetched: *mut u32);
        alias_forward!(alias_set_selection, SetSelection, cookie: u32, count: u32, selections: *const TF_SELECTION);
        alias_forward!(alias_start, GetStart, cookie: u32, result: *mut *mut c_void);
        alias_forward!(alias_end, GetEnd, cookie: u32, result: *mut *mut c_void);
        alias_forward!(alias_view, GetActiveView, result: *mut *mut c_void);
        alias_forward!(alias_views, EnumViews, result: *mut *mut c_void);
        alias_forward!(alias_status, GetStatus, result: *mut TS_STATUS);
        alias_forward!(alias_property, GetProperty, guid: *const GUID, result: *mut *mut c_void);
        alias_forward!(alias_app_property, GetAppProperty, guid: *const GUID, result: *mut *mut c_void);
        alias_forward!(alias_track, TrackProperties, props: *const *const GUID, count: u32,
            apps: *const *const GUID, app_count: u32, result: *mut *mut c_void);
        alias_forward!(alias_properties, EnumProperties, result: *mut *mut c_void);
        alias_forward!(alias_document, GetDocumentMgr, result: *mut *mut c_void);
        alias_forward!(alias_backup, CreateRangeBackup, cookie: u32, range: *mut c_void, result: *mut *mut c_void);

        static ALIAS_VTABLE: windows::Win32::UI::TextServices::ITfContext_Vtbl =
            windows::Win32::UI::TextServices::ITfContext_Vtbl {
                base__: windows::core::IUnknown_Vtbl {
                    QueryInterface: alias_query,
                    AddRef: alias_ref,
                    Release: alias_ref,
                },
                RequestEditSession: alias_edit,
                InWriteSession: alias_writing,
                GetSelection: alias_get_selection,
                SetSelection: alias_set_selection,
                GetStart: alias_start,
                GetEnd: alias_end,
                GetActiveView: alias_view,
                EnumViews: alias_views,
                GetStatus: alias_status,
                GetProperty: alias_property,
                GetAppProperty: alias_app_property,
                TrackProperties: alias_track,
                EnumProperties: alias_properties,
                GetDocumentMgr: alias_document,
                CreateRangeBackup: alias_backup,
            };

        #[test]
        fn native_host_boundary_canonical_identity_and_preflight() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            let host = NativeDocument::new().unwrap();
            let other: IUnknown = ClassFactory::new().into();
            let (service, shared) = basic_service(&host);
            let sink: ITfKeyEventSink = service.cast().unwrap();
            let mut a = IdentityAlias {
                vtable: &ALIAS_VTABLE,
                identity: host.context.cast().unwrap(),
                context: host.context.clone(),
                fail: false,
            };
            let mut b = IdentityAlias {
                vtable: &ALIAS_VTABLE,
                identity: a.identity.clone(),
                context: host.context.clone(),
                fail: false,
            };
            let mut wrong = IdentityAlias {
                vtable: &ALIAS_VTABLE,
                identity: other,
                context: host.context.clone(),
                fail: false,
            };
            let mut failed = IdentityAlias {
                vtable: &ALIAS_VTABLE,
                identity: a.identity.clone(),
                context: host.context.clone(),
                fail: true,
            };
            let as_context = |alias: &mut IdentityAlias| {
                std::mem::ManuallyDrop::new(unsafe {
                    ITfContext::from_raw(alias as *mut IdentityAlias as *mut c_void)
                })
            };
            let a = as_context(&mut a);
            let b = as_context(&mut b);
            let wrong = as_context(&mut wrong);
            let failed = as_context(&mut failed);
            assert_ne!(a.as_raw(), b.as_raw());
            assert_eq!(
                super::context_key(&a).unwrap(),
                super::context_key(&b).unwrap()
            );
            assert_eq!(super::context_key(&a).unwrap(), context_key(&host.context));
            assert_ne!(
                super::context_key(&wrong).unwrap(),
                context_key(&host.context)
            );
            assert!(super::context_key(&failed).is_err());
            for context in [&*a, &*b] {
                assert!(
                    unsafe { sink.OnTestKeyDown(context, WPARAM(b'N' as usize), LPARAM(0)) }
                        .unwrap()
                        .as_bool()
                );
            }
            for context in [&*wrong, &*failed] {
                assert!(
                    !unsafe { sink.OnTestKeyDown(context, WPARAM(b'N' as usize), LPARAM(0)) }
                        .unwrap()
                        .as_bool()
                );
                assert!(
                    !unsafe { sink.OnKeyDown(context, WPARAM(b'N' as usize), LPARAM(0)) }
                        .unwrap()
                        .as_bool()
                );
            }
            let state = shared.borrow();
            let active = state.as_ref().unwrap();
            let mut target = EditTarget {
                context_generation: Some(active.context_generation.get()),
                switch_to_english: false,
                promotion_claim: Rc::new(Cell::new(false)),
                activation: shared.clone(),
                thread_manager: host.manager.clone(),
                token: active.token,
                owner_thread: active.owner_thread,
                key: context_key(&host.context),
                focus_epoch: active.focus_epoch,
            };
            drop(state);
            assert!(target.is_current());
            target.key = super::context_key(&wrong).unwrap();
            assert!(
                !target.is_current(),
                "edit preflight must reject a different COM identity"
            );
            let mut router =
                EngineRouter::from_lexicon(Lexicon::from_tsv("ni\t你\t100").unwrap(), 9).unwrap();
            let update = router
                .process_key(map_windows_virtual_key(b'N' as u32, Modifiers::NONE).unwrap())
                .unwrap();
            assert!(
                request_checked_key_edit_session(
                    &host.context,
                    host.client,
                    update.clone(),
                    None,
                    CompositionController::new(),
                    Some(target.clone()),
                )
                .is_none(),
                "real edit session must reject wrong-identity preflight"
            );
            assert_eq!(read_document(&host, None), "");
            target.key = super::context_key(&b).unwrap();
            let outcome = request_checked_key_edit_session(
                &host.context,
                host.client,
                update,
                None,
                CompositionController::new(),
                Some(target),
            )
            .expect("same-identity alias must pass real edit preflight");
            assert_eq!(read_document(&host, None), "n");
            read_document(&host, outcome.composition);
            assert!(
                !unsafe { sink.OnKeyDown(&host.context, WPARAM(0xA0), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            );
            assert!(shared.borrow().as_ref().unwrap().shift.pressed.is_some());
            assert!(!unsafe { sink.OnKeyUp(&*failed, WPARAM(0xA0), LPARAM(0)) }
                .unwrap()
                .as_bool());
            assert!(shared.borrow().as_ref().unwrap().shift.pressed.is_none());
            assert!(!shared.borrow().as_ref().unwrap().english);
            assert!(shared.borrow().as_ref().unwrap().contexts.is_empty());
            shared.borrow_mut().take();
        }

        fn selection_offset(context: &ITfContext, cookie: u32) -> Result<usize> {
            let mut selections = [TF_SELECTION::default()];
            let mut fetched = 0;
            unsafe { context.GetSelection(cookie, u32::MAX, &mut selections, &mut fetched) }?;
            if fetched != 1 {
                return Err(E_UNEXPECTED.into());
            }
            let selected =
                std::mem::ManuallyDrop::into_inner(std::mem::take(&mut selections[0].range))
                    .ok_or(Error::from(E_UNEXPECTED))?;
            let start = unsafe { context.GetStart(cookie) }?;
            unsafe { start.ShiftEndToRange(cookie, &selected, TF_ANCHOR_START) }?;
            let mut text = [0u16; 128];
            let mut count = 0;
            unsafe { start.GetText(cookie, 0, &mut text, &mut count) }?;
            assert!(unsafe { selected.IsEmpty(cookie) }?.as_bool());
            Ok(count as usize)
        }

        #[implement(ITfEditSession)]
        struct SelectionProbe {
            context: ITfContext,
            offset: Rc<Cell<usize>>,
        }

        impl ITfEditSession_Impl for SelectionProbe_Impl {
            fn DoEditSession(&self, cookie: u32) -> Result<()> {
                self.offset.set(selection_offset(&self.context, cookie)?);
                Ok(())
            }
        }

        fn read_selection(host: &NativeDocument) -> usize {
            let offset = Rc::new(Cell::new(usize::MAX));
            let session: ITfEditSession = SelectionProbe {
                context: host.context.clone(),
                offset: offset.clone(),
            }
            .into();
            unsafe {
                host.context
                    .RequestEditSession(host.client, &session, TF_ES_SYNC | TF_ES_READ)
            }
            .unwrap()
            .ok()
            .unwrap();
            offset.get()
        }

        #[implement(ITfEditSession)]
        struct CaretProbe {
            context: ITfContext,
        }

        impl ITfEditSession_Impl for CaretProbe_Impl {
            fn DoEditSession(&self, cookie: u32) -> Result<()> {
                let mut host = TfCompositionHost::new(self.context.clone(), None, cookie);
                assert!(host.begin("😀", Some(1)).is_err());
                assert!(host.composition.is_none());
                host.begin("你😀ab", Some(3))?;
                assert_eq!(selection_offset(&self.context, cookie)?, 3);
                // Neither out-of-range nor split-surrogate offsets may mutate
                // text or real selection.
                assert!(host.replace("错😀xx", Some(2)).is_err());
                assert!(host.replace("你😀ab", Some(6)).is_err());
                assert_eq!(selection_offset(&self.context, cookie)?, 3);
                let mut text = [0u16; 32];
                let mut count = 0;
                unsafe { host.text_range()?.GetText(cookie, 0, &mut text, &mut count) }?;
                assert_eq!(
                    String::from_utf16(&text[..count as usize]).unwrap(),
                    "你😀ab"
                );
                host.replace("你😀axb", Some(4))?;
                assert_eq!(selection_offset(&self.context, cookie)?, 4);
                host.replace("你😀ab", Some(3))?;
                assert_eq!(selection_offset(&self.context, cookie)?, 3);
                host.commit("你")?;
                assert_eq!(selection_offset(&self.context, cookie)?, 1);
                Ok(())
            }
        }

        #[test]
        fn native_host_boundary_caret_utf16_and_keys() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            {
                let host = NativeDocument::new().unwrap();
                let session: ITfEditSession = CaretProbe {
                    context: host.context.clone(),
                }
                .into();
                unsafe {
                    host.context.RequestEditSession(
                        host.client,
                        &session,
                        TF_ES_SYNC | TF_ES_READWRITE,
                    )
                }
                .unwrap()
                .ok()
                .unwrap();
                assert_eq!(read_document(&host, None), "你");
            }
            let host = NativeDocument::new().unwrap();
            let (service, shared) = basic_service(&host);
            let sink: ITfKeyEventSink = service.cast().unwrap();
            for key in b"NIHAO" {
                send_key(&sink, &host.context, *key);
            }
            assert_eq!(read_selection(&host), 5);
            for (key, text, offset) in [
                (0x25, "nihao", 4),
                (0x25, "nihao", 3),
                (b'X', "nihxao", 4),
                (0x08, "nihao", 3),
                (0x2e, "niho", 3),
                (0x27, "niho", 4),
            ] {
                let before = read_selection(&host);
                assert!(unsafe {
                    sink.OnTestKeyDown(&host.context, WPARAM(key as usize), LPARAM(0))
                }
                .unwrap()
                .as_bool());
                assert_eq!(
                    read_selection(&host),
                    before,
                    "prediction must not move selection"
                );
                send_key(&sink, &host.context, key);
                assert_eq!(read_document(&host, None), text);
                assert_eq!(read_selection(&host), offset);
            }
            send_key(&sink, &host.context, 0x1b);
            shared.borrow_mut().take();
        }
        use windows::core::{BSTR, VARIANT};
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::TextServices::{
            CLSID_TF_ThreadMgr, ITfCompositionView, ITfContextOwnerCompositionServices,
            ITfDocumentMgr, TF_ES_READ, TF_POPF_ALL,
        };
        use windows::Win32::UI::TextServices::{
            ITfContextOwner, ITfContextOwner_Impl, ITfInputScope, ITfInputScope_Impl, InputScope,
            GUID_PROP_INPUTSCOPE, TS_STATUS,
        };

        thread_local! {
            static COMPAT_VIEW: Cell<windows::Win32::Foundation::HWND> = const {
                Cell::new(windows::Win32::Foundation::HWND(ptr::null_mut()))
            };
        }

        #[implement(ITfEditSession)]
        struct CompatScopeSession {
            context: ITfContext,
            expected: std::result::Result<(), ScopeBlockReason>,
        }

        impl ITfEditSession_Impl for CompatScopeSession_Impl {
            fn DoEditSession(&self, cookie: u32) -> Result<()> {
                assert!(native_edit_scope(&self.context));
                assert_eq!(
                    check_scope(&self.context, cookie, ScopePolicy::Ai),
                    self.expected
                );
                assert_eq!(
                    scope_allows_learning(&self.context, cookie),
                    self.expected.is_ok()
                );
                Ok(())
            }
        }

        #[test]
        fn native_compat_context_view_edit_and_richedit() {
            use windows::core::{w, PCWSTR};
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
            use windows::Win32::UI::WindowsAndMessaging::*;
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            unsafe {
                let module =
                    windows::Win32::System::LibraryLoader::LoadLibraryW(w!("Msftedit.dll"))
                        .unwrap();
                for class in [w!("Edit"), w!("RICHEDIT50W")] {
                    let window = CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        class,
                        PCWSTR::null(),
                        WS_POPUP,
                        0,
                        0,
                        100,
                        30,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap();
                    let previous = GetFocus();
                    let _ = SetFocus(window);
                    COMPAT_VIEW.with(|v| v.set(window));
                    for private in [false, true] {
                        let value = if private {
                            let scope: IUnknown = TestScope(vec![61]).into();
                            VARIANT::from(scope)
                        } else {
                            VARIANT::default()
                        };
                        let owner: IUnknown = ScopeOwner {
                            value: RefCell::new(value),
                            calls: Rc::new(Cell::new(0)),
                            fail: false,
                        }
                        .into();
                        let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                        let session: ITfEditSession = CompatScopeSession {
                            context: host.context.clone(),
                            expected: if private {
                                Err(ScopeBlockReason::Sensitive)
                            } else {
                                Ok(())
                            },
                        }
                        .into();
                        host.context
                            .RequestEditSession(host.client, &session, TF_ES_SYNC | TF_ES_READWRITE)
                            .unwrap()
                            .ok()
                            .unwrap();
                    }
                    COMPAT_VIEW.with(|v| v.set(Default::default()));
                    let _ = SetFocus(previous);
                    DestroyWindow(window).unwrap();
                }
                windows::Win32::Foundation::FreeLibrary(module).unwrap();
            }
        }

        // Isolated ACP owner: never advises ITfContextOwner or changes the desktop.
        #[test]
        fn native_compat_class_query_bypasses_window_subclass() {
            use windows::core::w;
            use windows::Win32::Foundation::{HWND, LRESULT};
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
            use windows::Win32::UI::WindowsAndMessaging::*;
            thread_local! {
                static QUERY_CALLS: Cell<u32> = const { Cell::new(0) };
                static ORIGINAL: Cell<WNDPROC> = const { Cell::new(None) };
            }
            unsafe extern "system" fn subclass(
                window: HWND,
                message: u32,
                wparam: WPARAM,
                lparam: LPARAM,
            ) -> LRESULT {
                if message == 0xD2 {
                    QUERY_CALLS.with(|v| v.set(v.get() + 1));
                    return LRESULT('*' as isize);
                }
                CallWindowProcW(ORIGINAL.with(Cell::get), window, message, wparam, lparam)
            }
            unsafe {
                let module =
                    windows::Win32::System::LibraryLoader::LoadLibraryW(w!("Msftedit.dll"))
                        .unwrap();
                assert!(!native_password_clear(HWND::default()));
                for class in [w!("Edit"), w!("RICHEDIT50W")] {
                    let window = CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        class,
                        w!(""),
                        WS_POPUP,
                        0,
                        0,
                        100,
                        30,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap();
                    let previous = GetFocus();
                    let _ = SetFocus(window);
                    let original =
                        SetWindowLongPtrW(window, GWLP_WNDPROC, subclass as *const () as isize);
                    assert_ne!(original, 0);
                    ORIGINAL.with(|v| v.set(std::mem::transmute::<isize, WNDPROC>(original)));
                    // Positive control: normal message dispatch hits the
                    // subclass and observes its deliberately false mask.
                    QUERY_CALLS.with(|v| v.set(0));
                    assert_ne!(SendMessageW(window, 0xD2, WPARAM(0), LPARAM(0)).0, 0);
                    assert_eq!(QUERY_CALLS.with(Cell::get), 1);
                    QUERY_CALLS.with(|v| v.set(0));
                    assert!(native_password_clear(window));
                    assert!(native_edit_window(window, || Some(window)));
                    assert_eq!(QUERY_CALLS.with(Cell::get), 0);
                    SendMessageW(window, 0xCC, WPARAM('*' as usize), LPARAM(0));
                    assert!(!native_password_clear(window));
                    assert!(!native_edit_window(window, || Some(window)));
                    assert_eq!(QUERY_CALLS.with(Cell::get), 0);
                    SetWindowLongPtrW(window, GWLP_WNDPROC, original);
                    ORIGINAL.with(|v| v.set(None));
                    let _ = SetFocus(previous);
                    DestroyWindow(window).unwrap();
                    assert!(!native_password_clear(window));
                }
                windows::Win32::Foundation::FreeLibrary(module).unwrap();
            }
        }

        #[test]
        fn native_compat_edit_window_and_policy() {
            use windows::core::w;
            use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
            use windows::Win32::UI::WindowsAndMessaging::*;
            unsafe {
                let edit = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("Edit"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    100,
                    30,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
                let previous = windows::Win32::UI::Input::KeyboardAndMouse::GetFocus();
                let _ = SetFocus(edit);
                assert_eq!(
                    windows::Win32::UI::Input::KeyboardAndMouse::GetFocus(),
                    edit
                );
                assert!(native_edit_window(edit, || Some(edit)));
                assert!(!native_edit_window(edit, || None));
                assert!(!native_edit_window(Default::default(), || Some(edit)));
                assert!(!native_password_clear(windows::Win32::Foundation::HWND(
                    1usize as *mut c_void,
                )));
                let other = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("Static"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    100,
                    30,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
                assert!(!native_edit_window(other, || Some(other)));
                let _ = SetFocus(other);
                assert!(!native_edit_window(other, || Some(other)));
                let _ = SetFocus(edit);
                DestroyWindow(other).unwrap();
                for style in [0x20, 0x800, WS_DISABLED.0] {
                    SetWindowLongW(edit, GWL_STYLE, (WS_POPUP.0 | style) as i32);
                    assert!(!native_edit_window(edit, || Some(edit)));
                }
                SetWindowLongW(edit, GWL_STYLE, WS_POPUP.0 as i32);
                SendMessageW(edit, 0xCC, WPARAM('*' as usize), LPARAM(0));
                assert!(!native_edit_window(edit, || Some(edit)));
                SendMessageW(edit, 0xCC, WPARAM(0), LPARAM(0));
                assert!(native_edit_window(edit, || Some(edit)));
                let _ = SetFocus(previous);
                DestroyWindow(edit).unwrap();
            }
            for reason in [
                ScopeBlockReason::Property,
                ScopeBlockReason::Value,
                ScopeBlockReason::Undeclared,
                ScopeBlockReason::Empty,
            ] {
                assert!(native_scope_fallback_allowed(ScopePolicy::Ai, reason));
                assert!(native_scope_fallback_allowed(ScopePolicy::Learning, reason));
            }
            for reason in [
                ScopeBlockReason::Sensitive,
                ScopeBlockReason::Unsupported,
                ScopeBlockReason::InvalidValue,
                ScopeBlockReason::Interface,
                ScopeBlockReason::Read,
                ScopeBlockReason::InvalidList,
                ScopeBlockReason::Selection,
            ] {
                assert!(!native_scope_fallback_allowed(ScopePolicy::Ai, reason));
                assert!(!native_scope_fallback_allowed(
                    ScopePolicy::Learning,
                    reason
                ));
            }
        }

        mod acp_scope_bridge {
            use super::*;
            include!("ui_element_tests.rs");
            use windows::core::{PCWSTR, PWSTR};
            use windows::Win32::Foundation::E_NOTIMPL;
            use windows::Win32::System::Com::{IDataObject, FORMATETC};
            use windows::Win32::UI::TextServices::*;

            #[implement(ITextStoreACP)]
            struct Store {
                sink: RefCell<Option<ITextStoreACPSink>>,
                locked: Cell<bool>,
                requested: Cell<bool>,
                value: VARIANT,
                trace: Rc<RefCell<Vec<&'static str>>>,
                text: RefCell<Vec<u16>>,
                selection: Cell<(i32, i32)>,
                writable: bool,
                reject_selection: Cell<bool>,
            }
            impl ITextStoreACP_Impl for Store_Impl {
                fn AdviseSink(
                    &self,
                    _: *const GUID,
                    sink: Option<&IUnknown>,
                    _: u32,
                ) -> Result<()> {
                    *self.sink.borrow_mut() = Some(sink.ok_or(Error::from(E_POINTER))?.cast()?);
                    Ok(())
                }
                fn UnadviseSink(&self, _: Option<&IUnknown>) -> Result<()> {
                    self.sink.borrow_mut().take();
                    Ok(())
                }
                fn RequestLock(&self, flags: u32) -> Result<HRESULT> {
                    self.trace.borrow_mut().push("lock");
                    if self.locked.replace(true) {
                        return Ok(TS_E_SYNCHRONOUS);
                    }
                    let Some(sink) = self.sink.borrow().clone() else {
                        self.locked.set(false);
                        return Err(E_UNEXPECTED.into());
                    };
                    let result = unsafe { sink.OnLockGranted(TEXT_STORE_LOCK_FLAGS(flags)) };
                    self.locked.set(false);
                    Ok(result.err().map_or(S_OK, |e| e.code()))
                }
                fn GetStatus(&self) -> Result<TS_STATUS> {
                    Ok(TS_STATUS::default())
                }
                fn QueryInsert(
                    &self,
                    start: i32,
                    end: i32,
                    _: u32,
                    a: *mut i32,
                    b: *mut i32,
                ) -> Result<()> {
                    unsafe {
                        *a = start;
                        *b = end;
                    }
                    Ok(())
                }
                fn GetSelection(
                    &self,
                    _: u32,
                    count: u32,
                    selection: *mut TS_SELECTION_ACP,
                    fetched: *mut u32,
                ) -> Result<()> {
                    self.trace.borrow_mut().push("selection");
                    if !self.locked.get() {
                        return Err(TS_E_NOLOCK.into());
                    }
                    unsafe {
                        *fetched = u32::from(count > 0);
                        if count > 0 {
                            *selection = TS_SELECTION_ACP {
                                acpStart: self.selection.get().0,
                                acpEnd: self.selection.get().1,
                                style: TS_SELECTIONSTYLE {
                                    ase: TS_AE_END,
                                    fInterimChar: FALSE,
                                },
                            };
                        }
                    }
                    Ok(())
                }
                fn SetSelection(
                    &self,
                    count: u32,
                    selection: *const TS_SELECTION_ACP,
                ) -> Result<()> {
                    self.trace.borrow_mut().push("set-selection");
                    if !self.locked.get() {
                        return Err(TS_E_NOLOCK.into());
                    }
                    if !self.writable || self.reject_selection.get() {
                        return Err(E_NOTIMPL.into());
                    }
                    if count != 1 || selection.is_null() {
                        return Err(E_INVALIDARG.into());
                    }
                    let selection = unsafe { &*selection };
                    if selection.acpStart < 0
                        || selection.acpEnd < selection.acpStart
                        || selection.acpEnd as usize > self.text.borrow().len()
                    {
                        return Err(E_INVALIDARG.into());
                    }
                    self.selection.set((selection.acpStart, selection.acpEnd));
                    Ok(())
                }
                fn GetText(
                    &self,
                    start: i32,
                    end: i32,
                    text: PWSTR,
                    capacity: u32,
                    copied: *mut u32,
                    runs: *mut TS_RUNINFO,
                    run_capacity: u32,
                    run_count: *mut u32,
                    next: *mut i32,
                ) -> Result<()> {
                    if !self.locked.get() {
                        return Err(TS_E_NOLOCK.into());
                    }
                    let text_value = self.text.borrow();
                    let end = if end == -1 {
                        text_value.len() as i32
                    } else {
                        end
                    };
                    if start < 0 || end < start || end as usize > text_value.len() {
                        return Err(E_INVALIDARG.into());
                    }
                    let count = ((end - start) as u32).min(capacity);
                    unsafe {
                        if !copied.is_null() {
                            *copied = count;
                        }
                        if count > 0 {
                            ptr::copy_nonoverlapping(
                                text_value.as_ptr().add(start as usize),
                                text.0,
                                count as usize,
                            );
                        }
                        if !next.is_null() {
                            *next = start + count as i32;
                        }
                        if !run_count.is_null() {
                            *run_count = u32::from(run_capacity > 0 && count > 0);
                        }
                        if run_capacity > 0 && count > 0 {
                            *runs = TS_RUNINFO {
                                uCount: count,
                                r#type: TS_RT_PLAIN,
                            };
                        }
                    }
                    Ok(())
                }
                fn SetText(
                    &self,
                    _: u32,
                    start: i32,
                    end: i32,
                    text: &PCWSTR,
                    count: u32,
                ) -> Result<TS_TEXTCHANGE> {
                    self.trace.borrow_mut().push("set-text");
                    if !self.locked.get() {
                        return Err(TS_E_NOLOCK.into());
                    }
                    if !self.writable {
                        return Err(E_NOTIMPL.into());
                    }
                    let mut value = self.text.borrow_mut();
                    if start < 0
                        || end < start
                        || end as usize > value.len()
                        || (count > 0 && text.0.is_null())
                    {
                        return Err(E_INVALIDARG.into());
                    }
                    let inserted = if count == 0 {
                        &[]
                    } else {
                        unsafe { std::slice::from_raw_parts(text.0, count as usize) }
                    };
                    value.splice(start as usize..end as usize, inserted.iter().copied());
                    let new_end = start + count as i32;
                    self.selection.set((new_end, new_end));
                    Ok(TS_TEXTCHANGE {
                        acpStart: start,
                        acpOldEnd: end,
                        acpNewEnd: new_end,
                    })
                }
                fn GetFormattedText(&self, _: i32, _: i32) -> Result<IDataObject> {
                    Err(E_NOTIMPL.into())
                }
                fn GetEmbedded(&self, _: i32, _: *const GUID, _: *const GUID) -> Result<IUnknown> {
                    Err(E_NOTIMPL.into())
                }
                fn QueryInsertEmbedded(&self, _: *const GUID, _: *const FORMATETC) -> Result<BOOL> {
                    Ok(FALSE)
                }
                fn InsertEmbedded(
                    &self,
                    _: u32,
                    _: i32,
                    _: i32,
                    _: Option<&IDataObject>,
                ) -> Result<TS_TEXTCHANGE> {
                    Err(E_NOTIMPL.into())
                }
                fn InsertTextAtSelection(
                    &self,
                    flags: u32,
                    text: &PCWSTR,
                    count: u32,
                    start: *mut i32,
                    end: *mut i32,
                    change: *mut TS_TEXTCHANGE,
                ) -> Result<()> {
                    self.trace.borrow_mut().push("insert");
                    if !self.locked.get() {
                        return Err(TS_E_NOLOCK.into());
                    }
                    if !self.writable {
                        return Err(E_NOTIMPL.into());
                    }
                    let (a, b) = self.selection.get();
                    let delta = if flags & TS_IAS_QUERYONLY != 0 {
                        TS_TEXTCHANGE {
                            acpStart: a,
                            acpOldEnd: b,
                            acpNewEnd: a + count as i32,
                        }
                    } else {
                        self.SetText(0, a, b, text, count)?
                    };
                    unsafe {
                        if !start.is_null() {
                            *start = a;
                        }
                        if !end.is_null() {
                            *end = if flags & TS_IAS_QUERYONLY != 0 {
                                b
                            } else {
                                delta.acpNewEnd
                            };
                        }
                        if !change.is_null() {
                            *change = delta;
                        }
                    }
                    Ok(())
                }
                fn InsertEmbeddedAtSelection(
                    &self,
                    _: u32,
                    _: Option<&IDataObject>,
                    _: *mut i32,
                    _: *mut i32,
                    _: *mut TS_TEXTCHANGE,
                ) -> Result<()> {
                    Err(E_NOTIMPL.into())
                }
                fn RequestSupportedAttrs(
                    &self,
                    _: u32,
                    count: u32,
                    attrs: *const GUID,
                ) -> Result<()> {
                    self.trace.borrow_mut().push("supported");
                    self.requested.set(false);
                    if count > 0 && attrs.is_null() {
                        return Err(E_POINTER.into());
                    }
                    for index in 0..count as usize {
                        if unsafe { *attrs.add(index) } == GUID_PROP_INPUTSCOPE {
                            self.requested.set(true);
                        }
                    }
                    Ok(())
                }
                // Chromium main ui/base/ime/win/tsf_text_store.cc: no document attrs.
                fn RequestAttrsAtPosition(
                    &self,
                    _: i32,
                    _: u32,
                    _: *const GUID,
                    _: u32,
                ) -> Result<()> {
                    self.trace.borrow_mut().push("position");
                    Ok(())
                }
                fn RequestAttrsTransitioningAtPosition(
                    &self,
                    _: i32,
                    _: u32,
                    _: *const GUID,
                    _: u32,
                ) -> Result<()> {
                    self.trace.borrow_mut().push("transition");
                    Ok(())
                }
                fn FindNextAttrTransition(
                    &self,
                    _: i32,
                    _: i32,
                    _: u32,
                    _: *const GUID,
                    _: u32,
                    next: *mut i32,
                    found: *mut BOOL,
                    offset: *mut i32,
                ) -> Result<()> {
                    unsafe {
                        *next = 0;
                        *found = FALSE;
                        *offset = 0;
                    }
                    Ok(())
                }
                fn RetrieveRequestedAttrs(
                    &self,
                    count: u32,
                    attrs: *mut TS_ATTRVAL,
                    fetched: *mut u32,
                ) -> Result<()> {
                    self.trace.borrow_mut().push("retrieve");
                    unsafe {
                        *fetched = 0;
                        if count > 0 && self.requested.replace(false) {
                            attrs.write(TS_ATTRVAL {
                                idAttr: GUID_PROP_INPUTSCOPE,
                                dwOverlapId: 0,
                                varValue: std::mem::ManuallyDrop::new(self.value.clone()),
                            });
                            *fetched = 1;
                        }
                    }
                    Ok(())
                }
                fn GetEndACP(&self) -> Result<i32> {
                    Ok(self.text.borrow().len() as i32)
                }
                fn GetActiveView(&self) -> Result<u32> {
                    Ok(0)
                }
                fn GetACPFromPoint(&self, _: u32, _: *const POINT, _: u32) -> Result<i32> {
                    Err(E_NOTIMPL.into())
                }
                fn GetTextExt(
                    &self,
                    _: u32,
                    _: i32,
                    _: i32,
                    rect: *mut RECT,
                    clipped: *mut BOOL,
                ) -> Result<()> {
                    unsafe {
                        *rect = RECT {
                            left: 0,
                            top: 0,
                            right: 100,
                            bottom: 20,
                        };
                        *clipped = FALSE;
                    }
                    Ok(())
                }
                fn GetScreenExt(&self, _: u32) -> Result<RECT> {
                    Ok(RECT {
                        left: 0,
                        top: 0,
                        right: 100,
                        bottom: 20,
                    })
                }
                fn GetWnd(&self, _: u32) -> Result<windows::Win32::Foundation::HWND> {
                    Ok(Default::default())
                }
            }

            fn writable_document() -> (NativeDocument, ITextStoreACP) {
                let store: ITextStoreACP = Store {
                    sink: RefCell::new(None),
                    locked: Cell::new(false),
                    requested: Cell::new(false),
                    value: VARIANT::default(),
                    trace: Rc::new(RefCell::new(Vec::new())),
                    text: RefCell::new(Vec::new()),
                    selection: Cell::new((0, 0)),
                    writable: true,
                    reject_selection: Cell::new(false),
                }
                .into();
                let manager: ITfThreadMgr =
                    unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) }
                        .unwrap();
                let client = unsafe { manager.Activate() }.unwrap();
                let document = unsafe { manager.CreateDocumentMgr() }.unwrap();
                let mut context = None;
                let mut cookie = 0;
                unsafe { document.CreateContext(client, 0, &store, &mut context, &mut cookie) }
                    .unwrap();
                let host = NativeDocument {
                    manager,
                    document,
                    context: context.unwrap(),
                    client,
                    owner_cookie: None,
                };
                unsafe { host.document.Push(&host.context) }.unwrap();
                unsafe { host.manager.SetFocus(&host.document) }.unwrap();
                (host, store)
            }

            #[test]
            fn native_host_boundary_writable_acp_keys_and_commit() {
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                    .ok()
                    .unwrap();
                let _apartment = Apartment;
                let (host, store) = writable_document();
                let (service, shared) = basic_service(&host);
                let sink: ITfKeyEventSink = service.cast().unwrap();
                for key in b"NIHAO" {
                    send_key(&sink, &host.context, *key);
                }
                assert_eq!(read_document(&host, None), "nihao");
                assert_eq!(read_selection(&host), 5);
                send_key(&sink, &host.context, 0x25);
                assert_eq!(read_selection(&host), 4);
                send_key(&sink, &host.context, 0x20);
                assert_eq!(read_document(&host, None), "你好");
                assert_eq!(read_selection(&host), 2);
                let implementation = unsafe { <ITextStoreACP as AsImpl<Store>>::as_impl(&store) };
                assert_eq!(
                    String::from_utf16(&implementation.text.borrow()).unwrap(),
                    "你好"
                );
                assert_eq!(implementation.selection.get(), (2, 2));
                assert!(implementation.trace.borrow().contains(&"set-text"));
                assert!(implementation.trace.borrow().contains(&"set-selection"));
                assert!(!implementation.locked.get());
                shared.borrow_mut().take();
            }

            #[test]
            fn native_host_boundary_acp_lock_and_selection_rejection() {
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                    .ok()
                    .unwrap();
                let _apartment = Apartment;
                let (host, store) = writable_document();
                let implementation = unsafe { <ITextStoreACP as AsImpl<Store>>::as_impl(&store) };
                let (service, shared) = basic_service(&host);
                let sink: ITfKeyEventSink = service.cast().unwrap();
                implementation.locked.set(true);
                assert_eq!(
                    unsafe { store.RequestLock(TS_LF_SYNC) }.unwrap(),
                    TS_E_SYNCHRONOUS
                );
                let denied =
                    unsafe { sink.OnKeyDown(&host.context, WPARAM(b'N' as usize), LPARAM(0)) };
                assert!(
                    !denied.as_ref().is_ok_and(|handled| handled.as_bool()),
                    "denied lock cannot consume input"
                );
                implementation.locked.set(false);
                assert!(implementation.text.borrow().is_empty());
                assert!(!shared.borrow().as_ref().unwrap().processing);
                implementation.reject_selection.set(true);
                let session: ITfEditSession = RejectSelectionProbe {
                    context: host.context.clone(),
                }
                .into();
                let started = Instant::now();
                let status = unsafe {
                    host.context.RequestEditSession(
                        host.client,
                        &session,
                        TF_ES_SYNC | TF_ES_READWRITE,
                    )
                }
                .unwrap();
                assert_eq!(
                    status, E_NOTIMPL,
                    "original selection HRESULT must reach the edit caller"
                );
                assert!(started.elapsed() < Duration::from_secs(2));
                let denied =
                    unsafe { sink.OnKeyDown(&host.context, WPARAM(b'N' as usize), LPARAM(0)) };
                assert!(
                    !denied.as_ref().is_ok_and(|handled| handled.as_bool()),
                    "selection failure cannot be reported as handled"
                );
                assert!(!shared.borrow().as_ref().unwrap().processing);
                assert!(!implementation.locked.get());
                assert!(implementation.trace.borrow().contains(&"set-selection"));
                shared.borrow_mut().take();
            }

            #[implement(ITfEditSession)]
            struct RejectSelectionProbe {
                context: ITfContext,
            }
            impl ITfEditSession_Impl for RejectSelectionProbe_Impl {
                fn DoEditSession(&self, cookie: u32) -> Result<()> {
                    let mut host = TfCompositionHost::new(self.context.clone(), None, cookie);
                    host.begin("n", Some(1))
                }
            }

            #[implement(ITfEditSession)]
            struct Probe {
                context: ITfContext,
                expected: Option<Vec<i32>>,
                trace: Rc<RefCell<Vec<&'static str>>>,
                ran: Rc<Cell<bool>>,
            }
            impl ITfEditSession_Impl for Probe_Impl {
                fn DoEditSession(&self, cookie: u32) -> Result<()> {
                    self.ran.set(true);
                    unsafe {
                        let mut selection = [TF_SELECTION::default()];
                        let mut fetched = 0;
                        let selected = self.context.GetSelection(
                            cookie,
                            u32::MAX,
                            &mut selection,
                            &mut fetched,
                        );
                        eprintln!("ACP selection={selected:?} fetched={fetched}");
                        selected?;
                        assert_eq!(fetched, 1);
                        let range = std::mem::ManuallyDrop::take(&mut selection[0].range)
                            .ok_or(Error::from(E_UNEXPECTED))?;
                        for app in [true, false] {
                            let value = if app {
                                self.context
                                    .GetAppProperty(&GUID_PROP_INPUTSCOPE)
                                    .and_then(|p| p.GetValue(cookie, &range))
                            } else {
                                self.context
                                    .GetProperty(&GUID_PROP_INPUTSCOPE)
                                    .and_then(|p| p.GetValue(cookie, &range))
                            };
                            eprintln!(
                                "ACP app={app} value={value:?} trace={:?}",
                                self.trace.borrow()
                            );
                            if !app {
                                // Observed native document channel result; do not confuse this
                                // with the successful application-attribute bridge above.
                                assert_eq!(
                                    value.unwrap_err().code(),
                                    windows::Win32::Foundation::E_FAIL
                                );
                                continue;
                            }
                            let value = value?;
                            let decoded = IUnknown::try_from(&value)
                                .and_then(|v| v.cast::<ITfInputScope>())
                                .and_then(|scope| {
                                    let mut data = ptr::null_mut();
                                    let mut count = 0;
                                    scope.GetInputScopes(&mut data, &mut count)?;
                                    let values = if count == 0 {
                                        vec![]
                                    } else {
                                        std::slice::from_raw_parts(data, count as usize)
                                            .iter()
                                            .map(|v| v.0)
                                            .collect()
                                    };
                                    windows::Win32::System::Com::CoTaskMemFree(Some(data.cast()));
                                    Ok(values)
                                });
                            eprintln!("ACP app={app} scopes={decoded:?}");
                            match &self.expected {
                                Some(expected) => assert_eq!(&decoded?, expected),
                                None => assert!(
                                    decoded.is_err(),
                                    "VT_EMPTY must not decode as input scope"
                                ),
                            }
                        }
                        let allowed = scope_allows_learning(&self.context, cookie);
                        eprintln!("ACP production_scope_allows_learning={allowed}");
                        let expected_learning = self.expected.as_ref().is_some_and(|s| {
                            !s.is_empty() && s.iter().all(|s| learning_scope_value_is_safe(*s))
                        });
                        assert_eq!(allowed, expected_learning);
                        let expected_ai = match &self.expected {
                            None => Err(ScopeBlockReason::Undeclared),
                            Some(s) if s.is_empty() => Err(ScopeBlockReason::Empty),
                            Some(s) => scope_policy(
                                &s.iter().copied().map(InputScope).collect::<Vec<_>>(),
                                ScopePolicy::Ai,
                            ),
                        };
                        assert_eq!(
                            check_scope(&self.context, cookie, ScopePolicy::Ai),
                            expected_ai
                        );
                    }
                    Ok(())
                }
            }

            #[test]
            fn native_acp_scope_bridge_chromium_isolated() {
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                    .ok()
                    .unwrap();
                let _apartment = Apartment;
                for scopes in [
                    Some(vec![IS_DEFAULT.0]),
                    Some(vec![IS_SEARCH.0]),
                    Some(vec![IS_SEARCH_INCREMENTAL.0]),
                    Some(vec![IS_CHINESE_HALFWIDTH.0]),
                    Some(vec![IS_CHINESE_FULLWIDTH.0]),
                    Some(vec![IS_NATIVE_SCRIPT.0]),
                    Some(vec![IS_TEXT.0]),
                    Some(vec![IS_CHAT.0]),
                    Some(vec![IS_CHAT_WITHOUT_EMOJI.0]),
                    Some(vec![31]),
                    Some(vec![61]),
                    Some(vec![63]),
                    Some(vec![64]),
                    Some(vec![65]),
                    Some(vec![66]),
                    Some(vec![0, 31]),
                    Some(vec![999, 61]),
                    Some(vec![999]),
                    Some(vec![]),
                    None,
                ] {
                    eprintln!("ACP case={scopes:?}");
                    let trace = Rc::new(RefCell::new(Vec::new()));
                    let value = scopes.as_ref().map_or_else(VARIANT::default, |s| {
                        let scope: IUnknown = TestScope(s.clone()).into();
                        VARIANT::from(scope)
                    });
                    let store: IUnknown = Store {
                        sink: RefCell::new(None),
                        locked: Cell::new(false),
                        requested: Cell::new(false),
                        value,
                        trace: trace.clone(),
                        text: RefCell::new(vec![b'x' as u16]),
                        selection: Cell::new((0, 0)),
                        writable: false,
                        reject_selection: Cell::new(false),
                    }
                    .into();
                    // Do not use with_owner: its ITfContextOwner advice is a different bridge.
                    let manager: ITfThreadMgr = unsafe {
                        CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER)
                    }
                    .unwrap();
                    let client = unsafe { manager.Activate() }.unwrap();
                    let document = unsafe { manager.CreateDocumentMgr() }.unwrap();
                    let mut context = None;
                    let mut cookie = 0;
                    unsafe { document.CreateContext(client, 0, &store, &mut context, &mut cookie) }
                        .unwrap();
                    let context = context.unwrap();
                    let host = NativeDocument {
                        manager,
                        document,
                        context,
                        client,
                        owner_cookie: None,
                    };
                    unsafe { host.document.Push(&host.context) }.unwrap();
                    unsafe { host.manager.SetFocus(&host.document) }.unwrap();
                    let ran = Rc::new(Cell::new(false));
                    let session: ITfEditSession = Probe {
                        context: host.context.clone(),
                        expected: scopes,
                        trace: trace.clone(),
                        ran: ran.clone(),
                    }
                    .into();
                    let result = unsafe {
                        host.context
                            .RequestEditSession(client, &session, TF_ES_SYNC | TF_ES_READ)
                    };
                    eprintln!(
                        "ACP request={result:?} ran={} trace={:?}",
                        ran.get(),
                        trace.borrow()
                    );
                    result.unwrap().ok().unwrap();
                    assert!(ran.get(), "real TSF edit callback must run");
                    assert!(trace.borrow().contains(&"supported"));
                    assert!(trace.borrow().contains(&"retrieve"));
                    assert!(trace.borrow().contains(&"selection"));
                }
            }
        }

        #[implement(ITfInputScope)]
        struct TestScope(Vec<i32>);

        impl ITfInputScope_Impl for TestScope_Impl {
            fn GetInputScopes(&self, values: *mut *mut InputScope, count: *mut u32) -> Result<()> {
                unsafe {
                    *count = self.0.len() as u32;
                    *values = windows::Win32::System::Com::CoTaskMemAlloc(
                        self.0.len().max(1) * std::mem::size_of::<InputScope>(),
                    )
                    .cast();
                    if (*values).is_null() {
                        return Err(E_UNEXPECTED.into());
                    }
                    for (index, value) in self.0.iter().enumerate() {
                        (*values).add(index).write(InputScope(*value));
                    }
                }
                Ok(())
            }
            fn GetPhrase(&self, _: *mut *mut BSTR, _: *mut u32) -> Result<()> {
                Err(E_UNEXPECTED.into())
            }
            fn GetRegularExpression(&self) -> Result<BSTR> {
                Err(E_UNEXPECTED.into())
            }
            fn GetSRGS(&self) -> Result<BSTR> {
                Err(E_UNEXPECTED.into())
            }
            fn GetXML(&self) -> Result<BSTR> {
                Err(E_UNEXPECTED.into())
            }
        }

        #[implement(ITfInputScope)]
        struct BrokenScope {
            returned_count: u32,
            allocate: bool,
            fail: bool,
        }
        impl ITfInputScope_Impl for BrokenScope_Impl {
            fn GetInputScopes(&self, values: *mut *mut InputScope, count: *mut u32) -> Result<()> {
                unsafe {
                    *count = self.returned_count;
                    // Only one initialized item: production must never slice an oversized
                    // or failed response, but must free even these hostile out parameters.
                    *values = if self.allocate {
                        let p = windows::Win32::System::Com::CoTaskMemAlloc(std::mem::size_of::<
                            InputScope,
                        >(
                        ))
                        .cast::<InputScope>();
                        if p.is_null() {
                            return Err(E_UNEXPECTED.into());
                        }
                        p.write(InputScope(0));
                        p
                    } else {
                        ptr::null_mut()
                    };
                }
                if self.fail {
                    Err(E_UNEXPECTED.into())
                } else {
                    Ok(())
                }
            }
            fn GetPhrase(&self, _: *mut *mut BSTR, _: *mut u32) -> Result<()> {
                Err(E_UNEXPECTED.into())
            }
            fn GetRegularExpression(&self) -> Result<BSTR> {
                Err(E_UNEXPECTED.into())
            }
            fn GetSRGS(&self) -> Result<BSTR> {
                Err(E_UNEXPECTED.into())
            }
            fn GetXML(&self) -> Result<BSTR> {
                Err(E_UNEXPECTED.into())
            }
        }

        #[test]
        fn native_ai_scope_invalid_com_responses() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            let mut cases = vec![
                (VARIANT::from(42i32), ScopeBlockReason::InvalidValue),
                (VARIANT::default(), ScopeBlockReason::Undeclared),
            ];
            let unknown: IUnknown = ClassFactory::new().into();
            cases.push((VARIANT::from(unknown), ScopeBlockReason::Interface));
            // A correctly tagged null IUnknown is not a scope interface.
            let mut raw = *VARIANT::default().as_raw();
            raw.Anonymous.Anonymous.vt = 13;
            cases.push((
                unsafe { VARIANT::from_raw(raw) },
                ScopeBlockReason::InvalidValue,
            ));
            for (count, allocate, fail, reason) in [
                (1, true, true, ScopeBlockReason::Read),
                (257, true, true, ScopeBlockReason::Read),
                (257, true, false, ScopeBlockReason::InvalidList),
                (1, false, false, ScopeBlockReason::InvalidList),
                (0, true, false, ScopeBlockReason::Empty),
                (0, false, false, ScopeBlockReason::Empty),
            ] {
                let scope: IUnknown = BrokenScope {
                    returned_count: count,
                    allocate,
                    fail,
                }
                .into();
                cases.push((VARIANT::from(scope), reason));
            }
            for (value, reason) in cases {
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(value),
                    calls: calls.clone(),
                    fail: false,
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (_service, shared) = ai_fixture(&host, &capture, &calls);
                tick_ai(&shared);
                pump_ai(&shared);
                assert_eq!(
                    ai_status(shared.borrow().as_ref().unwrap()),
                    AiStatus::ScopeBlocked(reason)
                );
                assert!(capture.requests.borrow().is_empty());
                drop(shared.borrow_mut().take());
            }
        }

        #[implement(ITfContextOwner)]
        struct ScopeOwner {
            value: RefCell<VARIANT>,
            calls: Rc<Cell<u32>>,
            fail: bool,
        }

        impl ITfContextOwner_Impl for ScopeOwner_Impl {
            fn GetACPFromPoint(&self, _: *const POINT, _: u32) -> Result<i32> {
                Ok(0)
            }
            fn GetTextExt(
                &self,
                _: i32,
                _: i32,
                rect: *mut RECT,
                clipped: *mut BOOL,
            ) -> Result<()> {
                unsafe {
                    *rect = self.GetScreenExt()?;
                    *clipped = FALSE;
                }
                Ok(())
            }
            fn GetScreenExt(&self) -> Result<RECT> {
                Ok(RECT {
                    left: 0,
                    top: 0,
                    right: 100,
                    bottom: 20,
                })
            }
            fn GetStatus(&self) -> Result<TS_STATUS> {
                Ok(TS_STATUS::default())
            }
            fn GetWnd(&self) -> Result<windows::Win32::Foundation::HWND> {
                Ok(COMPAT_VIEW.with(Cell::get))
            }
            fn GetAttribute(&self, guid: *const GUID) -> Result<VARIANT> {
                if unsafe { *guid } != GUID_PROP_INPUTSCOPE {
                    return Err(E_INVALIDARG.into());
                }
                self.calls.set(self.calls.get() + 1);
                if self.fail {
                    return Err(E_UNEXPECTED.into());
                }
                Ok(self.value.borrow().clone())
            }
        }

        #[implement(ITfEditSession)]
        struct CompareScope {
            context: ITfContext,
            calls: Rc<Cell<u32>>,
        }
        impl ITfEditSession_Impl for CompareScope_Impl {
            fn DoEditSession(&self, cookie: u32) -> Result<()> {
                unsafe {
                    fn status<T>(stage: &str, result: &Result<T>, calls: u32) {
                        eprintln!(
                            "stage={stage} hr=0x{:08X} owner_calls={calls}",
                            result.as_ref().err().map_or(0, |e| e.code().0 as u32)
                        );
                    }
                    let mut selection = [TF_SELECTION::default()];
                    let mut fetched = 0;
                    let selected =
                        self.context
                            .GetSelection(cookie, u32::MAX, &mut selection, &mut fetched);
                    status("selection", &selected, self.calls.get());
                    let _range = std::mem::ManuallyDrop::take(&mut selection[0].range);
                    let start = self.context.GetStart(cookie);
                    status("start", &start, self.calls.get());
                    let range = start?;
                    range.SetText(cookie, 0, &[b'x' as u16])?;
                    let range = self.context.GetStart(cookie)?;
                    let end = self.context.GetEnd(cookie)?;
                    range.ShiftEndToRange(cookie, &end, TF_ANCHOR_END)?;
                    let property = self.context.GetProperty(&GUID_PROP_INPUTSCOPE);
                    status("document_property", &property, self.calls.get());
                    let document = property.and_then(|p| p.GetValue(cookie, &range));
                    status("document_value", &document, self.calls.get());
                    assert_eq!(self.calls.get(), 0, "document channel must not query owner");
                    let property = self.context.GetAppProperty(&GUID_PROP_INPUTSCOPE);
                    status("app_property", &property, self.calls.get());
                    let app = property.and_then(|p| p.GetValue(cookie, &range));
                    status("app_value", &app, self.calls.get());
                    let app = app?;
                    let scope: ITfInputScope = IUnknown::try_from(&app)?.cast()?;
                    assert!(
                        self.calls.get() > 0,
                        "application property must query owner"
                    );
                    let mut values = std::ptr::null_mut();
                    let mut count = 0;
                    scope.GetInputScopes(&mut values, &mut count)?;
                    assert_eq!(count, 1);
                    assert_eq!((*values).0, 0);
                    windows::Win32::System::Com::CoTaskMemFree(Some(values.cast()));
                }
                Ok(())
            }
        }

        #[test]
        fn native_scope_commit_publishes_rankings_and_worker() {
            use crate::engine::{Key, PinyinSession};
            use windows::core::w;
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
            use windows::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, WINDOW_EX_STYLE,
                WNDCLASSW, WS_POPUP,
            };
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            let custom_class = w!("FlowerLearningCompatibilityTest");
            unsafe extern "system" fn custom_proc(
                window: windows::Win32::Foundation::HWND,
                message: u32,
                wparam: WPARAM,
                lparam: LPARAM,
            ) -> windows::Win32::Foundation::LRESULT {
                DefWindowProcW(window, message, wparam, lparam)
            }
            let class = WNDCLASSW {
                lpfnWndProc: Some(custom_proc),
                lpszClassName: custom_class,
                ..Default::default()
            };
            assert_ne!(unsafe { RegisterClassW(&class) }, 0);
            let rich =
                unsafe { windows::Win32::System::LibraryLoader::LoadLibraryW(w!("Msftedit.dll")) }
                    .unwrap();
            let root = std::path::PathBuf::from(
                std::env::var_os("FLOWER_TEST_SCRATCH")
                    .expect("native learning test requires an explicitly isolated scratch root"),
            );
            assert!(root.is_absolute());
            for (index, (scopes, fail, allowed)) in [
                (Some(vec![0]), false, true),
                (Some(vec![57]), false, true),
                (Some(vec![58]), false, true),
                (Some(vec![3]), false, true),
                (Some(vec![50]), false, true),
                (Some(vec![52]), false, true),
                (Some(vec![53]), false, true),
                (Some(vec![54]), false, true),
                (Some(vec![55]), false, true),
                (Some(vec![68]), false, true),
                (Some(vec![31]), false, false),
                (Some(vec![61]), false, false),
                (Some(vec![0, 31]), false, false),
                (Some(vec![999]), false, false),
                (Some(vec![]), false, true),
                (None, false, true),
                (Some(vec![0]), true, true),
                (Some(vec![0]), false, false), // adaptive ranking explicitly disabled
                (Some(vec![0]), false, false), // learning disabled, with no worker
                (None, false, true),           // custom S04, AI remains unavailable
                (Some(vec![]), false, true),   // custom S08
                (None, false, false),          // compatibility explicitly disabled
                (Some(vec![]), false, false),  // compatibility explicitly disabled
                (Some(vec![31]), false, false), // custom password
                (Some(vec![61]), false, false), // custom private
                (Some(vec![999]), false, false), // custom unsupported
                (Some(vec![0]), true, false),  // custom failed property
                (None, false, false),          // custom focus identity lost
            ]
            .into_iter()
            .enumerate()
            {
                eprintln!("native learning scope case {index}");
                let value = scopes.map_or_else(VARIANT::default, |s| {
                    let scope: IUnknown = TestScope(s).into();
                    VARIANT::from(scope)
                });
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(value),
                    fail,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let window = unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        if index >= 19 {
                            custom_class
                        } else if index % 2 == 0 {
                            w!("Edit")
                        } else {
                            w!("RICHEDIT50W")
                        },
                        w!(""),
                        WS_POPUP,
                        0,
                        0,
                        100,
                        30,
                        None,
                        None,
                        None,
                        None,
                    )
                }
                .unwrap();
                let previous = unsafe { GetFocus() };
                let _ = unsafe { SetFocus(window) };
                COMPAT_VIEW.with(|v| v.set(window));
                if index == 27 {
                    let _ = unsafe { SetFocus(previous) };
                }
                unsafe { host.manager.SetFocus(&host.document) }.unwrap();
                assert_eq!(
                    focused_context_key(&host.manager),
                    Some(context_key(&host.context))
                );
                let path = root.join(format!(
                    "native-learning-{index}-{}-{}.json",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                assert!(!path.exists(), "test never reuses an existing store");
                let store = DictionaryStore::new(&path);
                let data = store.load().unwrap();
                let service: ITfTextInputProcessor = TextService::new().into();
                let implementation =
                    unsafe { <ITfTextInputProcessor as AsImpl<TextService>>::as_impl(&service) };
                *implementation.activation.borrow_mut() = Some(Activation {
                    ui: Rc::new(UiState::default()),
                    _activation_flags: 0,
                    thread_manager: host.manager.clone(),
                    keystroke_manager: host.manager.cast().unwrap(),
                    client_id: host.client,
                    contexts: HashMap::new(),
                    token: next_activation_token(),
                    owner_thread: std::thread::current().id(),
                    focus_epoch: 0,
                    foreground: true,
                    shown_context: None,
                    processing: false,
                    english: false,
                    shift: ShiftGesture::default(),
                    context_generation: Rc::new(Cell::new(0)),
                    _thread_events: None,
                    settings: Settings {
                        ai_correction_enabled: index < 19,
                        ai_undeclared_scope_compatibility: !matches!(index, 21 | 22),
                        ..Settings::default()
                    },
                    lexicon: Lexicon::builtin().unwrap(),
                    rankings: Arc::new(Vec::new()),
                    adaptive_ranking_enabled: !matches!(index, 17 | 18),
                    learning: if index == 18 {
                        None
                    } else {
                        Some(LearningWorker::start(store, data.generation).unwrap())
                    },
                    _learning_registration: None,
                    local_data_error: false,
                    ai: None,
                    ai_unavailable: AiStatus::Unavailable,
                });
                let sink: ITfKeyEventSink = service.cast().unwrap();
                for key in b"WENTI" {
                    send_key(&sink, &host.context, *key);
                }
                send_key(&sink, &host.context, 0x1B);
                assert!(implementation
                    .activation
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .rankings
                    .is_empty());
                assert_eq!(read_document(&host, None), "");
                for key in b"WENTI" {
                    send_key(&sink, &host.context, *key);
                }
                // An actual TSF write with no valid edit cookie must fail and
                // must not produce the successful outcome needed for publication.
                let rejected: ITfEditSession = {
                    let state = implementation.activation.borrow();
                    let entry = &state.as_ref().unwrap().contexts[&context_key(&host.context)];
                    let mut router = entry.router.clone();
                    EditSession::new(
                        host.context.clone(),
                        router.select_candidate(1).unwrap(),
                        entry.composition.clone(),
                        entry.controller,
                    )
                    .into()
                };
                assert!(unsafe { rejected.DoEditSession(u32::MAX) }.is_err());
                assert!(unsafe {
                    <ITfEditSession as AsImpl<EditSession>>::as_impl(&rejected).take_outcome()
                }
                .is_none());
                assert!(implementation
                    .activation
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .rankings
                    .is_empty());
                match index % 4 {
                    0 => send_key(&sink, &host.context, b'2'),
                    1 => {
                        send_key(&sink, &host.context, 0x27);
                        send_key(&sink, &host.context, 0x0D);
                    }
                    2 => {
                        send_key(&sink, &host.context, 0x27);
                        send_key(&sink, &host.context, b' ');
                    }
                    _ => {
                        // Exercise the production mouse-action acceptance/TSF path.
                        // Geometry and successful-frame authorization live in popup.rs;
                        // this test begins at its emitted revision-bearing action,
                        // including the popup's shown-context publication.
                        implementation
                            .activation
                            .borrow_mut()
                            .as_mut()
                            .unwrap()
                            .shown_context = Some(context_key(&host.context));
                        let (token, thread, snapshot) = {
                            let state = implementation.activation.borrow();
                            let active = state.as_ref().unwrap();
                            (
                                active.token,
                                active.owner_thread,
                                active.contexts[&context_key(&host.context)]
                                    .popup_model
                                    .snapshot()
                                    .unwrap()
                                    .clone(),
                            )
                        };
                        implementation
                            .activation
                            .borrow_mut()
                            .as_mut()
                            .unwrap()
                            .foreground = false;
                        dispatch_popup_action(
                            &implementation.activation,
                            token,
                            thread,
                            CandidatePopupAction::Select {
                                revision: snapshot.revision,
                                page: snapshot.page,
                                index: 1,
                            },
                        );
                        assert!(implementation
                            .activation
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .rankings
                            .is_empty());
                        implementation
                            .activation
                            .borrow_mut()
                            .as_mut()
                            .unwrap()
                            .foreground = true;
                        dispatch_popup_action(
                            &implementation.activation,
                            token,
                            thread,
                            CandidatePopupAction::Select {
                                revision: snapshot.revision,
                                page: snapshot.page,
                                index: 1,
                            },
                        );
                        let deadline = Instant::now() + Duration::from_secs(2);
                        while implementation
                            .activation
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .processing
                        {
                            use windows::Win32::UI::WindowsAndMessaging::{
                                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
                            };
                            assert!(
                                Instant::now() < deadline,
                                "mouse edit session did not finish"
                            );
                            let mut message = MSG::default();
                            unsafe {
                                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                                    let _ = TranslateMessage(&message);
                                    DispatchMessageW(&message);
                                }
                            }
                            std::thread::yield_now();
                        }
                    }
                }
                assert_eq!(read_document(&host, None), "文体");
                assert!(calls.get() > 0);
                let orders = implementation
                    .activation
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .rankings
                    .clone();
                assert_eq!(!orders.is_empty(), allowed, "scope case {index}");
                if allowed {
                    assert_eq!(orders[0].candidates[0].text, "文体");
                    assert_eq!(orders[0].candidates[0].pinyin, "wen'ti");
                    assert_eq!(orders[0].candidates[0].consumed, 5);
                }
                for key in b"WENTI " {
                    send_key(&sink, &host.context, *key);
                }
                assert_eq!(
                    read_document(&host, None),
                    if allowed {
                        "文体文体"
                    } else {
                        "文体问题"
                    }
                );
                // A newly created TSF context must inherit the activation's orders,
                // not the stale snapshot of the context that committed the word.
                let mut fresh = None;
                let mut cookie = 0;
                let fresh_document = unsafe { host.manager.CreateDocumentMgr() }.unwrap();
                unsafe {
                    fresh_document
                        .CreateContext(host.client, 0, Some(&owner), &mut fresh, &mut cookie)
                        .unwrap();
                }
                let fresh = fresh.unwrap();
                let source: windows::Win32::UI::TextServices::ITfSource = fresh.cast().unwrap();
                let advice = unsafe { source.AdviseSink(&ITfContextOwner::IID, &owner) }.unwrap();
                unsafe {
                    fresh_document.Push(&fresh).unwrap();
                    host.manager.SetFocus(&fresh_document).unwrap();
                }
                for key in b"WENTI" {
                    send_key(&sink, &fresh, *key);
                }
                {
                    let state = implementation.activation.borrow();
                    let entry = &state.as_ref().unwrap().contexts[&context_key(&fresh)];
                    assert_eq!(
                        entry.popup_model.snapshot().unwrap().candidates[0].text,
                        if allowed { "文体" } else { "问题" },
                    );
                }
                send_key(&sink, &fresh, 0x1B);
                unsafe {
                    source.UnadviseSink(advice).unwrap();
                    fresh_document.Pop(TF_POPF_ALL).unwrap();
                    host.manager.SetFocus(&host.document).unwrap();
                }
                let mut active = implementation.activation.borrow_mut().take().unwrap();
                assert!(!active.local_data_error);
                if let Some(worker) = active.learning.as_mut() {
                    worker.shutdown().unwrap();
                }
                drop(active);
                let persisted = DictionaryStore::new(&path).load().unwrap();
                assert_eq!(!persisted.rankings.is_empty(), allowed);
                if allowed {
                    assert_eq!(persisted.rankings[0].candidates[0].text, "文体");
                    let mut restarted =
                        PinyinSession::from_lexicon(Lexicon::builtin().unwrap(), 9).unwrap();
                    restarted.set_ranking_orders(Arc::new(persisted.rankings));
                    for letter in "wenti".chars() {
                        restarted
                            .process_key(KeyEvent::plain(Key::Char(letter)))
                            .unwrap();
                    }
                    assert_eq!(restarted.candidates_on_current_page()[0].text, "文体");
                }
                COMPAT_VIEW.with(|v| v.set(Default::default()));
                let _ = unsafe { SetFocus(previous) };
                unsafe { DestroyWindow(window) }.unwrap();
            }
            unsafe { windows::Win32::Foundation::FreeLibrary(rich) }.unwrap();
        }

        #[test]
        fn native_owner_application_scope_path() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            let calls = Rc::new(Cell::new(0));
            let scope: IUnknown = TestScope(vec![0]).into();
            let owner: IUnknown = ScopeOwner {
                value: RefCell::new(VARIANT::from(scope)),
                calls: calls.clone(),
                fail: false,
            }
            .into();
            let host = NativeDocument::with_owner(Some(&owner)).unwrap();
            let session: ITfEditSession = CompareScope {
                context: host.context.clone(),
                calls,
            }
            .into();
            unsafe {
                host.context
                    .RequestEditSession(host.client, &session, TF_ES_SYNC | TF_ES_READWRITE)
            }
            .unwrap()
            .ok()
            .unwrap();
        }

        #[implement(ITfEditSession)]
        struct ReadDocument {
            context: ITfContext,
            terminate: Option<ActiveComposition>,
            text: Rc<RefCell<String>>,
        }

        impl ITfEditSession_Impl for ReadDocument_Impl {
            fn DoEditSession(&self, cookie: u32) -> Result<()> {
                if let Some(composition) = &self.terminate {
                    let owner: ITfContextOwnerCompositionServices = self.context.cast()?;
                    let view: ITfCompositionView = composition.handle.cast()?;
                    unsafe { owner.TerminateComposition(&view) }?;
                }
                let range = unsafe { self.context.GetStart(cookie) }?;
                let end = unsafe { self.context.GetEnd(cookie) }?;
                unsafe { range.ShiftEndToRange(cookie, &end, TF_ANCHOR_END) }?;
                let mut buffer = [0u16; 512];
                let mut fetched = 0;
                unsafe { range.GetText(cookie, 0, &mut buffer, &mut fetched) }?;
                *self.text.borrow_mut() = String::from_utf16(&buffer[..fetched as usize])
                    .map_err(|_| Error::from(E_UNEXPECTED))?;
                Ok(())
            }
        }

        fn read_document(host: &NativeDocument, terminate: Option<ActiveComposition>) -> String {
            let flags = if terminate.is_some() {
                TF_ES_READWRITE
            } else {
                TF_ES_READ
            };
            let text = Rc::new(RefCell::new(String::new()));
            let session: ITfEditSession = ReadDocument {
                context: host.context.clone(),
                terminate,
                text: Rc::clone(&text),
            }
            .into();
            unsafe {
                host.context
                    .RequestEditSession(host.client, &session, TF_ES_SYNC | flags)
            }
            .unwrap()
            .ok()
            .unwrap();
            let result = text.borrow().clone();
            result
        }

        fn send_key(sink: &ITfKeyEventSink, context: &ITfContext, key: u8) {
            assert!(
                unsafe { sink.OnTestKeyDown(context, WPARAM(key as usize), LPARAM(0)) }
                    .unwrap()
                    .as_bool(),
                "native TSF prediction for key {key:#x} must succeed"
            );
            assert!(
                unsafe { sink.OnKeyDown(context, WPARAM(key as usize), LPARAM(0)) }
                    .unwrap()
                    .as_bool(),
                "native TSF key {key:#x} must apply successfully"
            );
        }

        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }

        struct NativeDocument {
            manager: ITfThreadMgr,
            document: ITfDocumentMgr,
            context: ITfContext,
            client: u32,
            owner_cookie: Option<u32>,
        }

        impl NativeDocument {
            fn new() -> Result<Self> {
                Self::with_owner(None)
            }

            fn with_owner(owner: Option<&IUnknown>) -> Result<Self> {
                let manager: ITfThreadMgr =
                    unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) }?;
                let client = unsafe { manager.Activate() }?;
                let document = unsafe { manager.CreateDocumentMgr() }?;
                let mut context = None;
                let mut cookie = 0;
                unsafe {
                    document.CreateContext(client, 0, owner, &mut context, &mut cookie)?;
                }
                let context = context.ok_or_else(|| Error::from(E_UNEXPECTED))?;
                let owner_cookie = if let Some(owner) = owner {
                    let source: windows::Win32::UI::TextServices::ITfSource = context.cast()?;
                    Some(unsafe { source.AdviseSink(&ITfContextOwner::IID, owner) }?)
                } else {
                    None
                };
                unsafe {
                    document.Push(&context)?;
                    manager.SetFocus(&document)?;
                }
                Ok(Self {
                    manager,
                    document,
                    context,
                    client,
                    owner_cookie,
                })
            }
        }

        impl Drop for NativeDocument {
            fn drop(&mut self) {
                unsafe {
                    if let Some(cookie) = self.owner_cookie {
                        if let Ok(source) = self
                            .context
                            .cast::<windows::Win32::UI::TextServices::ITfSource>()
                        {
                            let _ = source.UnadviseSink(cookie);
                        }
                    }
                    let _ = self.manager.SetFocus(None::<&ITfDocumentMgr>);
                    let _ = self.document.Pop(TF_POPF_ALL);
                    let _ = self.manager.Deactivate();
                }
            }
        }

        #[derive(Default)]
        struct CapturedAi {
            requests: RefCell<Vec<Request>>,
            response: RefCell<Option<Response>>,
            stopped: Cell<bool>,
        }

        struct CapturingTransport {
            capture: Rc<CapturedAi>,
            scope_calls: Rc<Cell<u32>>,
        }

        impl AiTransport for CapturingTransport {
            fn submit(&self, request: Request) -> bool {
                assert!(
                    self.scope_calls.get() > 0,
                    "submit requires a real scope query"
                );
                self.capture.requests.borrow_mut().push(request);
                true
            }
            fn try_result(&self) -> Option<Response> {
                self.capture.response.borrow_mut().take()
            }
            fn shutdown(&mut self) {
                self.capture.stopped.set(true);
            }
        }

        fn ai_fixture(
            host: &NativeDocument,
            capture: &Rc<CapturedAi>,
            calls: &Rc<Cell<u32>>,
        ) -> (ITfTextInputProcessor, SharedActivation) {
            let service: ITfTextInputProcessor = TextService::new().into();
            let implementation =
                unsafe { <ITfTextInputProcessor as AsImpl<TextService>>::as_impl(&service) };
            let shared = implementation.activation.clone();
            let generation = Rc::new(Cell::new(0));
            let events = ThreadEvents::subscribe(&host.manager, generation.clone()).unwrap();
            *shared.borrow_mut() = Some(Activation {
                ui: Rc::new(UiState::default()),
                _activation_flags: 0,
                thread_manager: host.manager.clone(),
                keystroke_manager: host.manager.cast().unwrap(),
                client_id: host.client,
                contexts: HashMap::new(),
                token: next_activation_token(),
                owner_thread: std::thread::current().id(),
                focus_epoch: 0,
                foreground: true,
                shown_context: None,
                processing: false,
                english: false,
                shift: ShiftGesture::default(),
                context_generation: generation,
                _thread_events: Some(events),
                settings: Settings::default(),
                lexicon: Lexicon::from_tsv("jin'tian'xin'qing\t今天新情\t100").unwrap(),
                rankings: Arc::new(Vec::new()),
                adaptive_ranking_enabled: true,
                learning: None,
                _learning_registration: None,
                local_data_error: false,
                ai: Some(AiScheduler::new(Box::new(CapturingTransport {
                    capture: capture.clone(),
                    scope_calls: calls.clone(),
                }))),
                ai_unavailable: AiStatus::Unavailable,
            });
            let sink: ITfKeyEventSink = service.cast().unwrap();
            for key in b"JINTIANXINQING" {
                send_key(&sink, &host.context, *key);
            }
            {
                let mut state = shared.borrow_mut();
                let active = state.as_mut().unwrap();
                // No window is required for this owned document fixture.
                active.shown_context = Some(context_key(&host.context));
                let ai = active.ai.as_mut().unwrap();
                ai.last_input = Instant::now() - Duration::from_millis(300);
                assert!(!ai.attempted);
            }
            (service, shared)
        }

        fn tick_ai(shared: &SharedActivation) {
            let (token, owner) = {
                let state = shared.borrow();
                let active = state.as_ref().unwrap();
                (active.token, active.owner_thread)
            };
            ai_idle(shared, token, owner);
        }

        fn pump_ai(shared: &SharedActivation) {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let reading = shared
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .ai
                    .as_ref()
                    .unwrap()
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.reading);
                if !reading {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "TSF did not deliver the asynchronous read session"
                );
                let mut message = MSG::default();
                unsafe {
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                std::thread::yield_now();
            }
        }

        fn captured_reply(capture: &CapturedAi) {
            let id = capture.requests.borrow().last().unwrap().id;
            *capture.response.borrow_mut() = Some(Response {
                reading: String::new(),
                id,
                corrected: Some("今天心情".into()),
            });
        }

        #[test]
        fn native_ai_real_timer_reports_scope_no_suggestion_and_slow_result() {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
            };
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            for case in 0..3 {
                let scope: IUnknown = TestScope(vec![if case == 0 { 31 } else { 0 }]).into();
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(VARIANT::from(scope)),
                    fail: false,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (_service, shared) = ai_fixture(&host, &capture, &calls);
                let (token, thread) = {
                    let state = shared.borrow();
                    let active = state.as_ref().unwrap();
                    (active.token, active.owner_thread)
                };
                struct PopupGuard(ActivationToken, ThreadId);
                impl Drop for PopupGuard {
                    fn drop(&mut self) {
                        close_thread_popup(self.0, self.1);
                    }
                }
                let mut popup = CandidatePopupWindow::new().unwrap();
                let weak = Rc::downgrade(&shared);
                popup
                    .set_idle_sink(Some(Rc::new(move || {
                        if let Some(shared) = weak.upgrade() {
                            ai_idle(&shared, token, thread);
                        }
                    })))
                    .unwrap();
                assert!(register_thread_popup(token, popup));
                let _popup_guard = PopupGuard(token, thread);
                let before = read_document(&host, None);
                let mut replied = false;
                let expected = [
                    AiStatus::ScopeBlocked(ScopeBlockReason::Sensitive),
                    AiStatus::NoSuggestion,
                    AiStatus::Suggested,
                ][case];
                let deadline = Instant::now() + Duration::from_secs(3);
                // This loop dispatches real WM_TIMER messages; it never calls tick_ai.
                while ai_status(shared.borrow().as_ref().unwrap()) != expected {
                    assert!(Instant::now() < deadline, "timer status case {case}");
                    let mut message = MSG::default();
                    unsafe {
                        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                    if case != 0 && !replied && !capture.requests.borrow().is_empty() {
                        if case == 2 {
                            shared
                                .borrow_mut()
                                .as_mut()
                                .unwrap()
                                .ai
                                .as_mut()
                                .unwrap()
                                .pending
                                .as_mut()
                                .unwrap()
                                .dispatched = Some(Instant::now() - Duration::from_millis(1200));
                            captured_reply(&capture);
                        } else {
                            *capture.response.borrow_mut() = Some(Response {
                                reading: String::new(),
                                id: capture.requests.borrow().last().unwrap().id,
                                corrected: None,
                            });
                        }
                        replied = true;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(capture.requests.borrow().len(), usize::from(case != 0));
                assert_eq!(
                    read_document(&host, None),
                    before,
                    "timer never writes preedit"
                );
                if case == 2 {
                    assert!(shared
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .contexts
                        .get(&context_key(&host.context))
                        .unwrap()
                        .popup_model
                        .snapshot()
                        .unwrap()
                        .candidates[1]
                        .is_ai());
                }
                shared
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .ai
                    .as_mut()
                    .unwrap()
                    .input();
                assert_eq!(
                    ai_status(shared.borrow().as_ref().unwrap()),
                    AiStatus::Waiting
                );
                if case == 1 {
                    shared
                        .borrow_mut()
                        .as_mut()
                        .unwrap()
                        .ai
                        .as_mut()
                        .unwrap()
                        .last_input =
                        Instant::now() - Duration::from_millis(crate::ai::DEBOUNCE_MS + 1);
                    tick_ai(&shared);
                    pump_ai(&shared);
                    {
                        let mut state = shared.borrow_mut();
                        let pending = state
                            .as_mut()
                            .unwrap()
                            .ai
                            .as_mut()
                            .unwrap()
                            .pending
                            .as_mut()
                            .unwrap();
                        // Model an accepted read session that has not delivered
                        // its cookie: no source is retained or sent by that gate.
                        pending.dispatched = None;
                        pending.input = None;
                        pending.reading = true;
                        pending.queued = Instant::now()
                            - Duration::from_millis(crate::ai::RESULT_TIMEOUT_MS + 1);
                    }
                    tick_ai(&shared);
                    assert_eq!(
                        ai_status(shared.borrow().as_ref().unwrap()),
                        AiStatus::EditFailed
                    );
                    assert!(shared
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .ai
                        .as_ref()
                        .unwrap()
                        .pending
                        .is_none());
                }
                shared
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .settings
                    .ai_correction_enabled = false;
                assert_eq!(
                    ai_status(shared.borrow().as_ref().unwrap()),
                    AiStatus::Disabled
                );
                shared
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .settings
                    .ai_correction_enabled = true;
                shared.borrow_mut().as_mut().unwrap().ai = None;
                assert_eq!(
                    ai_status(shared.borrow().as_ref().unwrap()),
                    AiStatus::Unavailable
                );
                drop(shared.borrow_mut().take());
            }
        }

        #[test]
        fn native_ai_late_read_callbacks_enforce_their_own_deadlines() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            for publication in [false, true] {
                let scope: IUnknown = TestScope(vec![0]).into();
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(VARIANT::from(scope)),
                    fail: false,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (_service, shared) = ai_fixture(&host, &capture, &calls);
                tick_ai(&shared);
                pump_ai(&shared);
                let ticket = {
                    let mut state = shared.borrow_mut();
                    let pending = state
                        .as_mut()
                        .unwrap()
                        .ai
                        .as_mut()
                        .unwrap()
                        .pending
                        .as_mut()
                        .unwrap();
                    let past =
                        Instant::now() - Duration::from_millis(crate::ai::RESULT_TIMEOUT_MS + 1);
                    pending.reading = true;
                    if publication {
                        pending.dispatched = Some(past);
                    } else {
                        pending.queued = past;
                        pending.dispatched = None;
                        pending.input = None;
                    }
                    pending.ticket.clone()
                };
                capture.requests.borrow_mut().clear();
                let session: ITfEditSession = AiReadSession {
                    activation: Rc::downgrade(&shared),
                    ticket: ticket.clone(),
                    context: host.context.clone(),
                    manager: host.manager.clone(),
                    response: publication.then(|| Response {
                        reading: String::new(),
                        id: ticket.id,
                        corrected: Some("今天心情".into()),
                    }),
                    _registration: ObjectRegistration::new(),
                }
                .into();
                let hr = unsafe {
                    host.context
                        .RequestEditSession(host.client, &session, TF_ES_ASYNC | TF_ES_READ)
                }
                .unwrap();
                assert!(hr.is_ok());
                pump_ai(&shared); // No timer tick can clean pending first.
                assert!(capture.requests.borrow().is_empty());
                assert_eq!(
                    ai_status(shared.borrow().as_ref().unwrap()),
                    if publication {
                        AiStatus::Failed
                    } else {
                        AiStatus::EditFailed
                    }
                );
                assert!(shared
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .ai
                    .as_ref()
                    .unwrap()
                    .pending
                    .is_none());
                assert_eq!(
                    shared
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .contexts
                        .get(&context_key(&host.context))
                        .unwrap()
                        .popup_model
                        .snapshot()
                        .unwrap()
                        .candidates
                        .len(),
                    1
                );
                drop(shared.borrow_mut().take());
            }
        }

        #[test]
        fn native_ai_read_scope_matrix_and_source() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            use windows::Win32::UI::TextServices::*;
            use ScopeBlockReason::*;
            for (scopes, fail, expected_reason) in [
                (Some(vec![IS_DEFAULT.0]), false, None),
                (Some(vec![IS_TEXT.0]), false, None),
                (Some(vec![IS_CHAT.0]), false, None),
                (Some(vec![IS_SEARCH.0]), false, None),
                (Some(vec![IS_SEARCH_INCREMENTAL.0]), false, None),
                (Some(vec![IS_CHINESE_HALFWIDTH.0]), false, None),
                (Some(vec![IS_CHINESE_FULLWIDTH.0]), false, None),
                (Some(vec![IS_NATIVE_SCRIPT.0]), false, None),
                (Some(vec![IS_CHAT_WITHOUT_EMOJI.0]), false, None),
                (Some(vec![0, 57, 58]), false, None),
                (Some(vec![31]), false, Some(Sensitive)),
                (Some(vec![61]), false, Some(Sensitive)),
                (Some(vec![63]), false, Some(Sensitive)),
                (Some(vec![64]), false, Some(Sensitive)),
                (Some(vec![65]), false, Some(Sensitive)),
                (Some(vec![66]), false, Some(Sensitive)),
                (Some(vec![0, 31]), false, Some(Sensitive)),
                (Some(vec![999, 61]), false, Some(Sensitive)),
                (Some(vec![999]), false, Some(Unsupported)),
                (Some(vec![]), false, Some(Empty)),
                (None, false, Some(Undeclared)),
                (Some(vec![0]), true, Some(Value)),
            ] {
                let allowed = expected_reason.is_none();
                let value = scopes.map_or_else(VARIANT::default, |s| {
                    let scope: IUnknown = TestScope(s).into();
                    VARIANT::from(scope)
                });
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(value),
                    fail,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (service, shared) = ai_fixture(&host, &capture, &calls);
                assert!(capture.requests.borrow().is_empty());
                assert_eq!(calls.get(), 0);
                tick_ai(&shared);
                pump_ai(&shared);
                assert!(calls.get() > 0);
                assert_eq!(capture.requests.borrow().len(), usize::from(allowed));
                assert_eq!(
                    ai_status(shared.borrow().as_ref().unwrap()),
                    if allowed {
                        AiStatus::Checking
                    } else {
                        AiStatus::ScopeBlocked(expected_reason.unwrap())
                    }
                );
                if allowed {
                    assert_eq!(capture.requests.borrow()[0].text, "今天新情");
                    let raw = shared
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .contexts
                        .get(&context_key(&host.context))
                        .unwrap()
                        .router
                        .ai_input()
                        .unwrap()
                        .raw;
                    assert_eq!(raw, "jintianxinqing");
                }
                let active = shared.borrow_mut().take();
                drop(active);
                assert!(capture.stopped.get());
                drop(service);
            }
        }

        #[test]
        fn native_ai_publication_rechecks_privacy_and_selects_without_learning() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            for sensitive in [true, false] {
                let scope: IUnknown = TestScope(vec![0]).into();
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(VARIANT::from(scope)),
                    fail: false,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (service, shared) = ai_fixture(&host, &capture, &calls);
                tick_ai(&shared);
                pump_ai(&shared);
                assert_eq!(capture.requests.borrow().len(), 1);
                let before = read_document(&host, None);
                let previous_calls = calls.get();
                if sensitive {
                    let scope: IUnknown = TestScope(vec![31]).into();
                    let owner_interface: ITfContextOwner = owner.cast().unwrap();
                    let implementation = unsafe {
                        <ITfContextOwner as AsImpl<ScopeOwner>>::as_impl(&owner_interface)
                    };
                    *implementation.value.borrow_mut() = VARIANT::from(scope);
                }
                captured_reply(&capture);
                tick_ai(&shared);
                pump_ai(&shared);
                assert!(calls.get() > previous_calls);
                assert_eq!(
                    read_document(&host, None),
                    before,
                    "AI must not write preedit"
                );
                let snapshot = shared
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .contexts
                    .get(&context_key(&host.context))
                    .unwrap()
                    .popup_model
                    .snapshot()
                    .unwrap()
                    .clone();
                assert_eq!(snapshot.candidates.len(), if sensitive { 1 } else { 2 });
                assert_eq!(
                    ai_status(shared.borrow().as_ref().unwrap()),
                    if sensitive {
                        AiStatus::ScopeBlocked(ScopeBlockReason::Sensitive)
                    } else {
                        AiStatus::Suggested
                    }
                );
                if !sensitive {
                    captured_reply(&capture);
                    tick_ai(&shared);
                    pump_ai(&shared);
                    assert_eq!(
                        shared
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .contexts
                            .get(&context_key(&host.context))
                            .unwrap()
                            .popup_model
                            .snapshot()
                            .unwrap()
                            .revision,
                        snapshot.revision,
                        "a duplicate result must not republish"
                    );
                    let sink: ITfKeyEventSink = service.cast().unwrap();
                    send_key(&sink, &host.context, b'2');
                    assert_eq!(read_document(&host, None), "今天心情");
                    let state = shared.borrow();
                    let active = state.as_ref().unwrap();
                    assert!(active.rankings.is_empty());
                    assert!(active
                        .contexts
                        .get(&context_key(&host.context))
                        .unwrap()
                        .router
                        .pending_promotions()
                        .is_empty());
                }
                drop(shared.borrow_mut().take());
                drop(service);
            }
        }

        #[test]
        fn native_builtin_spelling_suggestions_commit_without_learning() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            for (raw, gold) in [("xuexo", "学习"), ("xueerunishuixingzhou", "学如逆水行舟")]
            {
                let scope: IUnknown = TestScope(vec![0]).into();
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(VARIANT::from(scope)),
                    fail: false,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (service, shared) = ai_fixture(&host, &capture, &calls);
                let sink: ITfKeyEventSink = service.cast().unwrap();
                send_key(&sink, &host.context, 0x1B);
                {
                    let mut state = shared.borrow_mut();
                    let active = state.as_mut().unwrap();
                    active.contexts.clear();
                    active.lexicon = Lexicon::builtin().unwrap();
                }
                for key in raw.bytes() {
                    send_key(&sink, &host.context, key.to_ascii_uppercase());
                }
                let (index, rankings) = {
                    let state = shared.borrow();
                    let active = state.as_ref().unwrap();
                    let snapshot = active.contexts[&context_key(&host.context)]
                        .popup_model
                        .snapshot()
                        .unwrap();
                    let index = snapshot
                        .candidates
                        .iter()
                        .position(|c| c.text == gold)
                        .unwrap();
                    assert!(snapshot.candidates[index].is_spelling());
                    assert!(!snapshot.candidates[index].is_ai());
                    assert_ne!(read_document(&host, None), gold);
                    (index, active.rankings.clone())
                };
                send_key(&sink, &host.context, b'1' + index as u8);
                assert_eq!(read_document(&host, None), gold);
                {
                    let state = shared.borrow();
                    let active = state.as_ref().unwrap();
                    let router = &active.contexts[&context_key(&host.context)].router;
                    assert!(router.pending_learning().is_empty());
                    assert!(router.pending_promotions().is_empty());
                    assert_eq!(active.rankings, rankings);
                }
                drop(shared.borrow_mut().take());
                drop(service);
            }
        }

        #[test]
        fn native_ai_builtin_spelling_real_timer_response_proof_and_commit() {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_TIMER,
            };
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            for accepted in [true, false] {
                let scope: IUnknown = TestScope(vec![0]).into();
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(VARIANT::from(scope)),
                    fail: false,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (service, shared) = ai_fixture(&host, &capture, &calls);
                let sink: ITfKeyEventSink = service.cast().unwrap();
                send_key(&sink, &host.context, 0x1B);
                {
                    let mut state = shared.borrow_mut();
                    let active = state.as_mut().unwrap();
                    active.contexts.clear();
                    active.lexicon = Lexicon::builtin().unwrap();
                }
                for key in b"XUANZAIZAINALISHANGBAN" {
                    send_key(&sink, &host.context, *key);
                }
                let (token, thread, original) = {
                    let mut state = shared.borrow_mut();
                    let active = state.as_mut().unwrap();
                    active.shown_context = Some(context_key(&host.context));
                    active.ai.as_mut().unwrap().last_input =
                        Instant::now() - Duration::from_millis(300);
                    let snapshot = active.contexts[&context_key(&host.context)]
                        .popup_model
                        .snapshot()
                        .unwrap()
                        .clone();
                    assert_eq!(snapshot.candidates[0].text, "选在在哪里上班");
                    (active.token, active.owner_thread, snapshot)
                };
                struct PopupGuard(ActivationToken, ThreadId);
                impl Drop for PopupGuard {
                    fn drop(&mut self) {
                        close_thread_popup(self.0, self.1);
                    }
                }
                let mut popup = CandidatePopupWindow::new().unwrap();
                let weak = Rc::downgrade(&shared);
                popup
                    .set_idle_sink(Some(Rc::new(move || {
                        if let Some(shared) = weak.upgrade() {
                            ai_idle(&shared, token, thread);
                        }
                    })))
                    .unwrap();
                assert!(register_thread_popup(token, popup));
                let _popup_guard = PopupGuard(token, thread);
                let before = read_document(&host, None);
                let initial_rankings = shared.borrow().as_ref().unwrap().rankings.clone();
                let mut replied = false;
                let mut timers = 0;
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    assert!(
                        Instant::now() < deadline,
                        "spelling timer response accepted={accepted}"
                    );
                    let mut message = MSG::default();
                    unsafe {
                        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                            timers += usize::from(message.message == WM_TIMER);
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                    if !replied && !capture.requests.borrow().is_empty() {
                        let requests = capture.requests.borrow();
                        assert_eq!(requests.len(), 1);
                        let request = &requests[0];
                        assert_eq!(request.text, "选在在哪里上班");
                        assert_eq!(request.raw, "xuanzaizainalishangban");
                        assert_eq!(request.reading, "xuan'zai'zai'na'li'shang'ban");
                        *capture.response.borrow_mut() = Some(Response {
                            id: request.id,
                            corrected: Some(
                                if accepted {
                                    "现在在哪里上班"
                                } else {
                                    // A valid first repair cannot authorize an unrelated 哪→那.
                                    "现在在那里上班"
                                }
                                .into(),
                            ),
                            reading: "xian'zai'zai'na'li'shang'ban".into(),
                        });
                        replied = true;
                    }
                    if replied
                        && shared
                            .borrow()
                            .as_ref()
                            .unwrap()
                            .ai
                            .as_ref()
                            .unwrap()
                            .pending
                            .is_none()
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(timers > 0, "only real WM_TIMER dispatch drives this test");
                assert_eq!(capture.requests.borrow().len(), 1);
                assert_eq!(
                    read_document(&host, None),
                    before,
                    "AI publication is read-only"
                );
                {
                    let state = shared.borrow();
                    let active = state.as_ref().unwrap();
                    let snapshot = active.contexts[&context_key(&host.context)]
                        .popup_model
                        .snapshot()
                        .unwrap();
                    assert_eq!(snapshot.candidates[0], original.candidates[0]);
                    if accepted {
                        assert_eq!(snapshot.candidates[1].text, "现在在哪里上班");
                        assert!(snapshot.candidates[1].is_ai());
                        assert_eq!(ai_status(active), AiStatus::Suggested);
                    } else {
                        assert_eq!(snapshot, &original);
                        assert!(snapshot.candidates.iter().all(|c| !c.is_ai()));
                    }
                }
                if accepted {
                    send_key(&sink, &host.context, b'2');
                    assert_eq!(read_document(&host, None), "现在在哪里上班");
                }
                {
                    let state = shared.borrow();
                    let active = state.as_ref().unwrap();
                    let router = &active.contexts[&context_key(&host.context)].router;
                    assert!(router.pending_learning().is_empty());
                    assert!(router.pending_promotions().is_empty());
                    assert_eq!(active.rankings, initial_rankings);
                }
                if !accepted {
                    assert_eq!(
                        read_document(&host, None),
                        before,
                        "rejected proof must not commit"
                    );
                    send_key(&sink, &host.context, 0x1B);
                    assert_eq!(read_document(&host, None), "");
                }
                drop(shared.borrow_mut().take());
                drop(service);
            }
        }

        #[test]
        #[ignore = "requires FLOWER_AI_TEST_HELPER and FLOWER_AI_TEST_PIPE for a verified packaged helper; isolated TSF, no installation"]
        fn native_ai_builtin_packaged_real_timer_model_and_commit() {
            use std::os::windows::ffi::OsStrExt;
            use std::process::{Child, Command, Stdio};
            use windows::core::PCWSTR;
            use windows::Win32::System::Pipes::WaitNamedPipeW;
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_TIMER,
            };
            struct Broker(Child);
            impl Drop for Broker {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            // Observe the production transport without fabricating or changing any payload.
            struct ObservedClient {
                client: crate::ai::ipc::AiClient,
                requests: Rc<RefCell<Vec<Request>>>,
                responses: Rc<RefCell<Vec<Response>>>,
                calls: Rc<Cell<u32>>,
            }
            impl AiTransport for ObservedClient {
                fn submit(&self, request: Request) -> bool {
                    assert!(
                        self.calls.get() > 0,
                        "real read-cookie scope query required"
                    );
                    if !self.client.submit(request.clone()) {
                        return false;
                    }
                    println!(
                        "request id={} raw={} source={} reading={}",
                        request.id, request.raw, request.text, request.reading
                    );
                    self.requests.borrow_mut().push(request);
                    true
                }
                fn try_result(&self) -> Option<Response> {
                    let response = self.client.try_result()?;
                    println!(
                        "response id={} corrected={:?} reading={}",
                        response.id, response.corrected, response.reading
                    );
                    self.responses.borrow_mut().push(response.clone());
                    Some(response)
                }
                fn shutdown(&mut self) {
                    self.client.shutdown();
                }
            }
            struct PopupGuard(ActivationToken, ThreadId);
            impl Drop for PopupGuard {
                fn drop(&mut self) {
                    close_thread_popup(self.0, self.1);
                }
            }
            fn pump_for(duration: Duration, timers: &mut usize) {
                let until = Instant::now() + duration;
                loop {
                    let mut message = MSG::default();
                    unsafe {
                        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                            *timers += usize::from(message.message == WM_TIMER);
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                    if Instant::now() >= until {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let helper = std::path::PathBuf::from(
                std::env::var_os("FLOWER_AI_TEST_HELPER")
                    .expect("explicit verified helper required"),
            );
            assert!(helper.is_absolute());
            let pipe = std::env::var_os("FLOWER_AI_TEST_PIPE")
                .expect("explicit canonical helper/version/SID pipe required");
            let wide: Vec<u16> = pipe.encode_wide().chain(Some(0)).collect();
            // Refuse an already running endpoint: never borrow or stop somebody else's broker.
            assert!(
                !unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 1) }.as_bool(),
                "endpoint already occupied"
            );
            let mut broker = Broker(
                Command::new(&helper)
                    .arg("--ai-server")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            while !unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 50) }.as_bool() {
                assert!(
                    broker.0.try_wait().unwrap().is_none(),
                    "owned broker exited"
                );
                assert!(Instant::now() < deadline, "owned broker did not bind");
                std::thread::sleep(Duration::from_millis(5));
            }
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            let builtin = Lexicon::builtin().unwrap();
            // Allows reusing the already established fast/completed-pause evidence.
            let overlap_only = std::env::var_os("FLOWER_AI_TEST_OVERLAP_ONLY").is_some();
            for (slow, overlap) in [(false, true), (false, false), (true, false)] {
                if overlap_only && !overlap {
                    continue;
                }
                for (raw, source, expected) in [
                    (
                        "xuanzaizainalishangban",
                        "选在在哪里上班",
                        Some("现在在哪里上班"),
                    ),
                    (
                        "xainzaizainalishangban",
                        "喜爱内在在哪里上班",
                        Some("现在在哪里上班"),
                    ),
                    (
                        "xiaanzaizainalishangban",
                        "下岸在在哪里上班",
                        Some("现在在哪里上班"),
                    ),
                    (
                        "xinzaizainalishangban",
                        "新在在哪里上班",
                        Some("现在在哪里上班"),
                    ),
                    ("xianzaizainalishangban", "现在在哪里上班", None),
                    ("womenxuanzainalijianmian", "我们选在哪里见面", None),
                ] {
                    if overlap
                        && ![
                            "xuanzaizainalishangban",
                            "xainzaizainalishangban",
                            "xiaanzaizainalishangban",
                        ]
                        .contains(&raw)
                    {
                        continue;
                    }
                    println!("case slow={slow} overlap={overlap} raw={raw}");
                    let scope: IUnknown = TestScope(vec![0]).into();
                    let calls = Rc::new(Cell::new(0));
                    let owner: IUnknown = ScopeOwner {
                        value: RefCell::new(VARIANT::from(scope)),
                        fail: false,
                        calls: calls.clone(),
                    }
                    .into();
                    let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                    let capture = Rc::new(CapturedAi::default());
                    let (service, shared) = ai_fixture(&host, &capture, &calls);
                    let sink: ITfKeyEventSink = service.cast().unwrap();
                    send_key(&sink, &host.context, 0x1B);
                    let requests = Rc::new(RefCell::new(Vec::new()));
                    let responses = Rc::new(RefCell::new(Vec::new()));
                    let (token, thread) = {
                        let mut state = shared.borrow_mut();
                        let active = state.as_mut().unwrap();
                        active.contexts.clear();
                        active.lexicon = builtin.clone();
                        active.ai = Some(AiScheduler::new(Box::new(ObservedClient {
                            client: crate::ai::ipc::AiClient::start(helper.clone()).unwrap(),
                            requests: requests.clone(),
                            responses: responses.clone(),
                            calls: calls.clone(),
                        })));
                        (active.token, active.owner_thread)
                    };
                    let mut popup = CandidatePopupWindow::new().unwrap();
                    let weak = Rc::downgrade(&shared);
                    popup
                        .set_idle_sink(Some(Rc::new(move || {
                            if let Some(shared) = weak.upgrade() {
                                ai_idle(&shared, token, thread);
                            }
                        })))
                        .unwrap();
                    assert!(register_thread_popup(token, popup));
                    let _popup_guard = PopupGuard(token, thread);
                    let mut timers = 0;
                    for (index, key) in raw.bytes().enumerate() {
                        send_key(&sink, &host.context, key.to_ascii_uppercase());
                        shared.borrow_mut().as_mut().unwrap().shown_context =
                            Some(context_key(&host.context));
                        if overlap && index == 10 {
                            let deadline = Instant::now() + Duration::from_secs(7);
                            // Stop on the exact dispatch that submitted the prefix, rather
                            // than draining messages until a response might also be polled.
                            while requests.borrow().is_empty() {
                                assert!(Instant::now() < deadline, "prefix dispatch missing");
                                let mut message = MSG::default();
                                if unsafe {
                                    PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool()
                                } {
                                    timers += usize::from(message.message == WM_TIMER);
                                    unsafe {
                                        let _ = TranslateMessage(&message);
                                        DispatchMessageW(&message);
                                    }
                                } else {
                                    std::thread::yield_now();
                                }
                            }
                            assert_eq!(requests.borrow().len(), 1);
                            assert!(responses.borrow().is_empty(), "prefix already delivered");
                            {
                                let state = shared.borrow();
                                let ai = state.as_ref().unwrap().ai.as_ref().unwrap();
                                let pending =
                                    ai.pending.as_ref().expect("prefix must be in flight");
                                assert!(pending.dispatched.is_some());
                                assert_eq!(ai.status, AiStatus::Checking);
                                println!(
                                    "overlap prefix_id={} responses=0 dispatched=true status={:?}",
                                    requests.borrow()[0].id,
                                    ai.status
                                );
                            }
                            // No further pump or wait: immediately deliver remaining keys.
                        } else if slow && index == 10 {
                            // A real pause, not a backdated scheduler timestamp.
                            let deadline = Instant::now() + Duration::from_secs(7);
                            while requests.borrow().is_empty()
                                || shared
                                    .borrow()
                                    .as_ref()
                                    .unwrap()
                                    .ai
                                    .as_ref()
                                    .unwrap()
                                    .pending
                                    .is_some()
                            {
                                assert!(
                                    Instant::now() < deadline,
                                    "mid-input dispatch/completion missing"
                                );
                                pump_for(Duration::from_millis(5), &mut timers);
                            }
                            println!(
                                "mid-input status={:?} responses={}",
                                ai_status(shared.borrow().as_ref().unwrap()),
                                responses.borrow().len()
                            );
                        }
                    }
                    let before = read_document(&host, None);
                    let deadline = Instant::now() + Duration::from_secs(7);
                    loop {
                        pump_for(Duration::from_millis(5), &mut timers);
                        let complete = requests.borrow().last().is_some_and(|r| r.raw == raw)
                            && shared
                                .borrow()
                                .as_ref()
                                .unwrap()
                                .ai
                                .as_ref()
                                .unwrap()
                                .pending
                                .is_none();
                        if complete {
                            break;
                        }
                        assert!(Instant::now() < deadline,
                            "final request/publication missing raw={raw} requests={} responses={} status={:?}",
                            requests.borrow().len(), responses.borrow().len(),
                            ai_status(shared.borrow().as_ref().unwrap()));
                    }
                    assert!(timers > 0);
                    assert!(calls.get() > 0);
                    let request = requests.borrow().last().unwrap().clone();
                    assert_eq!(request.raw, raw);
                    assert_eq!(request.text, source);
                    assert!(!request.reading.is_empty());
                    if slow || overlap {
                        assert!(
                            requests.borrow().iter().any(|r| r.raw != raw),
                            "pause must actually dispatch a different prefix request"
                        );
                        if overlap {
                            assert_eq!(request.id, 2, "final request must replace prefix id=1");
                        }
                    } else {
                        assert_eq!(requests.borrow().len(), 1);
                    }
                    let response = responses
                        .borrow()
                        .iter()
                        .find(|r| r.id == request.id)
                        .expect("real final response")
                        .clone();
                    assert_eq!(response.corrected.as_deref(), expected);
                    assert_eq!(
                        read_document(&host, None),
                        before,
                        "publication is read-only"
                    );
                    {
                        let state = shared.borrow();
                        let active = state.as_ref().unwrap();
                        println!("final status={:?} timers={timers}", ai_status(active));
                        let snapshot = active.contexts[&context_key(&host.context)]
                            .popup_model
                            .snapshot()
                            .unwrap();
                        assert_eq!(snapshot.candidates[0].text, source);
                        if let Some(text) = expected {
                            assert_eq!(ai_status(active), AiStatus::Suggested);
                            assert_eq!(snapshot.candidates[1].text, text);
                            assert!(snapshot.candidates[1].is_ai());
                        } else {
                            assert_eq!(ai_status(active), AiStatus::NoSuggestion);
                            assert!(snapshot.candidates.iter().all(|c| !c.is_ai()));
                        }
                    }
                    if let Some(text) = expected {
                        send_key(&sink, &host.context, b'2');
                        assert_eq!(read_document(&host, None), text);
                        println!("digit2_commit={text}");
                    } else {
                        send_key(&sink, &host.context, 0x1B);
                        assert_eq!(read_document(&host, None), "");
                    }
                    {
                        let state = shared.borrow();
                        let active = state.as_ref().unwrap();
                        let router = &active.contexts[&context_key(&host.context)].router;
                        assert!(router.pending_learning().is_empty());
                        assert!(router.pending_promotions().is_empty());
                        assert!(active.rankings.is_empty());
                    }
                    drop(shared.borrow_mut().take()); // production shutdown joins client
                    drop(service);
                }
            }
            broker.0.kill().unwrap();
            println!("owned_broker_exit_observed={}", broker.0.wait().unwrap());
        }

        #[test]
        fn native_ai_stale_tickets_and_deadline() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            for case in 0..9 {
                let scope: IUnknown = TestScope(vec![0]).into();
                let calls = Rc::new(Cell::new(0));
                let owner: IUnknown = ScopeOwner {
                    value: RefCell::new(VARIANT::from(scope)),
                    fail: false,
                    calls: calls.clone(),
                }
                .into();
                let host = NativeDocument::with_owner(Some(&owner)).unwrap();
                let capture = Rc::new(CapturedAi::default());
                let (service, shared) = ai_fixture(&host, &capture, &calls);
                tick_ai(&shared);
                pump_ai(&shared);
                assert_eq!(capture.requests.borrow().len(), 1);
                captured_reply(&capture);
                match case {
                    0 => {
                        let sink: ITfKeyEventSink = service.cast().unwrap();
                        send_key(&sink, &host.context, 0x1b); // cancel
                    }
                    1 => {
                        let sink: ITfKeyEventSink = service.cast().unwrap();
                        send_key(&sink, &host.context, b'N');
                    }
                    2 | 8 => {
                        // SetFocus(NULL) can succeed while TSF still reports
                        // the previous document. Empty that owned document
                        // to establish an observable missing top context.
                        // Detach notifications to exercise timer-side cleanup.
                        let events = shared.borrow_mut().as_mut().unwrap()._thread_events.take();
                        drop(events);
                        unsafe { host.manager.SetFocus(None::<&ITfDocumentMgr>) }.unwrap();
                        unsafe { host.document.Pop(TF_POPF_ALL) }.unwrap();
                        assert!(
                            focused_context_key(&host.manager).is_none(),
                            "the focused empty document must have no top context"
                        );
                        if case == 8 {
                            *capture.response.borrow_mut() = None;
                        }
                    }
                    3 => {
                        let mut state = shared.borrow_mut();
                        let ai = state.as_mut().unwrap().ai.as_mut().unwrap();
                        ai.pending.as_mut().unwrap().dispatched = Some(
                            Instant::now()
                                - Duration::from_millis(crate::ai::RESULT_TIMEOUT_MS + 1),
                        );
                    }
                    4 => {
                        let mut state = shared.borrow_mut();
                        let entry = state
                            .as_mut()
                            .unwrap()
                            .contexts
                            .get_mut(&context_key(&host.context))
                            .unwrap();
                        let update = entry.router.set_candidate_width(Some(480));
                        entry.popup_model.apply_update(&update);
                    }
                    5 => {
                        let generation =
                            shared.borrow().as_ref().unwrap().context_generation.clone();
                        let before = generation.get();
                        unsafe {
                            host.document.Pop(TF_POPF_ALL).unwrap();
                            host.document.Push(&host.context).unwrap();
                        }
                        assert!(
                            generation.get() > before,
                            "real Push/Pop must invalidate the ticket"
                        );
                    }
                    6 => {
                        let mut response = capture.response.borrow_mut();
                        response.as_mut().unwrap().id += 1;
                    }
                    7 => {
                        let sink: ITfKeyEventSink = service.cast().unwrap();
                        let _ = unsafe {
                            sink.OnKeyDown(&host.context, WPARAM(VK_SHIFT.0 as usize), LPARAM(0))
                        }
                        .unwrap();
                    }
                    _ => unreachable!(),
                }
                tick_ai(&shared);
                pump_ai(&shared);
                assert!(
                    shared
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .ai
                        .as_ref()
                        .unwrap()
                        .pending
                        .is_none(),
                    "stale case {case} must release its pending source"
                );
                assert!(
                    shared
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .contexts
                        .values()
                        .all(|entry| {
                            entry
                                .popup_model
                                .snapshot()
                                .map_or(true, |s| s.candidates.iter().all(|c| c.text != "今天心情"))
                        }),
                    "stale case {case}"
                );
                // A duplicate response cannot publish after the rejected result.
                captured_reply(&capture);
                tick_ai(&shared);
                pump_ai(&shared);
                assert!(
                    shared
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .contexts
                        .values()
                        .all(|entry| {
                            entry
                                .popup_model
                                .snapshot()
                                .map_or(true, |s| s.candidates.iter().all(|c| c.text != "今天心情"))
                        }),
                    "duplicate stale case {case}"
                );
                drop(shared.borrow_mut().take());
                drop(service);
            }
        }

        #[test]
        fn native_shift_commits_original_spelling_and_switches_only_after_success() {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            let host = NativeDocument::new().unwrap();
            let service: ITfTextInputProcessor = TextService::new().into();
            let implementation =
                unsafe { <ITfTextInputProcessor as AsImpl<TextService>>::as_impl(&service) };
            *implementation.activation.borrow_mut() = Some(Activation {
                ui: Rc::new(UiState::default()),
                _activation_flags: 0,
                thread_manager: host.manager.clone(),
                keystroke_manager: host.manager.cast().unwrap(),
                client_id: host.client,
                contexts: HashMap::new(),
                token: next_activation_token(),
                owner_thread: std::thread::current().id(),
                focus_epoch: 0,
                foreground: true,
                shown_context: None,
                processing: false,
                english: false,
                shift: ShiftGesture::default(),
                context_generation: Rc::new(Cell::new(0)),
                _thread_events: None,
                settings: Settings::default(),
                lexicon: Lexicon::from_tsv("ni\t你\t100\nhao\t好\t100\nni'hao\t你好\t200").unwrap(),
                rankings: Arc::new(Vec::new()),
                adaptive_ranking_enabled: true,
                learning: None,
                _learning_registration: None,
                local_data_error: false,
                ai: None,
                ai_unavailable: AiStatus::Unavailable,
            });
            let sink: ITfKeyEventSink = service.cast().unwrap();
            let down = |key| {
                unsafe { sink.OnKeyDown(&host.context, WPARAM(key), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            };
            let test_down = |key| {
                unsafe { sink.OnTestKeyDown(&host.context, WPARAM(key), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            };
            let test_up = || {
                unsafe { sink.OnTestKeyUp(&host.context, WPARAM(0xA0), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            };
            let up = || {
                unsafe { sink.OnKeyUp(&host.context, WPARAM(0xA0), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            };
            let generation = Rc::clone(
                &implementation
                    .activation
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .context_generation,
            );
            let events = ThreadEvents::subscribe(&host.manager, Rc::clone(&generation)).unwrap();
            implementation
                .activation
                .borrow_mut()
                .as_mut()
                .unwrap()
                ._thread_events = Some(events);
            for key in b"NIHAO" {
                send_key(&sink, &host.context, *key);
            }
            let partial = implementation
                .activation
                .borrow()
                .as_ref()
                .unwrap()
                .contexts
                .get(&context_key(&host.context))
                .unwrap()
                .router
                .session()
                .candidates_on_current_page()
                .iter()
                .position(|c| c.text == "你")
                .unwrap();
            send_key(&sink, &host.context, b'1' + partial as u8);
            assert_eq!(read_document(&host, None), "你hao");
            assert!(test_down(0xA0));
            assert!(test_down(0xA0)); // Prediction does not arm or toggle.
            assert!(!test_up());
            assert!(!down(0xA0));
            assert!(test_up());
            assert!(test_up()); // Prediction does not commit.
            assert_eq!(read_document(&host, None), "你hao");
            assert!(!implementation.activation.borrow().as_ref().unwrap().english);
            assert!(up());
            assert_eq!(read_document(&host, None), "nihao");
            {
                let state = implementation.activation.borrow();
                let active = state.as_ref().unwrap();
                assert!(active.english);
                assert!(active.rankings.is_empty());
                let router = &active
                    .contexts
                    .get(&context_key(&host.context))
                    .unwrap()
                    .router;
                assert!(router.pending_learning().is_empty());
                assert!(router.pending_promotions().is_empty());
            }
            assert!(!test_down(b'N' as usize));
            assert!(!down(b'N' as usize)); // Host handles English; test never injects keys.
            assert_eq!(read_document(&host, None), "nihao");
            assert!(!down(0xA0));
            assert!(test_down(b'A' as usize)); // Route chord solely to disarm.
            assert!(!down(b'A' as usize));
            assert!(!up());
            assert!(implementation.activation.borrow().as_ref().unwrap().english);
            assert!(!down(0xA0));
            assert!(up());
            assert!(!implementation.activation.borrow().as_ref().unwrap().english);

            // A context can leave and return without key-sink OnSetFocus.
            // The real ThreadMgr event subscription must still invalidate Shift.
            assert!(!down(0xA0));
            let before = generation.get();
            let mut extra = None;
            let mut cookie = 0;
            unsafe {
                host.document
                    .CreateContext(host.client, 0, None, &mut extra, &mut cookie)
                    .unwrap();
                host.document.Push(extra.as_ref().unwrap()).unwrap();
                host.document.Pop(0).unwrap();
            }
            assert!(generation.get() > before);
            assert_eq!(
                focused_context_key(&host.manager),
                Some(context_key(&host.context))
            );
            assert!(
                !unsafe { sink.OnKeyDown(&host.context, WPARAM(0xA0), LPARAM(1 << 30)) }
                    .unwrap()
                    .as_bool()
            );
            assert!(!up());
            assert!(!implementation.activation.borrow().as_ref().unwrap().english);

            // Invalid edit client fails before committing; mode and engine are restored.
            for key in b"NIHAO" {
                send_key(&sink, &host.context, *key);
            }
            for stale_generation in [false, true] {
                let (mut entry, target) = {
                    let state = implementation.activation.borrow();
                    let active = state.as_ref().unwrap();
                    (
                        active
                            .contexts
                            .get(&context_key(&host.context))
                            .unwrap()
                            .clone(),
                        EditTarget {
                            context_generation: Some(
                                generation.get() + u64::from(stale_generation),
                            ),
                            switch_to_english: true,
                            promotion_claim: Rc::new(Cell::new(false)),
                            activation: Rc::clone(&implementation.activation),
                            thread_manager: host.manager.clone(),
                            token: active.token,
                            owner_thread: active.owner_thread,
                            key: context_key(&host.context),
                            focus_epoch: active.focus_epoch + u64::from(!stale_generation),
                        },
                    )
                };
                let update = entry.router.commit_raw();
                assert!(request_checked_key_edit_session(
                    &host.context,
                    host.client,
                    update,
                    entry.composition.take(),
                    entry.controller,
                    Some(target),
                )
                .is_none());
                assert_eq!(read_document(&host, None), "nihaonihao");
                assert!(!implementation.activation.borrow().as_ref().unwrap().english);
            }
            implementation
                .activation
                .borrow_mut()
                .as_mut()
                .unwrap()
                .client_id = u32::MAX;
            assert!(!down(0xA0));
            assert!(!up());
            assert_eq!(read_document(&host, None), "nihaonihao");
            {
                let state = implementation.activation.borrow();
                let active = state.as_ref().unwrap();
                assert!(!active.english);
                assert_eq!(
                    active
                        .contexts
                        .get(&context_key(&host.context))
                        .unwrap()
                        .router
                        .preedit(),
                    "nihao"
                );
            }
            implementation
                .activation
                .borrow_mut()
                .as_mut()
                .unwrap()
                .client_id = host.client;
            assert!(!down(0xA0));
            unsafe { sink.OnSetFocus(FALSE) }.unwrap();
            assert!(!test_up());
            assert!(!up());
            assert!(!implementation.activation.borrow().as_ref().unwrap().english);
            unsafe { sink.OnSetFocus(TRUE) }.unwrap();
            assert!(!down(0xA0));
            assert!(up());
            assert!(implementation.activation.borrow().as_ref().unwrap().english);
            assert_eq!(read_document(&host, None), "nihaonihao");
            implementation.activation.borrow_mut().take();
        }

        #[test]
        fn native_context_accepts_pinyin_in_write_edit_session() {
            // Only a new, TSF-owned empty document is used. This never reads a
            // user document, injects global keys, or changes profile selection.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .unwrap();
            let _apartment = Apartment;
            let host = NativeDocument::new().unwrap();
            let service: ITfTextInputProcessor = TextService::new().into();
            let implementation =
                unsafe { <ITfTextInputProcessor as AsImpl<TextService>>::as_impl(&service) };
            let token = next_activation_token();
            *implementation.activation.borrow_mut() = Some(Activation {
                ui: Rc::new(UiState::default()),
                _activation_flags: 0,
                thread_manager: host.manager.clone(),
                keystroke_manager: host.manager.cast().unwrap(),
                client_id: host.client,
                contexts: HashMap::new(),
                token,
                owner_thread: std::thread::current().id(),
                focus_epoch: 0,
                foreground: true,
                shown_context: None,
                processing: false,
                english: false,
                shift: ShiftGesture::default(),
                context_generation: Rc::new(Cell::new(0)),
                _thread_events: None,
                settings: Settings::default(),
                lexicon: Lexicon::builtin().unwrap(),
                rankings: Arc::new(Vec::new()),
                adaptive_ranking_enabled: false,
                learning: None,
                _learning_registration: None,
                local_data_error: false,
                ai: None,
                ai_unavailable: AiStatus::Unavailable,
            });
            let sink: ITfKeyEventSink = service.cast().unwrap();
            let tested =
                unsafe { sink.OnTestKeyDown(&host.context, WPARAM(b'N' as usize), LPARAM(0)) };
            let handled =
                unsafe { sink.OnKeyDown(&host.context, WPARAM(b'N' as usize), LPARAM(0)) };
            assert!(
                tested.unwrap().as_bool(),
                "engine must accept a plain letter"
            );
            assert!(
                handled.unwrap().as_bool(),
                "native TSF edit must apply before eating the letter"
            );
            assert_eq!(read_document(&host, None), "n");
            for key in b"IHAO" {
                send_key(&sink, &host.context, *key);
            }
            assert_eq!(read_document(&host, None), "nihao");
            send_key(&sink, &host.context, b' ');
            assert_eq!(read_document(&host, None), "你好");
            for key in b"SHI" {
                send_key(&sink, &host.context, *key);
            }
            send_key(&sink, &host.context, 0x1B);
            assert_eq!(read_document(&host, None), "你好");
            for key in b"NI HAO " {
                send_key(&sink, &host.context, *key);
            }
            assert_eq!(
                read_document(&host, None),
                "你好你好",
                "commits must follow the caret"
            );
            for key in b"SHI" {
                send_key(&sink, &host.context, *key);
            }
            let entry = implementation
                .activation
                .borrow()
                .as_ref()
                .unwrap()
                .contexts
                .get(&context_key(&host.context))
                .unwrap()
                .clone();
            let revision = entry.popup_model.snapshot().unwrap().revision;
            let composition = entry.composition.unwrap();
            assert_eq!(
                read_document(&host, Some(composition.clone())),
                "你好你好shi"
            );
            assert!(
                composition.terminated.get(),
                "TSF must notify the real sink"
            );
            send_key(&sink, &host.context, b'N');
            assert!(
                implementation
                    .activation
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .contexts
                    .get(&context_key(&host.context))
                    .unwrap()
                    .popup_model
                    .snapshot()
                    .unwrap()
                    .revision
                    > revision,
                "old popup actions must stay invalid"
            );
            send_key(&sink, &host.context, b'I');
            send_key(&sink, &host.context, b' ');
            assert_eq!(read_document(&host, None), "你好你好shi你");
            // A minimal source isolates UTF-16 edit behavior from candidate rank.
            // The full dictionary's supplementary lookup is verified separately.
            implementation
                .activation
                .borrow_mut()
                .as_mut()
                .unwrap()
                .contexts
                .get_mut(&context_key(&host.context))
                .unwrap()
                .router = EngineRouter::from_lexicon_tsv("he\t𠀀\t1", 9).unwrap();
            for key in b"HE " {
                send_key(&sink, &host.context, *key);
            }
            assert_eq!(read_document(&host, None), "你好你好shi你𠀀");
            implementation
                .activation
                .borrow_mut()
                .as_mut()
                .unwrap()
                .contexts
                .get_mut(&context_key(&host.context))
                .unwrap()
                .router = EngineRouter::from_lexicon_tsv("ni\t你\t1000\nhao\t好\t1000", 9).unwrap();
            for key in b"NIHAO2" {
                send_key(&sink, &host.context, *key);
            }
            assert_eq!(read_document(&host, None), "你好你好shi你𠀀你hao");
            send_key(&sink, &host.context, b' ');
            assert_eq!(read_document(&host, None), "你好你好shi你𠀀你好");
            implementation
                .activation
                .borrow_mut()
                .as_mut()
                .unwrap()
                .contexts
                .get_mut(&context_key(&host.context))
                .unwrap()
                .router = EngineRouter::from_lexicon_tsv("ni'hao\t你好\t1000", 9).unwrap();
            for key in b"NH" {
                send_key(&sink, &host.context, *key);
            }
            assert_eq!(read_document(&host, None), "你好你好shi你𠀀你好nh");
            send_key(&sink, &host.context, b' ');
            assert_eq!(read_document(&host, None), "你好你好shi你𠀀你好你好");
            // Real TSF writes, not global keyboard injection: punctuation works
            // while idle and confirms the complete composition before appending.
            let prefix = read_document(&host, None);
            send_key(&sink, &host.context, 0xBC);
            assert_eq!(read_document(&host, None), format!("{prefix}，"));
            for key in b"NIHAO" {
                send_key(&sink, &host.context, *key);
            }
            send_key(&sink, &host.context, 0xBE);
            assert_eq!(read_document(&host, None), format!("{prefix}，你好。"));
            for _ in 0..2 {
                assert!(
                    unsafe { sink.OnTestKeyDown(&host.context, WPARAM(0xDE), LPARAM(0)) }
                        .unwrap()
                        .as_bool()
                );
            }
            send_key(&sink, &host.context, 0xDE);
            send_key(&sink, &host.context, 0xDE);
            assert_eq!(read_document(&host, None), format!("{prefix}，你好。‘’"));
            implementation
                .activation
                .borrow_mut()
                .as_mut()
                .unwrap()
                .english = true;
            assert!(
                !unsafe { sink.OnTestKeyDown(&host.context, WPARAM(0xBC), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            );
            assert!(
                !unsafe { sink.OnKeyDown(&host.context, WPARAM(0xBC), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            );
            assert_eq!(read_document(&host, None), format!("{prefix}，你好。‘’"));
            implementation.activation.borrow_mut().take();
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows_exports::{DllCanUnloadNow, DllGetClassObject};

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::{FLOWER_TEXT_SERVICE_CLSID, TEXT_SERVICE_DESCRIPTION};

    #[test]
    fn text_service_clsid_matches_registration() {
        assert_eq!(
            FLOWER_TEXT_SERVICE_CLSID.to_u128(),
            0x3d5b7cb1_96cb_4f44_a792_64304f6e7426
        );
    }

    #[test]
    fn text_service_description_is_stable() {
        assert_eq!(TEXT_SERVICE_DESCRIPTION, "Flower");
    }
}
