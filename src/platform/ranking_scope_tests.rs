// Included in the native TSF tests. All ranges belong to an owned real document;
// only its application property provider is an isolated COM fixture.
use windows::Win32::UI::TextServices::{
    IEnumTfRanges, IEnumTfRanges_Impl, ITfReadOnlyProperty, ITfReadOnlyProperty_Impl,
};

thread_local! {
    static RANGE_SCOPE_PROPERTY: RefCell<Option<ITfReadOnlyProperty>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug)]
enum RangeScopeCase {
    Safe,
    ClippedSafe,
    Sensitive(i32),
    TailSensitive(i32),
    Undeclared,
    Empty,
    PointFallback,
    NoSegments,
    MissingTail,
    Gap,
    Overlap,
    Reversed,
    FailEnum,
    FailNext,
    FailValue,
}

fn fixture_scope_range(
    context: &ITfContext,
    cookie: u32,
    start: i32,
    end: i32,
) -> Result<ITfRange> {
    unsafe {
        let range = context.GetStart(cookie)?.Clone()?;
        let mut moved = 0;
        range.ShiftEnd(cookie, end, &mut moved, ptr::null())?;
        if moved != end {
            return Err(E_UNEXPECTED.into());
        }
        range.ShiftStart(cookie, start, &mut moved, ptr::null())?;
        if moved != start {
            return Err(E_UNEXPECTED.into());
        }
        Ok(range)
    }
}

#[implement(IEnumTfRanges)]
struct RangeScopeEnumeration {
    ranges: Vec<ITfRange>,
    index: Cell<usize>,
    fail_after: Option<usize>,
}

impl IEnumTfRanges_Impl for RangeScopeEnumeration_Impl {
    fn Clone(&self) -> Result<IEnumTfRanges> {
        Ok(RangeScopeEnumeration {
            ranges: self.ranges.clone(),
            index: Cell::new(self.index.get()),
            fail_after: self.fail_after,
        }
        .into())
    }
    fn Next(&self, count: u32, ranges: *mut Option<ITfRange>, fetched: *mut u32) -> Result<()> {
        if count != 1 || ranges.is_null() || fetched.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe {
            *fetched = 0;
        }
        if self.fail_after == Some(self.index.get()) {
            return Err(E_UNEXPECTED.into());
        }
        if let Some(range) = self.ranges.get(self.index.get()) {
            unsafe {
                ranges.write(Some(range.clone()));
                *fetched = 1;
            }
            self.index.set(self.index.get() + 1);
        }
        Ok(())
    }
    fn Reset(&self) -> Result<()> {
        self.index.set(0);
        Ok(())
    }
    fn Skip(&self, count: u32) -> Result<()> {
        self.index.set(
            self.index
                .get()
                .saturating_add(count as usize)
                .min(self.ranges.len()),
        );
        Ok(())
    }
}

#[implement(ITfReadOnlyProperty)]
struct RangeScopeProperty {
    context: ITfContext,
    case: RangeScopeCase,
    queries: Rc<RefCell<Vec<usize>>>,
}

impl RangeScopeProperty {
    fn intervals(&self) -> [(i32, i32); 2] {
        match self.case {
            RangeScopeCase::ClippedSafe => [(0, 40), (40, 80)],
            RangeScopeCase::Gap => [(0, 1), (2, 4)],
            RangeScopeCase::Overlap => [(0, 3), (2, 4)],
            RangeScopeCase::Reversed => [(2, 4), (0, 2)],
            _ => [(0, 2), (2, 4)],
        }
    }
}

impl ITfReadOnlyProperty_Impl for RangeScopeProperty_Impl {
    fn GetType(&self) -> Result<GUID> {
        Ok(GUID_PROP_INPUTSCOPE)
    }
    fn GetContext(&self) -> Result<ITfContext> {
        Ok(self.context.clone())
    }
    fn EnumRanges(
        &self,
        cookie: u32,
        result: *mut Option<IEnumTfRanges>,
        target: Option<&ITfRange>,
    ) -> Result<()> {
        if matches!(self.case, RangeScopeCase::FailEnum) {
            return Err(E_UNEXPECTED.into());
        }
        let target = target.ok_or_else(|| Error::from(E_INVALIDARG))?;
        let (start, end) = if matches!(self.case, RangeScopeCase::ClippedSafe) {
            (16, 80)
        } else {
            (0, 4)
        };
        let expected = fixture_scope_range(&self.context, cookie, start, end)?;
        unsafe {
            assert_eq!(target.CompareStart(cookie, &expected, TF_ANCHOR_START)?, 0);
            assert_eq!(target.CompareEnd(cookie, &expected, TF_ANCHOR_END)?, 0);
        }
        let mut ranges = self
            .intervals()
            .into_iter()
            .map(|(start, end)| fixture_scope_range(&self.context, cookie, start, end))
            .collect::<Result<Vec<_>>>()?;
        if matches!(self.case, RangeScopeCase::NoSegments) {
            ranges.clear();
        }
        if matches!(self.case, RangeScopeCase::MissingTail) {
            ranges.truncate(1);
        }
        unsafe {
            *result = Some(
                RangeScopeEnumeration {
                    ranges,
                    index: Cell::new(0),
                    fail_after: matches!(self.case, RangeScopeCase::FailNext).then_some(1),
                }
                .into(),
            );
        }
        Ok(())
    }
    fn GetValue(&self, cookie: u32, range: Option<&ITfRange>) -> Result<VARIANT> {
        let range = range.ok_or_else(|| Error::from(E_INVALIDARG))?;
        if matches!(self.case, RangeScopeCase::PointFallback) {
            if !unsafe { range.IsEmpty(cookie)? }.as_bool() {
                self.queries.borrow_mut().push(0);
            }
            return Ok(VARIANT::default());
        }
        // The current collapsed selection is safe in every case. A single
        // aggregate GetValue over the whole range also cannot expose the unsafe
        // interior: the production code must enumerate and query each segment.
        if !unsafe { range.IsEmpty(cookie)? }.as_bool() {
            for (index, (start, end)) in self.intervals().into_iter().enumerate() {
                let segment = fixture_scope_range(&self.context, cookie, start, end)?;
                if unsafe {
                    range.CompareStart(cookie, &segment, TF_ANCHOR_START)? >= 0
                        && range.CompareEnd(cookie, &segment, TF_ANCHOR_END)? <= 0
                } {
                    self.queries.borrow_mut().push(index);
                    if let (1, RangeScopeCase::TailSensitive(value)) = (index, self.case) {
                        let scope: IUnknown = TestScope(vec![value]).into();
                        return Ok(VARIANT::from(scope));
                    }
                    if index == 0 {
                        match self.case {
                            RangeScopeCase::FailValue => return Err(E_UNEXPECTED.into()),
                            RangeScopeCase::Undeclared => return Ok(VARIANT::default()),
                            RangeScopeCase::Empty => {
                                let scope: IUnknown = TestScope(Vec::new()).into();
                                return Ok(VARIANT::from(scope));
                            }
                            RangeScopeCase::Sensitive(value) => {
                                let scope: IUnknown = TestScope(vec![value]).into();
                                return Ok(VARIANT::from(scope));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let scope: IUnknown = TestScope(vec![0]).into();
        Ok(VARIANT::from(scope))
    }
}

#[test]
fn native_ranking_prefix_checks_every_real_range_segment_before_reading() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
    };
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    struct ViewGuard(HWND, HWND);
    impl Drop for ViewGuard {
        fn drop(&mut self) {
            COMPAT_VIEW.with(|v| v.set(HWND::default()));
            unsafe {
                let _ = SetFocus(self.1);
                let _ = DestroyWindow(self.0);
            }
        }
    }
    let previous = unsafe { GetFocus() };
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("Edit"),
            windows::core::w!(""),
            WS_POPUP,
            0,
            0,
            160,
            40,
            None,
            None,
            None,
            None,
        )
    }
    .unwrap();
    let _view = ViewGuard(window, previous);
    unsafe {
        let _ = SetFocus(window);
    }
    COMPAT_VIEW.with(|v| v.set(window));
    struct PropertyGuard;
    impl Drop for PropertyGuard {
        fn drop(&mut self) {
            RANGE_SCOPE_PROPERTY.with(|p| p.borrow_mut().take());
        }
    }
    for case in [
        RangeScopeCase::Sensitive(31),
        RangeScopeCase::Safe,
        RangeScopeCase::ClippedSafe,
        RangeScopeCase::Sensitive(61),
        RangeScopeCase::TailSensitive(61),
        RangeScopeCase::Sensitive(999),
        RangeScopeCase::Undeclared,
        RangeScopeCase::Empty,
        RangeScopeCase::PointFallback,
        RangeScopeCase::NoSegments,
        RangeScopeCase::MissingTail,
        RangeScopeCase::Gap,
        RangeScopeCase::Overlap,
        RangeScopeCase::Reversed,
        RangeScopeCase::FailEnum,
        RangeScopeCase::FailNext,
        RangeScopeCase::FailValue,
    ] {
        let safe: IUnknown = TestScope(vec![0]).into();
        let owner: IUnknown = ScopeOwner {
            value: RefCell::new(VARIANT::from(safe)),
            calls: Rc::new(Cell::new(0)),
            fail: false,
        }
        .into();
        let host = NativeDocument::with_owner(Some(&owner)).unwrap();
        let (_service, shared) = basic_service(&host);
        let document = if matches!(case, RangeScopeCase::ClippedSafe) {
            "甲".repeat(80)
        } else {
            "隐私安全".into()
        };
        writing_fixture_edit(&host, Some(&document), false);
        let queries = Rc::new(RefCell::new(Vec::new()));
        RANGE_SCOPE_PROPERTY.with(|slot| {
            *slot.borrow_mut() = Some(
                RangeScopeProperty {
                    context: host.context.clone(),
                    case,
                    queries: queries.clone(),
                }
                .into(),
            )
        });
        let _property = PropertyGuard;
        let mut alias = IdentityAlias {
            vtable: &ALIAS_VTABLE,
            identity: host.context.cast().unwrap(),
            context: host.context.clone(),
            fail: false,
        };
        // This borrowed tear-off stays alive for the entire synchronous request.
        let context = std::mem::ManuallyDrop::new(unsafe {
            ITfContext::from_raw((&mut alias as *mut IdentityAlias).cast())
        });
        let target = {
            let state = shared.borrow();
            let active = state.as_ref().unwrap();
            EditTarget {
                context_generation: Some(active.context_generation.get()),
                switch_to_english: false,
                promotion_claim: Rc::new(Cell::new(false)),
                activation: shared.clone(),
                thread_manager: host.manager.clone(),
                token: active.token,
                owner_thread: active.owner_thread,
                key: super::context_key(&host.context).unwrap(),
                focus_epoch: active.focus_epoch,
            }
        };
        let before = RANKING_BODY_READS.with(Cell::get);
        let actual = capture_ranking_prefix(&context, host.client, None, target, true);
        let allowed = matches!(case, RangeScopeCase::Safe | RangeScopeCase::ClippedSafe);
        let expected = match case {
            RangeScopeCase::Safe => "隐私安全".to_owned(),
            RangeScopeCase::ClippedSafe => "甲".repeat(32),
            _ => String::new(),
        };
        assert_eq!(actual, expected, "{case:?}");
        assert_eq!(
            RANKING_BODY_READS.with(Cell::get) - before,
            usize::from(allowed),
            "{case:?}: denied ranges must not call GetText"
        );
        if allowed {
            assert_eq!(*queries.borrow(), vec![0, 1]);
        }
        if matches!(case, RangeScopeCase::PointFallback) {
            assert_eq!(*queries.borrow(),vec![0],"the native selection fallback was accepted, but cannot authorize the expanded range");
        }
        assert_eq!(read_document(&host, None), document);
        shared.borrow_mut().take();
    }
}
