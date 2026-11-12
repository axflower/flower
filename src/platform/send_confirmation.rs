//! Owned native confirmation; no common-controls manifest dependency.
use windows::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{GetStockObject, GetSysColorBrush, COLOR_BTNFACE, DEFAULT_GUI_FONT},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                EnableWindow, GetFocus, IsWindowEnabled, SetFocus, VK_ESCAPE, VK_RETURN,
            },
            WindowsAndMessaging::*,
        },
    },
};

#[derive(Debug, PartialEq, Eq)]
pub enum SendDecision {
    Cancel,
    Send { dont_ask_again: bool },
}

/// Only explicit send may persist consent. Error means send is allowed but not remembered.
#[cfg(test)]
pub(super) fn apply_decision(
    store: Option<&crate::settings::SettingsStore>,
    decision: SendDecision,
) -> Result<bool, String> {
    apply_decision_for(
        store,
        &crate::writing::config::ApiProfile::default(),
        decision,
    )
}

pub(super) fn apply_decision_for(
    store: Option<&crate::settings::SettingsStore>,
    profile: &crate::writing::config::ApiProfile,
    decision: SendDecision,
) -> Result<bool, String> {
    match decision {
        SendDecision::Cancel => Ok(false),
        SendDecision::Send {
            dont_ask_again: false,
        } => Ok(true),
        SendDecision::Send {
            dont_ask_again: true,
        } => {
            let store = store.ok_or("本次可发送，未记住偏好。")?;
            crate::writing::consent::remember_for(store, profile, true)
                .map_err(|_| "本次可发送，未记住偏好。".to_owned())?;
            Ok(true)
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            // The loop owns destruction and restoration of the owner.
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 1);
            LRESULT(0)
        }
        WM_COMMAND if wp.0 >> 16 == BN_CLICKED as usize => {
            match wp.0 & 0xffff {
                1 => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 2);
                }
                2 => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 1);
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
pub(super) fn confirm_with(
    owner: HWND,
    after_show: impl FnOnce(HWND),
) -> Result<SendDecision, String> {
    confirm_with_for(
        owner,
        &crate::writing::config::ApiProfile::default(),
        after_show,
    )
}

pub fn confirm_send_for(
    owner: HWND,
    profile: &crate::writing::config::ApiProfile,
) -> Result<SendDecision, String> {
    confirm_with_for(owner, profile, |_| {})
}

pub(super) fn confirm_with_for(
    owner: HWND,
    profile: &crate::writing::config::ApiProfile,
    after_show: impl FnOnce(HWND),
) -> Result<SendDecision, String> {
    let profile = profile.normalized().map_err(|e| e.to_string())?;
    let protocol = match profile.protocol {
        crate::writing::config::ApiProtocol::ChatCompletions => "Chat Completions",
        crate::writing::config::ApiProtocol::Responses => "Responses",
    };
    let disclosure: Vec<u16> = format!(
        "本次文本将发送至：{}\n模型：{} · 协议：{}\n可能消耗账号额度或产生费用。数据留存、训练及地域处理受服务商隐私政策与条款约束，Flower 不保证零留存或不训练。\n取消无法撤回已发送文本。是否同意发送？",
        profile.base_url, profile.model, protocol
    ).encode_utf16().chain(Some(0)).collect();
    unsafe {
        let module = GetModuleHandleW(None).map_err(|_| "无法创建发送确认。")?;
        let class = w!("Flower.SendConfirmation.1");
        RegisterClassExW(&WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: HINSTANCE(module.0),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        });
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT,
            class,
            w!("确认在线发送"),
            WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            660,
            310,
            owner,
            None,
            module,
            None,
        )
        .map_err(|_| "无法创建发送确认。")?;
        let controls = (|| -> windows::core::Result<(HWND, HWND)> {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                windows::core::PCWSTR(disclosure.as_ptr()),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_VSCROLL
                    | WS_TABSTOP
                    | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                20,
                20,
                605,
                150,
                hwnd,
                None,
                module,
                None,
            )?;
            let check = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!("不再提示"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
                20,
                180,
                260,
                30,
                hwnd,
                HMENU(3 as *mut _),
                module,
                None,
            )?;
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!("发送"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                390,
                220,
                100,
                30,
                hwnd,
                HMENU(1 as *mut _),
                module,
                None,
            )?;
            let cancel = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!("取消"),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
                510,
                220,
                100,
                30,
                hwnd,
                HMENU(2 as *mut _),
                module,
                None,
            )?;
            Ok((check, cancel))
        })();
        let (check, cancel) = match controls {
            Ok(value) => value,
            Err(_) => {
                let _ = DestroyWindow(hwnd);
                return Err("无法创建发送确认。".into());
            }
        };
        let font = GetStockObject(DEFAULT_GUI_FONT);
        let mut child = GetWindow(hwnd, GW_CHILD).unwrap_or_default();
        while !child.is_invalid() {
            SendMessageW(child, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
        }
        let restore = !owner.is_invalid() && IsWindowEnabled(owner).as_bool();
        if restore {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetFocus(cancel);
        after_show(hwnd);
        let mut error = false;
        let mut quit = None;
        while IsWindow(hwnd).as_bool() && GetWindowLongPtrW(hwnd, GWLP_USERDATA) == 0 {
            let mut msg = MSG::default();
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            if result <= 0 {
                if result == 0 {
                    quit = Some(msg.wParam.0 as i32);
                } else {
                    error = true;
                }
                break;
            }
            if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_ESCAPE.0 as usize {
                break;
            }
            if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_RETURN.0 as usize {
                let command = if GetDlgCtrlID(GetFocus()) == 1 { 1 } else { 2 };
                SendMessageW(hwnd, WM_COMMAND, WPARAM(command), LPARAM(0));
                continue;
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let decision = if IsWindow(hwnd).as_bool() && GetWindowLongPtrW(hwnd, GWLP_USERDATA) == 2 {
            SendDecision::Send {
                dont_ask_again: SendMessageW(check, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1,
            }
        } else {
            SendDecision::Cancel
        };
        if restore && IsWindow(owner).as_bool() {
            let _ = EnableWindow(owner, true);
        }
        let _ = DestroyWindow(hwnd);
        if restore && IsWindow(owner).as_bool() {
            let _ = SetFocus(owner);
        }
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
        if error {
            Err("发送确认失败，未发送文本。".into())
        } else {
            Ok(decision)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "creates only owned native confirmation windows"]
    fn native_custom_protocol_disclosure_uses_validated_snapshot() {
        use crate::writing::config::{ApiProfile, ApiProtocol};
        for protocol in [ApiProtocol::ChatCompletions, ApiProtocol::Responses] {
            let profile = ApiProfile {
                id: "fixture".into(),
                name: "合成连接".into(),
                protocol,
                base_url: "https://fixture.example/v1/".into(),
                model: "fixture-model".into(),
            };
            let answer = confirm_with_for(HWND::default(), &profile, |hwnd| unsafe {
                let body = GetWindow(hwnd, GW_CHILD).unwrap();
                let mut text = vec![0u16; GetWindowTextLengthW(body) as usize + 1];
                let count = GetWindowTextW(body, &mut text);
                let text = String::from_utf16_lossy(&text[..count as usize]);
                assert!(text.contains("https://fixture.example/v1"));
                assert!(text.contains("fixture-model"));
                assert!(text.contains(match protocol {
                    ApiProtocol::ChatCompletions => "Chat Completions",
                    ApiProtocol::Responses => "Responses",
                }));
                assert!(text.contains("费用"));
                assert!(text.contains("隐私政策"));
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            })
            .unwrap();
            assert_eq!(answer, SendDecision::Cancel);
        }
    }
    use super::*;

    #[test]
    #[ignore = "isolated file storage; explicit session scratch directory required"]
    fn isolated_decisions_and_latest_preferences() {
        use crate::{
            settings::{Settings, SettingsStore},
            writing::consent::*,
        };
        let directory = std::path::PathBuf::from(
            std::env::var_os("FLOWER_CONSENT_TEST_DIR")
                .expect("explicit session scratch directory required"),
        );
        std::fs::create_dir_all(&directory).unwrap();
        let store = SettingsStore::new(directory.join("consent.json"));
        store.save(&Settings::default()).unwrap();
        assert!(confirmation_required(Some(&store)));
        assert!(confirmation_required(None));
        assert!(!apply_decision(Some(&store), SendDecision::Cancel).unwrap());
        assert!(confirmation_required(Some(&store)));
        assert!(apply_decision(
            Some(&store),
            SendDecision::Send {
                dont_ask_again: false
            }
        )
        .unwrap());
        assert!(confirmation_required(Some(&store)));
        let mut latest = store.load().unwrap();
        latest.writing_shortcuts.draft.key = 0x33;
        latest.online_polish.enabled = true;
        store.save(&latest).unwrap();
        assert!(apply_decision(
            Some(&store),
            SendDecision::Send {
                dont_ask_again: true
            }
        )
        .unwrap());
        let reloaded = SettingsStore::new(store.path());
        assert!(!confirmation_required(Some(&reloaded)));
        let remembered = reloaded.load().unwrap();
        assert_eq!(remembered.writing_shortcuts, latest.writing_shortcuts);
        assert_eq!(remembered.online_polish, latest.online_polish);
        remember(&store, false).unwrap();
        assert!(confirmation_required(Some(&reloaded)));
        latest.writing_send_consent = Some("other/model/v1".into());
        store.save(&latest).unwrap();
        assert!(confirmation_required(Some(&store)));
        let failing = SettingsStore::new(directory.join("blocked"));
        std::fs::create_dir_all(failing.path()).unwrap();
        assert!(apply_decision(
            Some(&failing),
            SendDecision::Send {
                dont_ask_again: true
            }
        )
        .is_err());
        assert!(confirmation_required(Some(&failing)));
    }

    #[test]
    #[ignore = "creates owned native windows; run explicitly on Windows desktop"]
    fn native_checkbox_send_cancel_escape_close_restore_owner() {
        unsafe {
            let module = GetModuleHandleW(None).unwrap();
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("isolated owner"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                50,
                50,
                None,
                None,
                module,
                None,
            )
            .unwrap();
            for action in 0..6 {
                let decision = confirm_with(owner, |hwnd| {
                    assert!(!IsWindowEnabled(owner).as_bool());
                    let check = GetDlgItem(hwnd, 3).unwrap();
                    assert_eq!(SendMessageW(check, BM_GETCHECK, WPARAM(0), LPARAM(0)).0, 0);
                    if action != 0 {
                        SendMessageW(check, BM_CLICK, WPARAM(0), LPARAM(0));
                        assert_eq!(SendMessageW(check, BM_GETCHECK, WPARAM(0), LPARAM(0)).0, 1);
                    }
                    match action {
                        0 | 1 => {
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(1), LPARAM(0));
                        }
                        2 => {
                            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(2), LPARAM(0));
                        }
                        3 => {
                            let _ = PostMessageW(
                                hwnd,
                                WM_KEYDOWN,
                                WPARAM(VK_ESCAPE.0 as usize),
                                LPARAM(0),
                            );
                        }
                        5 => {
                            let _ = PostMessageW(
                                hwnd,
                                WM_KEYDOWN,
                                WPARAM(VK_RETURN.0 as usize),
                                LPARAM(0),
                            );
                        }
                        _ => {
                            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                })
                .unwrap();
                assert_eq!(
                    decision,
                    match action {
                        0 => SendDecision::Send {
                            dont_ask_again: false
                        },
                        1 => SendDecision::Send {
                            dont_ask_again: true
                        },
                        _ => SendDecision::Cancel,
                    }
                );
                assert!(IsWindowEnabled(owner).as_bool());
            }
            DestroyWindow(owner).unwrap();
        }
    }
}
