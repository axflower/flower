//! Opt-in, debug-only TSF status diagnostics. Never accepts input text.

use windows::core::{Result, HRESULT};

#[derive(Clone, Copy, Debug)]
pub(super) enum Stage {
    LayoutTextExt,
    LayoutScreenExt,
    LayoutViewWindow,
    AnchorBounds,
    AnchorGui,
    AnchorView,
    UiBegin,
    UiPermission,
    UiShow,
    PopupOwner,
    PopupBind,
    PopupOwnedCreate,
    PopupFallback,
    PopupShow,
    PopupVisible,
    Activate,
    ActivateResult,
    UiElementManager,
    AdviseThreadEvents,
    AdviseKeyTrace,
    PopupCreate,
    PopupTheme,
    PopupActionSink,
    AdviseKeySink,
    ActivationReady,
    TestKeyDown,
    KeyDown,
    KeyDone,
    KeyModeQuery,
    KeyModeReady,
    KeyRoute,
    KeyLexiconBegin,
    TestKeyFocus,
    KeyFocus,
    KeyLexicon,
    RequestKeyEdit,
    EditSessionResult,
    DoEditSession,
    EditTargetRejected,
    QueryInsert,
    QueryInsertionRange,
    StartComposition,
    SetInitialText,
    ApplyUpdate,
    CommitUpdate,
    LearningAppProperty,
    LearningScopeValue,
}

pub(super) fn record_result<T>(stage: Stage, result: &Result<T>) {
    record(
        stage,
        result
            .as_ref()
            .err()
            .map_or(HRESULT(0), |error| error.code()),
    );
}

pub(super) fn record(stage: Stage, status: HRESULT) {
    #[cfg(debug_assertions)]
    {
        use std::io::Write;

        let Some(path) = diagnostic_path(option_env!("FLOWER_TSF_DIAGNOSTIC_FILE")) else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", status_line(std::process::id(), stage, status));
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = (stage, status);
}

#[cfg(any(debug_assertions, test))]
fn diagnostic_path(value: Option<&str>) -> Option<&std::path::Path> {
    use std::path::{Component, Prefix};

    value.map(std::path::Path::new).filter(|path| {
        path.is_absolute()
            && matches!(
                path.components().next(),
                Some(Component::Prefix(prefix))
                    if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            )
    })
}

#[cfg(any(debug_assertions, test))]
fn status_line(process: u32, stage: Stage, status: HRESULT) -> String {
    format!(
        "FlowerTSF pid={process} stage={stage:?} hr=0x{:08X}",
        status.0 as u32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_require_an_explicit_absolute_path() {
        assert!(diagnostic_path(None).is_none());
        assert!(diagnostic_path(Some("")).is_none());
        assert!(diagnostic_path(Some("flower.log")).is_none());
        assert!(diagnostic_path(Some(r"C:flower.log")).is_none());
        assert!(diagnostic_path(Some(r"\flower.log")).is_none());
        assert!(diagnostic_path(Some(r"\\server\share\flower.log")).is_none());
        assert!(diagnostic_path(Some(r"\\?\UNC\server\share\flower.log")).is_none());
        assert!(diagnostic_path(Some(r"\\.\pipe\flower")).is_none());
        assert_eq!(
            diagnostic_path(Some(r"C:\diagnostics\flower.log")),
            Some(std::path::Path::new(r"C:\diagnostics\flower.log"))
        );
        assert!(diagnostic_path(Some(r"\\?\C:\diagnostics\flower.log")).is_some());
    }

    #[test]
    fn diagnostic_records_contain_only_fixed_status_fields() {
        assert_eq!(
            status_line(42, Stage::RequestKeyEdit, HRESULT(0x80040208_u32 as i32)),
            "FlowerTSF pid=42 stage=RequestKeyEdit hr=0x80040208"
        );
    }
}
