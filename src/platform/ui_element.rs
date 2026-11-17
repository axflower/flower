// Included in windows_exports so the COM boundary shares its activation identity.
use windows::core::BSTR;
use windows::Win32::UI::TextServices::{
    ITfCandidateListUIElement, ITfCandidateListUIElement_Impl, ITfDocumentMgr,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfUIElementMgr, ITfUIElement_Impl,
};

#[derive(Default)]
struct UiState {
    manager: Option<ITfUIElementMgr>,
    live: RefCell<Option<LiveUi>>,
    busy: Cell<bool>,
    closed: Rc<Cell<bool>>,
    serial: Cell<u64>,
    last_error: Cell<Option<HRESULT>>,
}

struct LiveUi {
    id: u32,
    data: Rc<UiData>,
}

struct UiData {
    snapshot: RefCell<crate::platform::CandidatePopupSnapshot>,
    context: ITfContext,
    allowed: Cell<bool>,
    shown: Cell<bool>,
    alive: Cell<bool>,
    negotiated: Cell<bool>,
    closed: Rc<Cell<bool>>,
    requested: Cell<Option<bool>>,
    token: ActivationToken,
    owner: ThreadId,
    caret: Cell<Option<PopupRect>>,
    _registration: ObjectRegistration,
}

#[implement(ITfCandidateListUIElement)]
struct CandidateUi {
    data: Rc<UiData>,
}

fn render_ui(data: &UiData) {
    data.shown.set(false);
    if let Some(mut popup) = take_thread_popup(data.token, data.owner) {
        popup.window.cancel_pending_actions();
        if data.alive.get() && data.negotiated.get() && data.allowed.get() {
            let snapshot = data.snapshot.borrow().clone();
            if popup.window.update(&snapshot).is_ok() {
                if let Some(rect) = data.caret.get() {
                    let _ = popup.window.show_at(rect);
                }
            }
        } else {
            popup.window.hide();
        }
        data.shown
            .set(data.alive.get() && data.allowed.get() && popup.window.is_visible());
        if data.closed.get() {
            popup.window.hide();
            drop(popup);
        } else {
            return_thread_popup(data.token, data.owner, popup);
        }
    }
}

impl ITfUIElement_Impl for CandidateUi_Impl {
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("Flower candidates"))
    }
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID::from_u128(0xb84ca94a_03b1_4685_a88f_d98389b9dc42))
    }
    fn Show(&self, show: BOOL) -> Result<()> {
        let data = &self.get_impl().data;
        if std::thread::current().id() != data.owner || !data.alive.get() {
            return Err(E_UNEXPECTED.into());
        }
        data.requested.set(Some(show.as_bool()));
        data.allowed.set(show.as_bool());
        render_ui(data);
        Ok(())
    }
    fn IsShown(&self) -> Result<BOOL> {
        let data = &self.get_impl().data;
        Ok(BOOL::from(data.alive.get() && data.shown.get()))
    }
}

impl ITfCandidateListUIElement_Impl for CandidateUi_Impl {
    fn GetUpdatedFlags(&self) -> Result<u32> {
        // Every notification publishes a complete immutable current-page snapshot.
        Ok(0x3f)
    }
    fn GetDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        unsafe { self.get_impl().data.context.GetDocumentMgr() }
    }
    fn GetCount(&self) -> Result<u32> {
        Ok(self.get_impl().data.snapshot.borrow().candidates.len() as u32)
    }
    fn GetSelection(&self) -> Result<u32> {
        Ok(self
            .get_impl()
            .data
            .snapshot
            .borrow()
            .highlighted
            .unwrap_or(0) as u32)
    }
    fn GetString(&self, index: u32) -> Result<BSTR> {
        self.get_impl()
            .data
            .snapshot
            .borrow()
            .candidates
            .get(index as usize)
            .map(|candidate| BSTR::from(candidate.display_text().as_ref()))
            .ok_or_else(|| E_INVALIDARG.into())
    }
    fn GetPageIndex(&self, index: *mut u32, size: u32, count: *mut u32) -> Result<()> {
        if count.is_null() || (size > 0 && index.is_null()) {
            return Err(E_POINTER.into());
        }
        unsafe {
            count.write(1);
            if size > 0 {
                index.write(0);
            }
        }
        Ok(())
    }
    fn SetPageIndex(&self, index: *const u32, count: u32) -> Result<()> {
        if index.is_null() {
            return Err(E_POINTER.into());
        }
        if count != 1 || unsafe { index.read() } != 0 {
            return Err(E_INVALIDARG.into());
        }
        Ok(())
    }
    fn GetCurrentPage(&self) -> Result<u32> {
        Ok(0)
    }
}

impl UiState {
    fn set_shown(&self, shown: bool) {
        if let Some(live) = self.live.borrow().as_ref() {
            live.data
                .shown
                .set(shown && live.data.alive.get() && live.data.allowed.get());
        }
    }
    fn allowed(&self) -> bool {
        if self.closed.get() || self.busy.get() {
            return false;
        }
        // Legacy internal fixtures have no manager; production requires one.
        if self.manager.is_none() {
            return cfg!(test);
        }
        self.live
            .borrow()
            .as_ref()
            .is_some_and(|live| live.data.alive.get() && live.data.allowed.get())
    }

    fn end(&self) {
        self.serial.set(self.serial.get().wrapping_add(1));
        let live = self.live.borrow_mut().take();
        if let Some(live) = live {
            live.data.alive.set(false);
            live.data.allowed.set(false);
            live.data.shown.set(false);
            render_ui(&live.data);
            if let Some(manager) = &self.manager {
                if let Err(error) = unsafe { manager.EndUIElement(live.id) } {
                    self.last_error.set(Some(error.code()));
                }
            }
        }
    }

    fn close(&self) {
        self.closed.set(true);
        self.end();
    }

    fn publish(
        &self,
        context: &ITfContext,
        snapshot: Option<&crate::platform::CandidatePopupSnapshot>,
        caret: Option<PopupRect>,
        token: ActivationToken,
        owner: ThreadId,
    ) {
        let Some(manager) = &self.manager else {
            return;
        };
        if self.closed.get() {
            return;
        }
        let Some(snapshot) = snapshot.filter(|s| !s.candidates.is_empty()) else {
            self.end();
            return;
        };
        if self.busy.replace(true) {
            return;
        }
        let existing = self
            .live
            .borrow()
            .as_ref()
            .map(|l| (l.id, Rc::clone(&l.data)));
        if let Some((id, data)) = existing {
            if context_key(&data.context).ok() == context_key(context).ok() {
                *data.snapshot.borrow_mut() = snapshot.clone();
                data.caret.set(caret);
                let result = unsafe { manager.UpdateUIElement(id) };
                self.busy.set(false);
                if let Err(error) = result {
                    self.last_error.set(Some(error.code()));
                    self.end();
                }
                return;
            }
            self.end();
        }
        let serial = self.serial.get();
        let data = Rc::new(UiData {
            snapshot: RefCell::new(snapshot.clone()),
            context: context.clone(),
            allowed: Cell::new(false),
            shown: Cell::new(false),
            alive: Cell::new(true),
            negotiated: Cell::new(false),
            closed: Rc::clone(&self.closed),
            requested: Cell::new(None),
            token,
            owner,
            caret: Cell::new(caret),
            _registration: ObjectRegistration::new(),
        });
        let element: ITfCandidateListUIElement = CandidateUi {
            data: Rc::clone(&data),
        }
        .into();
        let mut show = TRUE;
        let mut id = u32::MAX;
        let result = unsafe { manager.BeginUIElement(&element, &mut show, &mut id) };
        self.busy.set(false);
        if result.is_err() || self.closed.get() || self.serial.get() != serial {
            data.alive.set(false);
            data.allowed.set(false);
            render_ui(&data);
            if let Err(error) = &result {
                self.last_error.set(Some(error.code()));
            }
            if id != u32::MAX {
                let _ = unsafe { manager.EndUIElement(id) };
            }
            return;
        }
        data.allowed
            .set(show.as_bool() && data.requested.get().unwrap_or(true));
        data.negotiated.set(true);
        *self.live.borrow_mut() = Some(LiveUi { id, data });
    }
}

fn activation_ui(shared: &SharedActivation, token: ActivationToken) -> Option<Rc<UiState>> {
    shared
        .try_borrow()
        .ok()?
        .as_ref()
        .filter(|a| a.token == token)
        .map(|a| Rc::clone(&a.ui))
}

fn ui_allows(shared: &SharedActivation, token: ActivationToken) -> bool {
    activation_ui(shared, token).is_some_and(|ui| ui.allowed())
}
