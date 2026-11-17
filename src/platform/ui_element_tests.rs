#[derive(Default)]
struct UiObservations {
    allow: Cell<bool>,
    begins: Cell<u32>,
    updates: Cell<u32>,
    ends: Cell<u32>,
    element: RefCell<Option<ITfCandidateListUIElement>>,
}

#[test]
fn ui_element_clean_candidate_strings_preserve_original_identity() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let (host, _store) = writable_document();
    let candidates = vec![
        crate::engine::Candidate::new("普通", "pu'tong"),
        crate::engine::Candidate::new("纠正", crate::ai::AI_COMMENT),
        crate::engine::Candidate::new(
            "拼写结果",
            format!("{}pin'xie", crate::engine::Candidate::SPELLING_PREFIX),
        ),
        crate::engine::Candidate::new(
            "完整后续",
            format!("{}wan'zheng", crate::engine::Candidate::COMPLETION_PREFIX),
        ),
    ];
    let data = Rc::new(UiData {
        snapshot: RefCell::new(crate::platform::CandidatePopupSnapshot {
            revision: 1,
            preedit: "fixture".into(),
            candidates: candidates.clone(),
            highlighted: Some(0),
            page: 0,
            page_count: 1,
        }),
        context: host.context.clone(),
        allowed: Cell::new(false),
        shown: Cell::new(false),
        alive: Cell::new(true),
        negotiated: Cell::new(false),
        closed: Rc::new(Cell::new(false)),
        requested: Cell::new(None),
        token: next_activation_token(),
        owner: std::thread::current().id(),
        caret: Cell::new(None),
        _registration: ObjectRegistration::new(),
    });
    let element: ITfCandidateListUIElement = CandidateUi { data: data.clone() }.into();
    for (index, expected) in ["普通", "纠正 √", "拼写结果 √", "完整后续"]
        .iter()
        .enumerate()
    {
        assert_eq!(
            unsafe { element.GetString(index as u32) }
                .unwrap()
                .to_string(),
            *expected
        );
    }
    assert!(unsafe { element.GetString(4) }.is_err());
    assert_eq!(data.snapshot.borrow().candidates, candidates);
}

#[implement(ITfUIElementSink)]
struct ObservingUiSink {
    manager: ITfUIElementMgr,
    observations: Rc<UiObservations>,
    shared: SharedActivation,
}

impl ITfUIElementSink_Impl for ObservingUiSink_Impl {
    fn BeginUIElement(&self, id: u32, show: *mut BOOL) -> Result<()> {
        let this = self.get_impl();
        assert!(
            this.shared.try_borrow_mut().is_ok(),
            "Begin outside activation borrow"
        );
        let element: ITfCandidateListUIElement =
            unsafe { this.manager.GetUIElement(id) }?.cast()?;
        assert!(element.cast::<ITfCandidateListUIElementBehavior>().is_err());
        this.observations
            .begins
            .set(this.observations.begins.get() + 1);
        *this.observations.element.borrow_mut() = Some(element);
        unsafe {
            show.write(BOOL::from(this.observations.allow.get()));
        }
        Ok(())
    }
    fn UpdateUIElement(&self, _: u32) -> Result<()> {
        let this = self.get_impl();
        assert!(
            this.shared.try_borrow_mut().is_ok(),
            "Update outside activation borrow"
        );
        this.observations
            .updates
            .set(this.observations.updates.get() + 1);
        Ok(())
    }
    fn EndUIElement(&self, _: u32) -> Result<()> {
        let this = self.get_impl();
        assert!(
            this.shared.try_borrow_mut().is_ok(),
            "End outside activation borrow"
        );
        this.observations.ends.set(this.observations.ends.get() + 1);
        Ok(())
    }
}

fn isolated_service() -> (ITfTextInputProcessorEx, std::path::PathBuf) {
    let root = std::path::PathBuf::from(
        std::env::var_os("FLOWER_TEST_SCRATCH").expect("isolated scratch"),
    )
    .join(format!(
        "ui-element-{}-{}",
        std::process::id(),
        next_activation_token()
    ));
    let settings_store = SettingsStore::new(root.join("settings.json"));
    let settings = Settings {
        ai_correction_enabled: false,
        ..Settings::default()
    };
    settings_store.save(&settings).unwrap();
    let mut implementation = TextService::new();
    implementation.stores = Some((
        settings_store,
        DictionaryStore::new(root.join("dictionary.json")),
    ));
    (implementation.into(), root)
}

fn assert_popup_visibility(shared: &SharedActivation, expected: bool) {
    let (token, owner) = {
        let state = shared.borrow();
        let active = state.as_ref().unwrap();
        (active.token, active.owner_thread)
    };
    let popup = take_thread_popup(token, owner).expect("hidden infrastructure exists");
    assert_eq!(popup.window.is_visible(), expected);
    return_thread_popup(token, owner, popup);
}

#[test]
fn ui_element_real_activate_ex_host_negotiation_and_keyboard() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let (host, _store) = writable_document();
    let (service, root) = isolated_service();
    let advised = Rc::new(Cell::new(0));
    let manager: ITfThreadMgr = TestThreadManager {
        real: host.manager.clone(),
        context: host.context.clone(),
        key_sink: RefCell::new(None),
        advised: advised.clone(),
        fail_begin: Rc::new(Cell::new(false)),
        fail_update: Rc::new(Cell::new(false)),
        begin_hook: RefCell::new(None),
    }
    .into();
    let implementation =
        unsafe { <ITfTextInputProcessorEx as AsImpl<TextService>>::as_impl(&service) };
    let shared = implementation.activation.clone();
    let observations = Rc::new(UiObservations::default());
    let observer: ITfUIElementSink = ObservingUiSink {
        manager: host.manager.cast().unwrap(),
        observations: observations.clone(),
        shared: shared.clone(),
    }
    .into();
    let source: ITfSource = host.manager.cast().unwrap();
    let cookie = unsafe { source.AdviseSink(&ITfUIElementSink::IID, &observer) }.unwrap();
    unsafe { service.ActivateEx(&manager, host.client, TF_TMAE_UIELEMENTENABLEDONLY) }.unwrap();
    assert_eq!(advised.get(), 1);
    assert_eq!(
        shared.borrow().as_ref().unwrap()._activation_flags,
        TF_TMAE_UIELEMENTENABLEDONLY
    );
    assert!(unsafe { service.ActivateEx(&manager, host.client, 0) }.is_err());
    assert_eq!(advised.get(), 1, "no duplicate subscription");
    let keys: ITfKeystrokeMgr = manager.cast().unwrap();
    let sink: ITfKeyEventSink = service.cast().unwrap();
    // Actual manager dispatch proves AdviseKeyEventSink was performed.
    assert!(
        unsafe { keys.TestKeyDown(WPARAM(b'N' as usize), LPARAM(0)) }
            .unwrap()
            .as_bool()
    );
    assert_eq!(read_document(&host, None), "", "OnTest is pure");
    assert_eq!(observations.begins.get(), 0);
    assert!(unsafe { keys.KeyDown(WPARAM(b'N' as usize), LPARAM(0)) }
        .unwrap()
        .as_bool());
    assert_eq!(observations.begins.get(), 1);
    assert_popup_visibility(&shared, false);
    let element = observations.element.borrow().as_ref().unwrap().clone();
    assert!(!unsafe { element.IsShown() }.unwrap().as_bool());
    let document = unsafe { element.GetDocumentMgr() }.unwrap();
    assert_eq!(
        document.cast::<IUnknown>().unwrap(),
        host.document.cast::<IUnknown>().unwrap()
    );
    let snapshot = shared.borrow().as_ref().unwrap().contexts[&context_key(&host.context)]
        .popup_model
        .snapshot()
        .unwrap()
        .clone();
    assert_eq!(
        unsafe { element.GetCount() }.unwrap() as usize,
        snapshot.candidates.len()
    );
    for (i, candidate) in snapshot.candidates.iter().enumerate() {
        assert_eq!(
            unsafe { element.GetString(i as u32) }.unwrap().to_string(),
            candidate.display_text()
        );
    }
    assert!(unsafe { element.GetString(snapshot.candidates.len() as u32) }.is_err());
    let mut page = [99];
    let mut count = 99;
    unsafe { element.GetPageIndex(&mut page, &mut count) }.unwrap();
    assert_eq!((page[0], count), (0, 1));
    unsafe { element.SetPageIndex(&[0]) }.unwrap();
    assert!(unsafe { element.SetPageIndex(&[1]) }.is_err());
    assert!(unsafe { element.SetPageIndex(&[0, 0]) }.is_err());
    assert_eq!(unsafe { element.GetCurrentPage() }.unwrap(), 0);
    unsafe { element.Show(TRUE) }.unwrap();
    assert_popup_visibility(&shared, true);
    unsafe { element.Show(FALSE) }.unwrap();
    send_key(&sink, &host.context, b'I');
    assert!(observations.updates.get() > 0);
    assert_popup_visibility(&shared, false);
    refresh_ai_status(
        &shared,
        shared.borrow().as_ref().unwrap().token,
        std::thread::current().id(),
    );
    assert_popup_visibility(&shared, false);
    send_key(&sink, &host.context, 0x20);
    assert_eq!(read_document(&host, None), "你");
    assert_eq!(observations.ends.get(), 1);
    assert!(
        unsafe { element.Show(TRUE) }.is_err(),
        "ended element cannot show"
    );
    observations.allow.set(true);
    send_key(&sink, &host.context, b'N');
    assert_popup_visibility(&shared, true);
    send_key(&sink, &host.context, 0x1b);
    assert_eq!(observations.ends.get(), 2);
    send_key(&sink, &host.context, b'N');
    unsafe { host.manager.SetFocus(None::<&ITfDocumentMgr>) }.unwrap();
    unsafe { sink.OnSetFocus(FALSE) }.unwrap();
    assert_eq!(observations.ends.get(), 3);
    assert_popup_visibility(&shared, false);
    unsafe { host.manager.SetFocus(&host.document) }.unwrap();
    unsafe { sink.OnSetFocus(TRUE) }.unwrap();
    send_key(&sink, &host.context, b'I');
    let old = observations.element.borrow().as_ref().unwrap().clone();
    unsafe { service.Deactivate() }.unwrap();
    assert!(shared.borrow().is_none());
    assert!(unsafe { old.Show(TRUE) }.is_err());
    // Original Activate uses precisely the same isolated initialization.
    let base: ITfTextInputProcessor = service.cast().unwrap();
    unsafe { base.Activate(&manager, host.client) }.unwrap();
    assert_eq!(shared.borrow().as_ref().unwrap()._activation_flags, 0);
    assert!(
        unsafe { old.Show(TRUE) }.is_err(),
        "old element cannot affect new activation"
    );
    unsafe { base.Deactivate() }.unwrap();
    unsafe { source.UnadviseSink(cookie) }.unwrap();
    drop(service);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ui_element_begin_update_failures_and_current_page_snapshot() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let (host, _store) = writable_document();
    let (service, root) = isolated_service();
    let fail_begin = Rc::new(Cell::new(true));
    let fail_update = Rc::new(Cell::new(false));
    let manager: ITfThreadMgr = TestThreadManager {
        real: host.manager.clone(),
        context: host.context.clone(),
        key_sink: RefCell::new(None),
        advised: Rc::new(Cell::new(0)),
        fail_begin: fail_begin.clone(),
        fail_update: fail_update.clone(),
        begin_hook: RefCell::new(None),
    }
    .into();
    let implementation =
        unsafe { <ITfTextInputProcessorEx as AsImpl<TextService>>::as_impl(&service) };
    let shared = implementation.activation.clone();
    let observations = Rc::new(UiObservations::default());
    observations.allow.set(true);
    let observer: ITfUIElementSink = ObservingUiSink {
        manager: host.manager.cast().unwrap(),
        observations: observations.clone(),
        shared: shared.clone(),
    }
    .into();
    let source: ITfSource = host.manager.cast().unwrap();
    let cookie = unsafe { source.AdviseSink(&ITfUIElementSink::IID, &observer) }.unwrap();
    unsafe { service.ActivateEx(&manager, host.client, 0) }.unwrap();
    // Deterministic fixture data; no user dictionary is read.
    let long = "这是完整候选文字不能截断".repeat(20);
    // Long-body policy deliberately keeps just one complete candidate. The
    // remaining fixture choices must be partial prefixes to exercise paging.
    let mut tsv = format!("ni'hao\t{long}\t10000\n");
    for i in 0..25 {
        tsv.push_str(&format!("ni\t候选{i}\t{}\n", 1000 - i));
    }
    {
        let mut state = shared.borrow_mut();
        let active = state.as_mut().unwrap();
        // Installing fixture data also replaces the deferred builtin load.
        active.pending_lexicon = None;
        // Exercise a genuine second page independently of default quotas.
        active.settings.candidate_page_size = 2;
        active.lexicon = Lexicon::from_tsv(&tsv).unwrap();
    }
    let sink: ITfKeyEventSink = service.cast().unwrap();
    send_key(&sink, &host.context, b'N');
    assert_popup_visibility(&shared, false);
    assert_eq!(observations.begins.get(), 0);
    fail_begin.set(false);
    send_key(&sink, &host.context, b'I');
    for key in b"HAO" {
        send_key(&sink, &host.context, *key);
    }
    assert_eq!(observations.begins.get(), 1);
    let element = observations.element.borrow().as_ref().unwrap().clone();
    assert_eq!(unsafe { element.GetString(0) }.unwrap().to_string(), long);
    let first_count = unsafe { element.GetCount() }.unwrap();
    assert!(first_count < 26);
    send_key(&sink, &host.context, 0x22);
    let snapshot = shared.borrow().as_ref().unwrap().contexts[&context_key(&host.context)]
        .popup_model
        .snapshot()
        .unwrap()
        .clone();
    assert!(snapshot.page > 0);
    assert_eq!(unsafe { element.GetCurrentPage() }.unwrap(), 0);
    assert_eq!(
        unsafe { element.GetCount() }.unwrap() as usize,
        snapshot.candidates.len()
    );
    assert_eq!(
        unsafe { element.GetSelection() }.unwrap() as usize,
        snapshot.highlighted.unwrap_or(0)
    );
    for (i, candidate) in snapshot.candidates.iter().enumerate() {
        assert_eq!(
            unsafe { element.GetString(i as u32) }.unwrap().to_string(),
            candidate.text
        );
    }
    fail_update.set(true);
    send_key(&sink, &host.context, 0x21);
    assert_eq!(observations.ends.get(), 1);
    assert_popup_visibility(&shared, false);
    assert!(unsafe { element.Show(TRUE) }.is_err());
    fail_update.set(false);
    send_key(&sink, &host.context, 0x22);
    assert_eq!(observations.begins.get(), 2);
    let current = observations.element.borrow().as_ref().unwrap().clone();
    // Host composition termination, not merely a later key, ends the element.
    let composition = shared.borrow().as_ref().unwrap().contexts[&context_key(&host.context)]
        .composition
        .as_ref()
        .unwrap()
        .clone();
    let binding = composition.ui.borrow().clone().unwrap();
    assert!(binding.1.upgrade().unwrap().alive.get());
    // The existing native fixture's read session invokes TSF termination.
    let _ = read_document(&host, Some(composition));
    assert_eq!(observations.ends.get(), 2);
    assert!(unsafe { current.Show(TRUE) }.is_err());
    unsafe { service.Deactivate() }.unwrap();
    unsafe { source.UnadviseSink(cookie) }.unwrap();
    drop(service);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ui_element_uiless_async_ai_does_not_show_popup_or_status() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let scope: IUnknown = TestScope(vec![0]).into();
    let calls = Rc::new(Cell::new(0));
    let owner: IUnknown = ScopeOwner {
        value: RefCell::new(VARIANT::from(scope)),
        fail: false,
        calls: calls.clone(),
    }
    .into();
    let host = NativeDocument::with_owner(Some(&owner)).unwrap();
    let (service, root) = isolated_service();
    let manager: ITfThreadMgr = TestThreadManager {
        real: host.manager.clone(),
        context: host.context.clone(),
        key_sink: RefCell::new(None),
        advised: Rc::new(Cell::new(0)),
        fail_begin: Rc::new(Cell::new(false)),
        fail_update: Rc::new(Cell::new(false)),
        begin_hook: RefCell::new(None),
    }
    .into();
    let implementation =
        unsafe { <ITfTextInputProcessorEx as AsImpl<TextService>>::as_impl(&service) };
    let shared = implementation.activation.clone();
    let observations = Rc::new(UiObservations::default());
    let observer: ITfUIElementSink = ObservingUiSink {
        manager: host.manager.cast().unwrap(),
        observations: observations.clone(),
        shared: shared.clone(),
    }
    .into();
    let source: ITfSource = host.manager.cast().unwrap();
    let cookie = unsafe { source.AdviseSink(&ITfUIElementSink::IID, &observer) }.unwrap();
    unsafe { service.ActivateEx(&manager, host.client, TF_TMAE_UIELEMENTENABLEDONLY) }.unwrap();
    let capture = Rc::new(CapturedAi::default());
    {
        let mut state = shared.borrow_mut();
        let active = state.as_mut().unwrap();
        active.settings.ai_correction_enabled = true;
        active.pending_lexicon = None;
        active.lexicon = Lexicon::from_tsv("jin'tian'xin'qing\t今天新情\t100").unwrap();
        active.ai = Some(AiScheduler::new(Box::new(CapturingTransport {
            capture: capture.clone(),
            scope_calls: calls.clone(),
        })));
    }
    let sink: ITfKeyEventSink = service.cast().unwrap();
    for key in b"JINTIANXINQING" {
        send_key(&sink, &host.context, *key);
    }
    shared
        .borrow_mut()
        .as_mut()
        .unwrap()
        .ai
        .as_mut()
        .unwrap()
        .last_input = Instant::now() - Duration::from_millis(300);
    let before = read_document(&host, None);
    let updates = observations.updates.get();
    tick_ai(&shared);
    pump_ai(&shared);
    assert_eq!(
        capture.requests.borrow().len(),
        1,
        "UIless retains AI scheduling"
    );
    captured_reply(&capture);
    tick_ai(&shared);
    pump_ai(&shared);
    assert!(observations.updates.get() > updates);
    assert_popup_visibility(&shared, false);
    assert_eq!(
        read_document(&host, None),
        before,
        "AI publication does not write text"
    );
    let element = observations.element.borrow().as_ref().unwrap().clone();
    let strings: Vec<String> = (0..unsafe { element.GetCount() }.unwrap())
        .map(|i| unsafe { element.GetString(i) }.unwrap().to_string())
        .collect();
    assert!(
        strings.iter().any(|text| text == "今天心情 √"),
        "{strings:?}"
    );
    unsafe { element.Show(TRUE) }.unwrap();
    assert_popup_visibility(&shared, true);
    unsafe { element.Show(FALSE) }.unwrap();
    // Caret/key refresh and status changes cannot override Show(false).
    send_key(&sink, &host.context, 0x25);
    let token = shared.borrow().as_ref().unwrap().token;
    refresh_ai_status(&shared, token, std::thread::current().id());
    assert_popup_visibility(&shared, false);
    unsafe { service.Deactivate() }.unwrap();
    assert!(capture.stopped.get());
    unsafe { source.UnadviseSink(cookie) }.unwrap();
    drop(service);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn ui_element_begin_reentry_cannot_publish_into_new_activation() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let (host, _store) = writable_document();
    let (service, root) = isolated_service();
    let manager: ITfThreadMgr = TestThreadManager {
        real: host.manager.clone(),
        context: host.context.clone(),
        key_sink: RefCell::new(None),
        advised: Rc::new(Cell::new(0)),
        fail_begin: Rc::new(Cell::new(false)),
        fail_update: Rc::new(Cell::new(false)),
        begin_hook: RefCell::new(None),
    }
    .into();
    let implementation =
        unsafe { <ITfTextInputProcessorEx as AsImpl<TextService>>::as_impl(&service) };
    let shared = implementation.activation.clone();
    let observations = Rc::new(UiObservations::default());
    observations.allow.set(true);
    let observer: ITfUIElementSink = ObservingUiSink {
        manager: host.manager.cast().unwrap(),
        observations: observations.clone(),
        shared: shared.clone(),
    }
    .into();
    let source: ITfSource = host.manager.cast().unwrap();
    let cookie = unsafe { source.AdviseSink(&ITfUIElementSink::IID, &observer) }.unwrap();
    unsafe { service.ActivateEx(&manager, host.client, 0) }.unwrap();
    let old_token = shared.borrow().as_ref().unwrap().token;
    let reentrant_service = service.clone();
    let reentrant_manager = manager.clone();
    let client = host.client;
    let facade = unsafe { <ITfThreadMgr as AsImpl<TestThreadManager>>::as_impl(&manager) };
    *facade.begin_hook.borrow_mut() = Some(Rc::new(move || unsafe {
        reentrant_service.Deactivate().unwrap();
        reentrant_service
            .ActivateEx(&reentrant_manager, client, 0)
            .unwrap();
    }));
    let keys: ITfKeystrokeMgr = manager.cast().unwrap();
    assert!(unsafe { keys.KeyDown(WPARAM(b'N' as usize), LPARAM(0)) }
        .unwrap()
        .as_bool());
    assert_ne!(shared.borrow().as_ref().unwrap().token, old_token);
    assert!(shared.borrow().as_ref().unwrap().contexts.is_empty());
    assert_eq!(observations.begins.get(), 1);
    assert_eq!(observations.ends.get(), 1, "stale Begin is paired with End");
    let stale = observations.element.borrow().as_ref().unwrap().clone();
    assert!(unsafe { stale.Show(TRUE) }.is_err());
    assert_popup_visibility(&shared, false);
    unsafe { service.Deactivate() }.unwrap();
    unsafe { source.UnadviseSink(cookie) }.unwrap();
    drop(service);
    std::fs::remove_dir_all(root).unwrap();
}
// Isolated application client IDs cannot be advised as registered TIP IDs by
// msctf. This COM facade records the real service's subscription, while keeping
// the actual TSF document, ACP edit sessions, UIElement manager and sink events.
#[implement(ITfThreadMgr, ITfKeystrokeMgr, ITfUIElementMgr, ITfSource)]
struct TestThreadManager {
    real: ITfThreadMgr,
    context: ITfContext,
    key_sink: RefCell<Option<ITfKeyEventSink>>,
    advised: Rc<Cell<u32>>,
    fail_begin: Rc<Cell<bool>>,
    fail_update: Rc<Cell<bool>>,
    begin_hook: RefCell<Option<Rc<dyn Fn()>>>,
}

impl ITfThreadMgr_Impl for TestThreadManager_Impl {
    fn Activate(&self) -> Result<u32> {
        Err(E_NOTIMPL.into())
    }
    fn Deactivate(&self) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn CreateDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        unsafe { self.real.CreateDocumentMgr() }
    }
    fn EnumDocumentMgrs(&self) -> Result<IEnumTfDocumentMgrs> {
        unsafe { self.real.EnumDocumentMgrs() }
    }
    fn GetFocus(&self) -> Result<ITfDocumentMgr> {
        unsafe { self.real.GetFocus() }
    }
    fn SetFocus(&self, doc: Option<&ITfDocumentMgr>) -> Result<()> {
        unsafe { self.real.SetFocus(doc) }
    }
    fn AssociateFocus(
        &self,
        _: windows::Win32::Foundation::HWND,
        _: Option<&ITfDocumentMgr>,
    ) -> Result<ITfDocumentMgr> {
        Err(E_NOTIMPL.into())
    }
    fn IsThreadFocus(&self) -> Result<BOOL> {
        Ok(TRUE)
    }
    fn GetFunctionProvider(&self, _: *const GUID) -> Result<ITfFunctionProvider> {
        Err(E_NOTIMPL.into())
    }
    fn EnumFunctionProviders(&self) -> Result<IEnumTfFunctionProviders> {
        Err(E_NOTIMPL.into())
    }
    fn GetGlobalCompartment(&self) -> Result<ITfCompartmentMgr> {
        unsafe { self.real.GetGlobalCompartment() }
    }
}

impl ITfSource_Impl for TestThreadManager_Impl {
    fn AdviseSink(&self, iid: *const GUID, sink: Option<&IUnknown>) -> Result<u32> {
        unsafe { self.real.cast::<ITfSource>()?.AdviseSink(iid, sink) }
    }
    fn UnadviseSink(&self, cookie: u32) -> Result<()> {
        unsafe { self.real.cast::<ITfSource>()?.UnadviseSink(cookie) }
    }
}

impl ITfUIElementMgr_Impl for TestThreadManager_Impl {
    fn BeginUIElement(
        &self,
        element: Option<&ITfUIElement>,
        show: *mut BOOL,
        id: *mut u32,
    ) -> Result<()> {
        if self.fail_begin.get() {
            return Err(E_UNEXPECTED.into());
        }
        let result = unsafe {
            self.real
                .cast::<ITfUIElementMgr>()?
                .BeginUIElement(element, show, id)
        };
        let hook = self.begin_hook.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
        result
    }
    fn UpdateUIElement(&self, id: u32) -> Result<()> {
        if self.fail_update.get() {
            return Err(E_UNEXPECTED.into());
        }
        unsafe { self.real.cast::<ITfUIElementMgr>()?.UpdateUIElement(id) }
    }
    fn EndUIElement(&self, id: u32) -> Result<()> {
        unsafe { self.real.cast::<ITfUIElementMgr>()?.EndUIElement(id) }
    }
    fn GetUIElement(&self, id: u32) -> Result<ITfUIElement> {
        unsafe { self.real.cast::<ITfUIElementMgr>()?.GetUIElement(id) }
    }
    fn EnumUIElements(&self) -> Result<IEnumTfUIElements> {
        unsafe { self.real.cast::<ITfUIElementMgr>()?.EnumUIElements() }
    }
}

impl ITfKeystrokeMgr_Impl for TestThreadManager_Impl {
    fn AdviseKeyEventSink(
        &self,
        _: u32,
        sink: Option<&ITfKeyEventSink>,
        foreground: BOOL,
    ) -> Result<()> {
        assert!(foreground.as_bool());
        let sink = sink.ok_or_else(|| Error::from(E_POINTER))?.clone();
        assert!(self.key_sink.borrow().is_none());
        *self.key_sink.borrow_mut() = Some(sink.clone());
        self.advised.set(self.advised.get() + 1);
        unsafe { sink.OnSetFocus(TRUE) }
    }
    fn UnadviseKeyEventSink(&self, _: u32) -> Result<()> {
        self.key_sink.borrow_mut().take();
        Ok(())
    }
    fn GetForeground(&self) -> Result<GUID> {
        Ok(FLOWER_TEXT_SERVICE_CLSID)
    }
    fn TestKeyDown(&self, w: WPARAM, l: LPARAM) -> Result<BOOL> {
        let sink = self
            .key_sink
            .borrow()
            .clone()
            .ok_or_else(|| Error::from(E_UNEXPECTED))?;
        unsafe { sink.OnTestKeyDown(&self.context, w, l) }
    }
    fn TestKeyUp(&self, w: WPARAM, l: LPARAM) -> Result<BOOL> {
        let sink = self
            .key_sink
            .borrow()
            .clone()
            .ok_or_else(|| Error::from(E_UNEXPECTED))?;
        unsafe { sink.OnTestKeyUp(&self.context, w, l) }
    }
    fn KeyDown(&self, w: WPARAM, l: LPARAM) -> Result<BOOL> {
        let sink = self
            .key_sink
            .borrow()
            .clone()
            .ok_or_else(|| Error::from(E_UNEXPECTED))?;
        unsafe { sink.OnKeyDown(&self.context, w, l) }
    }
    fn KeyUp(&self, w: WPARAM, l: LPARAM) -> Result<BOOL> {
        let sink = self
            .key_sink
            .borrow()
            .clone()
            .ok_or_else(|| Error::from(E_UNEXPECTED))?;
        unsafe { sink.OnKeyUp(&self.context, w, l) }
    }
    fn GetPreservedKey(&self, _: Option<&ITfContext>, _: *const TF_PRESERVEDKEY) -> Result<GUID> {
        Err(E_NOTIMPL.into())
    }
    fn IsPreservedKey(&self, _: *const GUID, _: *const TF_PRESERVEDKEY) -> Result<BOOL> {
        Ok(FALSE)
    }
    fn PreserveKey(
        &self,
        _: u32,
        _: *const GUID,
        _: *const TF_PRESERVEDKEY,
        _: &PCWSTR,
        _: u32,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn UnpreserveKey(&self, _: *const GUID, _: *const TF_PRESERVEDKEY) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn SetPreservedKeyDescription(&self, _: *const GUID, _: &PCWSTR, _: u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn GetPreservedKeyDescription(&self, _: *const GUID) -> Result<windows::core::BSTR> {
        Err(E_NOTIMPL.into())
    }
    fn SimulatePreservedKey(&self, _: Option<&ITfContext>, _: *const GUID) -> Result<BOOL> {
        Err(E_NOTIMPL.into())
    }
}
