// Included in native_tests. Only owned TSF documents and this thread's keyboard table.

struct RoutedKeys {
    trace: KeyTraceEvents,
    sink: ITfKeyEventSink,
    shared: SharedActivation,
}
impl RoutedKeys {
    fn new(
        host: &NativeDocument,
        service: &ITfTextInputProcessor,
        shared: &SharedActivation,
    ) -> Self {
        let token = shared.borrow().as_ref().unwrap().token;
        Self {
            trace: KeyTraceEvents::subscribe(&host.manager, shared, token).unwrap(),
            sink: service.cast().unwrap(),
            shared: shared.clone(),
        }
    }
    fn keyboard(modifiers: Modifiers, caps: bool) -> LocalKeyboard {
        let guard = LocalKeyboard::set(modifiers);
        let mut keys = [0u8; 256];
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardState(&mut keys) }
            .unwrap();
        keys[0x14] = u8::from(caps);
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetKeyboardState(&keys) }.unwrap();
        guard
    }
    fn down(&self, context: &ITfContext, vk: usize, modifiers: Modifiers, caps: bool) -> bool {
        let _keyboard = Self::keyboard(modifiers, caps);
        unsafe { self.trace.sink.OnKeyTraceDown(WPARAM(vk), LPARAM(0)) }.unwrap();
        let before = self
            .shared
            .borrow()
            .as_ref()
            .map(|a| (a.english, a.writing_gesture.held, a.shift.pressed));
        let expected = unsafe { self.sink.OnTestKeyDown(context, WPARAM(vk), LPARAM(0)) }.unwrap();
        for _ in 0..2 {
            assert_eq!(
                unsafe { self.sink.OnTestKeyDown(context, WPARAM(vk), LPARAM(0)) }.unwrap(),
                expected
            );
        }
        assert_eq!(
            self.shared.borrow().as_ref().map(|a| (
                a.english,
                a.writing_gesture.held,
                a.shift.pressed
            )),
            before
        );
        if expected.as_bool() {
            assert!(
                unsafe { self.sink.OnKeyDown(context, WPARAM(vk), LPARAM(0)) }
                    .unwrap()
                    .as_bool()
            );
        }
        expected.as_bool()
    }
    fn up(&self, context: &ITfContext, vk: usize, modifiers: Modifiers, caps: bool) {
        let _keyboard = Self::keyboard(modifiers, caps);
        unsafe { self.trace.sink.OnKeyTraceUp(WPARAM(vk), LPARAM(0)) }.unwrap();
        // An application must receive balanced native modifier releases.
        assert!(
            !unsafe { self.sink.OnTestKeyUp(context, WPARAM(vk), LPARAM(0)) }
                .unwrap()
                .as_bool()
        );
    }
}

#[test]
fn native_trace_commands_and_english_symbols_are_never_claimed() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let host = NativeDocument::new().unwrap();
    let (service, shared) = basic_service(&host);
    let keys = RoutedKeys::new(&host, &service, &shared);
    for composing in [false, true] {
        if composing {
            assert!(keys.down(&host.context, b'N' as usize, Modifiers::NONE, false));
            assert!(keys.down(&host.context, b'I' as usize, Modifiers::NONE, false));
        }
        let before = read_document(&host, None);
        for caps in [false, true] {
            for vk in b"CVXAZY" {
                assert!(!keys.down(&host.context, 0xA2, Modifiers::CONTROL, caps));
                assert!(!keys.down(&host.context, *vk as usize, Modifiers::CONTROL, caps));
                keys.up(&host.context, *vk as usize, Modifiers::CONTROL, caps);
                keys.up(&host.context, 0xA2, Modifiers::NONE, caps);
                assert_eq!(read_document(&host, None), before);
                assert!(!shared.borrow().as_ref().unwrap().english);
            }
        }
    }
    // Flush only the test's current spelling before testing a base-English state.
    assert!(apply_engine_key(&shared, &host.context, None, false)
        .unwrap()
        .as_bool());
    shared.borrow_mut().as_mut().unwrap().english = true;
    let before = read_document(&host, None);
    let native: Vec<usize> = (0x30..=0x5a)
        .chain(0x60..=0x6f)
        .chain([
            0x20, 0x08, 0x0d, 0x25, 0x26, 0x27, 0x28, 0x2e, 0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf,
            0xc0, 0xdb, 0xdc, 0xdd, 0xde,
        ])
        .collect();
    for caps in [false, true] {
        for modifiers in [Modifiers::NONE, Modifiers::SHIFT] {
            for vk in &native {
                if modifiers.shift {
                    assert!(!keys.down(&host.context, 0xa0, modifiers, caps));
                }
                assert!(
                    !keys.down(&host.context, *vk, modifiers, caps),
                    "native vk={vk:x}"
                );
                keys.up(&host.context, *vk, modifiers, caps);
                if modifiers.shift {
                    keys.up(&host.context, 0xa0, Modifiers::NONE, caps);
                }
                assert!(shared.borrow().as_ref().unwrap().english);
                assert_eq!(read_document(&host, None), before);
            }
        }
    }
}

#[test]
fn native_trace_shift_uppercase_and_caps_restore_base_mode() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let host = NativeDocument::new().unwrap();
    let (service, shared) = basic_service(&host);
    let keys = RoutedKeys::new(&host, &service, &shared);
    assert!(!keys.down(&host.context, 0xa0, Modifiers::SHIFT, false));
    assert!(keys.down(&host.context, b'V' as usize, Modifiers::SHIFT, false));
    keys.up(&host.context, b'V' as usize, Modifiers::SHIFT, false);
    keys.up(&host.context, 0xa0, Modifiers::NONE, false);
    assert_eq!(read_document(&host, None), "V");
    assert!(!shared.borrow().as_ref().unwrap().english);
    for c in b"NI" {
        assert!(keys.down(&host.context, *c as usize, Modifiers::NONE, false));
    }
    assert!(!keys.down(&host.context, 0x14, Modifiers::NONE, true));
    assert_eq!(read_document(&host, None), "Vni");
    let key = context_key(&host.context);
    assert!(!shared.borrow().as_ref().unwrap().contexts[&key]
        .router
        .is_composing());
    assert!(shared.borrow().as_ref().unwrap().contexts[&key]
        .router
        .pending_learning()
        .is_empty());
    assert!(!keys.down(&host.context, b'A' as usize, Modifiers::NONE, true));
    assert!(!keys.down(&host.context, 0xa0, Modifiers::SHIFT, true));
    keys.up(&host.context, 0xa0, Modifiers::NONE, true);
    assert!(!shared.borrow().as_ref().unwrap().english);
    assert!(!keys.down(&host.context, 0x14, Modifiers::NONE, false));
    assert!(keys.down(&host.context, b'N' as usize, Modifiers::NONE, false));
    assert_eq!(read_document(&host, None), "Vnin");
    assert!(apply_engine_key(&shared, &host.context, None, false)
        .unwrap()
        .as_bool());
    shared.borrow_mut().as_mut().unwrap().english = true;
    assert!(!keys.down(&host.context, 0x14, Modifiers::NONE, true));
    assert!(!keys.down(&host.context, 0x14, Modifiers::NONE, false));
    assert!(shared.borrow().as_ref().unwrap().english);
}

#[test]
fn native_trace_standalone_shift_and_focus_invalidation() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let host = NativeDocument::new().unwrap();
    let (service, shared) = basic_service(&host);
    let keys = RoutedKeys::new(&host, &service, &shared);
    assert!(!keys.down(&host.context, 0xa0, Modifiers::SHIFT, false));
    keys.up(&host.context, 0xa0, Modifiers::NONE, false);
    assert!(shared.borrow().as_ref().unwrap().english);
    assert!(!keys.down(&host.context, 0xa0, Modifiers::SHIFT, false));
    assert!(!keys.down(&host.context, b'V' as usize, Modifiers::SHIFT, false));
    keys.up(&host.context, b'V' as usize, Modifiers::SHIFT, false);
    keys.up(&host.context, 0xa0, Modifiers::NONE, false);
    assert!(shared.borrow().as_ref().unwrap().english);
    assert!(!keys.down(&host.context, 0xa0, Modifiers::SHIFT, false));
    unsafe { keys.sink.OnSetFocus(FALSE) }.unwrap();
    unsafe { keys.sink.OnSetFocus(TRUE) }.unwrap();
    keys.up(&host.context, 0xa0, Modifiers::NONE, false);
    assert!(shared.borrow().as_ref().unwrap().english);
}

#[test]
fn native_trace_subscription_releases_observer_and_stale_token_is_inert() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .unwrap();
    let _apartment = Apartment;
    let host = NativeDocument::new().unwrap();
    let (_service, shared) = basic_service(&host);
    let token = shared.borrow().as_ref().unwrap().token;
    let before = OBJECT_COUNT.load(Ordering::SeqCst);
    let trace = KeyTraceEvents::subscribe(&host.manager, &shared, token).unwrap();
    assert_eq!(OBJECT_COUNT.load(Ordering::SeqCst), before + 1);
    shared.borrow_mut().as_mut().unwrap().token = next_activation_token();
    let _keyboard = RoutedKeys::keyboard(Modifiers::SHIFT, false);
    unsafe { trace.sink.OnKeyTraceDown(WPARAM(0xa0), LPARAM(0)) }.unwrap();
    assert!(shared.borrow().as_ref().unwrap().shift.pressed.is_none());
    drop(trace);
    assert_eq!(OBJECT_COUNT.load(Ordering::SeqCst), before);
}
// Legacy low-level tests explicitly drive observation before their old direct
// callbacks. Return value for release records the action, NOT native consumption.
unsafe fn legacy_trace_key_down(
    shared: &SharedActivation,
    sink: &ITfKeyEventSink,
    context: &ITfContext,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Result<BOOL> {
    observe_key_down(shared, Some(context), wparam, lparam)?;
    sink.OnKeyDown(context, wparam, lparam)
}
unsafe fn legacy_trace_key_up(
    shared: &SharedActivation,
    sink: &ITfKeyEventSink,
    context: &ITfContext,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Result<BOOL> {
    let action = observe_key_up(shared, Some(context), wparam, lparam)?;
    assert!(!sink.OnKeyUp(context, wparam, lparam)?.as_bool());
    Ok(action)
}
#[test]
fn writing_modifier_expansion_preserves_release_order_and_cancellation() {
    let shortcuts = crate::settings::WritingShortcuts {
        expand: Some(crate::settings::WritingShortcut {
            key: 0,
            modifiers: 5,
        }),
        ..Default::default()
    };
    let binding = (7, 1, 2, 3);
    for order in [[1, 4], [4, 1]] {
        for cancel in [false, true] {
            let mut gesture = WritingGesture::default();
            gesture.down(order[0], false, false, binding, shortcuts);
            gesture.down(order[1], false, false, binding, shortcuts);
            if cancel {
                gesture.down(0, false, false, binding, shortcuts);
            }
            assert_eq!(gesture.up(order[0], Some(binding)), None);
            assert_eq!(
                gesture.up(order[1], Some(binding)),
                if cancel {
                    None
                } else {
                    Some(crate::writing::Task::expand())
                }
            );
            assert_eq!(gesture.held, 0);
        }
    }
}
