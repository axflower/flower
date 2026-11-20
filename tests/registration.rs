use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use flower::registration::{
    default_dll_path, execute_com_deletion, execute_registration, execute_unregistration,
    execute_user_layout, make_absolute_path, parse_arguments, plan_registration, validate_pe_dll,
    validate_wide_dll_path, CliCommand, ComDeletionBackend, InstallationSnapshot, OperationOutcome,
    RegistrationBackend, RegistrationConflict, RegistrationResource, ResourceInstallation,
    ResourceState, UserLayoutBackend, UserLayoutOperation, FLOWER_INSTALL_LAYOUT_OR_TIP_PROFILE,
    FLOWER_LANGID_ZH_CN, FLOWER_LANGUAGE_PROFILE_DESCRIPTION, FLOWER_TEXT_SERVICE_CLSID_U128,
    ILOT_UNINSTALL,
};

#[test]
fn user_file_commands_require_exact_path_arguments() {
    for command in ["check-user-install-path", "install-user-files"] {
        let path = OsString::from(r"D:\software\Flower");
        let expected = if command == "check-user-install-path" {
            CliCommand::CheckUserInstallPath { path: path.clone() }
        } else {
            CliCommand::InstallUserFiles { path: path.clone() }
        };
        assert_eq!(
            parse_arguments(vec![command.into(), "--path".into(), path]),
            Ok(expected)
        );
        for tail in [
            vec![],
            vec!["--path"],
            vec!["--path", ""],
            vec!["--dll", "x"],
            vec!["--path", "x", "extra"],
            vec!["--path", "x", "--path", "y"],
            vec!["--help"],
        ] {
            let mut args = vec![OsString::from(command)];
            args.extend(tail.into_iter().map(OsString::from));
            assert!(parse_arguments(args).is_err());
        }
    }
}

#[test]
fn prepare_install_path_requires_exact_os_path_arguments() {
    let args = |v: &[&str]| v.iter().map(OsString::from).collect();
    assert_eq!(
        parse_arguments(args(&["prepare-install-path", "--path", r"D:\软件\Flower"])),
        Ok(CliCommand::PrepareInstallPath {
            path: OsString::from(r"D:\软件\Flower"),
        })
    );
    for tail in [
        vec![],
        vec!["--path"],
        vec!["--path", ""],
        vec!["--dll", "x"],
        vec!["--path", "x", "--path", "y"],
        vec!["--path", "x", "extra"],
        vec!["--help"],
    ] {
        let mut values = vec!["prepare-install-path"];
        values.extend(tail);
        assert!(parse_arguments(args(&values)).is_err());
    }
}

#[cfg(windows)]
#[test]
fn prepare_install_path_parser_preserves_non_unicode_os_path() {
    let path = OsString::from_wide(&[b'D' as u16, 58, 92, 0xd800]);
    assert_eq!(
        parse_arguments(vec![
            "prepare-install-path".into(),
            "--path".into(),
            path.clone(),
        ]),
        Ok(CliCommand::PrepareInstallPath { path })
    );
}

#[test]
fn install_path_command_requires_exactly_one_nonempty_path() {
    let args = |v: &[&str]| v.iter().map(OsString::from).collect();
    assert_eq!(
        parse_arguments(args(&[
            "check-install-path",
            "--path",
            r"D:\Software\Flower"
        ])),
        Ok(CliCommand::CheckInstallPath {
            path: OsString::from(r"D:\Software\Flower")
        })
    );
    for v in [
        vec!["check-install-path"],
        vec!["check-install-path", "--path"],
        vec!["check-install-path", "--path", ""],
        vec!["check-install-path", "--dll", "x"],
        vec!["check-install-path", "--path", "x", "--path", "y"],
    ] {
        assert!(parse_arguments(args(&v)).is_err());
    }
}

#[derive(Debug, Clone)]
struct FakeRegistrationBackend {
    snapshot: InstallationSnapshot,
    writes: Vec<RegistrationResource>,
    removals: Vec<RegistrationResource>,
    fail_install: Option<RegistrationResource>,
    fail_remove: Option<RegistrationResource>,
    write_failed_install: bool,
    conflict_failed_install: bool,
    fail_inspection_after_install_error: bool,
    fail_inspection: bool,
}

impl FakeRegistrationBackend {
    fn new(snapshot: InstallationSnapshot) -> Self {
        Self {
            snapshot,
            writes: Vec::new(),
            removals: Vec::new(),
            fail_install: None,
            fail_remove: None,
            write_failed_install: false,
            conflict_failed_install: false,
            fail_inspection_after_install_error: false,
            fail_inspection: false,
        }
    }

    fn state_mut(&mut self, resource: RegistrationResource) -> &mut ResourceState {
        match resource {
            RegistrationResource::Com => &mut self.snapshot.com,
            RegistrationResource::LanguageProfile => &mut self.snapshot.language_profile,
            RegistrationResource::KeyboardCategory => &mut self.snapshot.keyboard_category,
            RegistrationResource::UiElementCategory => &mut self.snapshot.ui_element_category,
            RegistrationResource::ImmersiveCategory => &mut self.snapshot.immersive_category,
        }
    }

    fn clear_faults(&mut self) {
        self.fail_install = None;
        self.fail_remove = None;
        self.write_failed_install = false;
        self.conflict_failed_install = false;
        self.fail_inspection_after_install_error = false;
        self.fail_inspection = false;
    }
}

impl RegistrationBackend for FakeRegistrationBackend {
    fn inspect_registration(
        &mut self,
        _expected_dll: Option<&Path>,
    ) -> Result<InstallationSnapshot, String> {
        if self.fail_inspection {
            Err("simulated inspection failure".to_owned())
        } else {
            Ok(self.snapshot)
        }
    }

    fn install_resource(
        &mut self,
        resource: RegistrationResource,
        _dll: &Path,
    ) -> Result<ResourceInstallation, String> {
        self.writes.push(resource);
        if self.fail_install == Some(resource) {
            if self.write_failed_install {
                *self.state_mut(resource) = ResourceState::PresentOwned;
            }
            if self.conflict_failed_install {
                *self.state_mut(resource) = ResourceState::Conflict;
            }
            if self.fail_inspection_after_install_error {
                self.fail_inspection = true;
            }
            return Err("simulated install failure".to_owned());
        }
        *self.state_mut(resource) = ResourceState::PresentOwned;
        Ok(ResourceInstallation::Created)
    }

    fn remove_resource(&mut self, resource: RegistrationResource) -> Result<(), String> {
        self.removals.push(resource);
        if self.fail_remove == Some(resource) {
            return Err("simulated rollback failure".to_owned());
        }
        *self.state_mut(resource) = ResourceState::Missing;
        Ok(())
    }
}

fn missing() -> InstallationSnapshot {
    InstallationSnapshot {
        com: ResourceState::Missing,
        language_profile: ResourceState::Missing,
        keyboard_category: ResourceState::Missing,
        ui_element_category: ResourceState::Missing,
        immersive_category: ResourceState::Missing,
    }
}

fn complete() -> InstallationSnapshot {
    InstallationSnapshot {
        com: ResourceState::PresentOwned,
        language_profile: ResourceState::PresentOwned,
        keyboard_category: ResourceState::PresentOwned,
        ui_element_category: ResourceState::PresentOwned,
        immersive_category: ResourceState::PresentOwned,
    }
}

#[test]
fn register_is_idempotent_and_installs_identity_components_in_order() {
    let mut backend = FakeRegistrationBackend::new(missing());
    let dll = Path::new(r"C:\Flower\flower.dll");

    assert_eq!(
        execute_registration(&mut backend, dll),
        Ok(OperationOutcome::Changed)
    );
    assert_eq!(
        backend.writes,
        vec![
            RegistrationResource::Com,
            RegistrationResource::LanguageProfile,
            RegistrationResource::KeyboardCategory,
            RegistrationResource::UiElementCategory,
            RegistrationResource::ImmersiveCategory,
        ]
    );
    assert!(backend.snapshot.is_complete());

    assert_eq!(
        execute_registration(&mut backend, dll),
        Ok(OperationOutcome::Unchanged)
    );
    assert_eq!(backend.writes.len(), 5);
    assert!(backend.removals.is_empty());
}

#[test]
fn conflicting_registration_is_rejected_before_any_write() {
    let conflict = InstallationSnapshot {
        com: ResourceState::Conflict,
        ..missing()
    };
    assert_eq!(
        plan_registration(conflict),
        Err(RegistrationConflict {
            resource: RegistrationResource::Com
        })
    );

    let mut backend = FakeRegistrationBackend::new(conflict);
    let error = execute_registration(&mut backend, Path::new(r"C:\Other\flower.dll")).unwrap_err();
    assert!(error.message.contains("未写入"));
    assert!(backend.writes.is_empty());
}

#[test]
fn failed_register_rolls_back_only_items_created_by_this_run() {
    let existing_category = InstallationSnapshot {
        keyboard_category: ResourceState::PresentOwned,
        ..missing()
    };
    let mut backend = FakeRegistrationBackend::new(existing_category);
    backend.fail_install = Some(RegistrationResource::LanguageProfile);

    let error = execute_registration(&mut backend, Path::new(r"C:\Flower\flower.dll")).unwrap_err();
    assert!(error.message.contains("TSF 语言配置安装失败"));
    assert_eq!(backend.removals, vec![RegistrationResource::Com]);
    assert_eq!(backend.snapshot.com, ResourceState::Missing);
    assert_eq!(
        backend.snapshot.keyboard_category,
        ResourceState::PresentOwned
    );
}

#[test]
fn missing_capability_can_be_added_without_disturbing_an_existing_install() {
    let dll = Path::new(r"C:\Flower\flower.dll");
    for resource in [
        RegistrationResource::UiElementCategory,
        RegistrationResource::ImmersiveCategory,
    ] {
        let mut backend = FakeRegistrationBackend::new(complete());
        *backend.state_mut(resource) = ResourceState::Conflict;
        assert_eq!(
            plan_registration(backend.snapshot).unwrap_err().resource,
            resource
        );
        assert!(execute_unregistration(&mut backend).is_err());
        assert!(backend.removals.is_empty());

        *backend.state_mut(resource) = ResourceState::Missing;
        let before = backend.snapshot;
        assert!(!before.is_complete());
        assert_eq!(plan_registration(before).unwrap().actions, vec![resource]);
        backend.fail_install = Some(resource);
        backend.write_failed_install = true;
        assert!(execute_registration(&mut backend, dll).is_err());
        assert_eq!(backend.removals, vec![resource]);
        assert_eq!(backend.snapshot, before);

        backend.clear_faults();
        assert_eq!(
            execute_registration(&mut backend, dll),
            Ok(OperationOutcome::Changed)
        );
        assert_eq!(backend.snapshot, complete());
        assert_eq!(backend.writes, vec![resource, resource]);
    }
}

#[test]
fn failed_install_that_wrote_its_resource_is_rechecked_and_cleaned() {
    let mut backend = FakeRegistrationBackend::new(missing());
    backend.fail_install = Some(RegistrationResource::KeyboardCategory);
    backend.write_failed_install = true;

    let error = execute_registration(&mut backend, Path::new(r"C:\Flower\flower.dll")).unwrap_err();
    assert!(error.message.contains("TSF 键盘分类安装失败"));
    assert_eq!(
        backend.removals,
        vec![
            RegistrationResource::KeyboardCategory,
            RegistrationResource::LanguageProfile,
            RegistrationResource::Com,
        ]
    );
    assert_eq!(backend.snapshot, missing());
}

#[test]
fn failed_install_recheck_error_keeps_dependencies_for_repair() {
    let mut backend = FakeRegistrationBackend::new(missing());
    backend.fail_install = Some(RegistrationResource::LanguageProfile);
    backend.write_failed_install = true;
    backend.fail_inspection_after_install_error = true;

    let error = execute_registration(&mut backend, Path::new(r"C:\Flower\flower.dll")).unwrap_err();
    assert!(error.message.contains("TSF 语言配置安装失败"));
    assert!(backend.removals.is_empty());
    assert_eq!(backend.snapshot.com, ResourceState::PresentOwned);
    assert_eq!(
        backend.snapshot.language_profile,
        ResourceState::PresentOwned
    );
    assert!(error.rollback_errors[0].contains("状态复核失败"));
}

#[test]
fn failed_install_with_conflicting_recheck_does_not_delete_dependencies() {
    let mut backend = FakeRegistrationBackend::new(missing());
    backend.fail_install = Some(RegistrationResource::LanguageProfile);
    backend.conflict_failed_install = true;

    let error = execute_registration(&mut backend, Path::new(r"C:\Flower\flower.dll")).unwrap_err();
    assert!(backend.removals.is_empty());
    assert_eq!(backend.snapshot.com, ResourceState::PresentOwned);
    assert!(error.rollback_errors[0].contains("无法确定为本次新建项"));
}

#[test]
fn tsf_cleanup_failure_preserves_com_and_can_be_retried() {
    let mut backend = FakeRegistrationBackend::new(missing());
    backend.fail_install = Some(RegistrationResource::KeyboardCategory);
    backend.write_failed_install = true;
    backend.fail_remove = Some(RegistrationResource::KeyboardCategory);

    let error = execute_registration(&mut backend, Path::new(r"C:\Flower\flower.dll")).unwrap_err();
    assert_eq!(
        backend.removals,
        vec![RegistrationResource::KeyboardCategory]
    );
    assert_eq!(backend.snapshot.com, ResourceState::PresentOwned);
    assert_eq!(
        backend.snapshot.language_profile,
        ResourceState::PresentOwned
    );
    assert!(error
        .rollback_errors
        .iter()
        .any(|error| { error.contains("保留依赖项") && error.contains("COM 类注册") }));

    backend.clear_faults();
    assert_eq!(
        execute_unregistration(&mut backend),
        Ok(OperationOutcome::Changed)
    );
    assert!(!backend.snapshot.is_complete());
}

#[test]
fn rollback_failures_are_reported_without_hiding_the_original_error() {
    let mut backend = FakeRegistrationBackend::new(missing());
    backend.fail_install = Some(RegistrationResource::LanguageProfile);
    backend.fail_remove = Some(RegistrationResource::Com);

    let error = execute_registration(&mut backend, Path::new(r"C:\Flower\flower.dll")).unwrap_err();
    assert!(error.message.contains("安装失败"));
    assert_eq!(error.rollback_errors.len(), 1);
    assert!(error.rollback_errors[0].contains("COM 类注册"));
}

#[test]
fn unregister_removes_tsf_before_com_and_repeats_safely() {
    let mut backend = FakeRegistrationBackend::new(complete());
    assert_eq!(
        execute_unregistration(&mut backend),
        Ok(OperationOutcome::Changed)
    );
    assert_eq!(
        backend.removals,
        vec![
            RegistrationResource::ImmersiveCategory,
            RegistrationResource::UiElementCategory,
            RegistrationResource::KeyboardCategory,
            RegistrationResource::LanguageProfile,
            RegistrationResource::Com,
        ]
    );

    assert_eq!(
        execute_unregistration(&mut backend),
        Ok(OperationOutcome::Unchanged)
    );
    assert_eq!(backend.removals.len(), 5);
}

#[test]
fn unregister_failure_preserves_com_for_retry() {
    let mut backend = FakeRegistrationBackend::new(complete());
    backend.fail_remove = Some(RegistrationResource::LanguageProfile);

    let error = execute_unregistration(&mut backend).unwrap_err();
    assert!(error.message.contains("TSF 语言配置移除失败"));
    assert_eq!(
        backend.removals,
        vec![
            RegistrationResource::ImmersiveCategory,
            RegistrationResource::UiElementCategory,
            RegistrationResource::KeyboardCategory,
            RegistrationResource::LanguageProfile,
        ]
    );
    assert_eq!(backend.snapshot.com, ResourceState::PresentOwned);
}

#[test]
fn conflicting_unregister_leaves_existing_registration_untouched() {
    let conflict = InstallationSnapshot {
        language_profile: ResourceState::Conflict,
        ..complete()
    };
    let mut backend = FakeRegistrationBackend::new(conflict);
    assert!(execute_unregistration(&mut backend).is_err());
    assert!(backend.removals.is_empty());
}

#[derive(Default)]
struct FakeComDeletionBackend {
    events: Vec<&'static str>,
    fail_inproc_deletion: bool,
    fail_class_deletion: bool,
    fail_restore: bool,
    root_exists: bool,
    inproc_exists: bool,
    dll_path: Option<PathBuf>,
    threading_model: Option<String>,
}

impl FakeComDeletionBackend {
    fn installed(dll_path: &Path) -> Self {
        Self {
            root_exists: true,
            inproc_exists: true,
            dll_path: Some(dll_path.to_path_buf()),
            threading_model: Some("Apartment".to_owned()),
            ..Self::default()
        }
    }
}

impl ComDeletionBackend for FakeComDeletionBackend {
    fn delete_inproc_server(&mut self) -> Result<(), String> {
        self.events.push("delete_inproc");
        if self.fail_inproc_deletion {
            Err("simulated inproc deletion failure".to_owned())
        } else {
            self.inproc_exists = false;
            self.dll_path = None;
            self.threading_model = None;
            Ok(())
        }
    }

    fn restore_inproc_server(&mut self, dll: &Path) -> Result<(), String> {
        self.events.push("restore_inproc");
        if self.fail_restore {
            Err("simulated restore failure".to_owned())
        } else {
            self.inproc_exists = true;
            self.dll_path = Some(dll.to_path_buf());
            self.threading_model = Some("Apartment".to_owned());
            Ok(())
        }
    }

    fn delete_class_key(&mut self) -> Result<(), String> {
        self.events.push("delete_class");
        if self.fail_class_deletion {
            Err("simulated class deletion failure".to_owned())
        } else {
            self.root_exists = false;
            self.inproc_exists = false;
            Ok(())
        }
    }
}

#[test]
fn com_deletion_uses_whole_key_operations() {
    let dll = Path::new(r"C:\Flower\flower.dll");
    let mut backend = FakeComDeletionBackend::installed(dll);
    assert_eq!(execute_com_deletion(&mut backend, dll), Ok(()));
    assert_eq!(backend.events, vec!["delete_inproc", "delete_class"]);
    assert!(!backend.root_exists);
    assert!(!backend.inproc_exists);
}

#[test]
fn failed_inproc_deletion_stops_before_root_changes() {
    let dll = Path::new(r"C:\Flower\flower.dll");
    let mut backend = FakeComDeletionBackend::installed(dll);
    backend.fail_inproc_deletion = true;
    assert!(execute_com_deletion(&mut backend, dll).is_err());
    assert_eq!(backend.events, vec!["delete_inproc"]);
    assert!(backend.root_exists);
    assert!(backend.inproc_exists);
    assert_eq!(backend.dll_path.as_deref(), Some(dll));
    assert_eq!(backend.threading_model.as_deref(), Some("Apartment"));
}

#[test]
fn failed_root_deletion_restores_known_inproc_state_for_retry() {
    let dll = Path::new(r"C:\Flower\flower.dll");
    let mut backend = FakeComDeletionBackend::installed(dll);
    backend.fail_class_deletion = true;
    let error = execute_com_deletion(&mut backend, dll).unwrap_err();
    assert_eq!(
        backend.events,
        vec!["delete_inproc", "delete_class", "restore_inproc"]
    );
    assert!(error.message.contains("simulated class deletion failure"));
    assert!(error.rollback_errors.is_empty());
    assert!(backend.root_exists);
    assert!(backend.inproc_exists);
    assert_eq!(backend.dll_path.as_deref(), Some(dll));
    assert_eq!(backend.threading_model.as_deref(), Some("Apartment"));

    backend.fail_class_deletion = false;
    assert_eq!(execute_com_deletion(&mut backend, dll), Ok(()));
    assert!(!backend.root_exists);
    assert!(!backend.inproc_exists);
    assert!(backend.dll_path.is_none());
    assert!(backend.threading_model.is_none());
}

#[test]
fn failed_restore_is_reported_without_claiming_recovery() {
    let dll = Path::new(r"C:\Flower\flower.dll");
    let mut backend = FakeComDeletionBackend::installed(dll);
    backend.fail_class_deletion = true;
    backend.fail_restore = true;
    let error = execute_com_deletion(&mut backend, dll).unwrap_err();
    assert!(error.message.contains("simulated class deletion failure"));
    assert!(error
        .rollback_errors
        .iter()
        .any(|error| error.contains("simulated restore failure")));
    assert!(backend.root_exists);
    assert!(!backend.inproc_exists);
    assert!(backend.dll_path.is_none());
    assert!(backend.threading_model.is_none());
}

#[derive(Default)]
struct FakeLayoutBackend {
    machine_complete: bool,
    operations: Vec<UserLayoutOperation>,
}

impl UserLayoutBackend for FakeLayoutBackend {
    fn machine_installation_complete(&mut self) -> Result<bool, String> {
        Ok(self.machine_complete)
    }

    fn apply_user_layout(&mut self, operation: UserLayoutOperation) -> Result<(), String> {
        self.operations.push(operation);
        Ok(())
    }
}

#[test]
fn enable_requires_complete_machine_registration_but_disable_does_not() {
    let mut backend = FakeLayoutBackend::default();
    assert!(execute_user_layout(&mut backend, UserLayoutOperation::Enable).is_err());
    assert!(backend.operations.is_empty());

    backend.machine_complete = true;
    assert_eq!(
        execute_user_layout(&mut backend, UserLayoutOperation::Enable),
        Ok(OperationOutcome::Changed)
    );
    assert_eq!(backend.operations, vec![UserLayoutOperation::Enable]);

    backend.machine_complete = false;
    assert_eq!(
        execute_user_layout(&mut backend, UserLayoutOperation::Disable),
        Ok(OperationOutcome::Changed)
    );
    assert_eq!(
        backend.operations,
        vec![UserLayoutOperation::Enable, UserLayoutOperation::Disable]
    );
}

#[test]
fn install_layout_or_tip_uses_only_fixed_flags_and_target_profile() {
    assert_eq!(UserLayoutOperation::Enable.install_layout_or_tip_flags(), 0);
    assert_eq!(
        UserLayoutOperation::Disable.install_layout_or_tip_flags(),
        ILOT_UNINSTALL
    );
    assert_eq!(ILOT_UNINSTALL, 1);
    assert_eq!(FLOWER_LANGID_ZH_CN, 0x0804);
    assert_eq!(
        FLOWER_INSTALL_LAYOUT_OR_TIP_PROFILE,
        "0x0804:{3D5B7CB1-96CB-4F44-A792-64304F6E7426}{6C7782F4-5F59-42CF-8C29-F9A8D46F1B72}"
    );
    assert_eq!(
        FLOWER_TEXT_SERVICE_CLSID_U128,
        0x3d5b7cb1_96cb_4f44_a792_64304f6e7426
    );
    assert_eq!(FLOWER_LANGUAGE_PROFILE_DESCRIPTION, "Flower");
    assert_eq!(flower::registration::FLOWER_ICON_RESOURCE_ID, 101);
    assert_eq!(flower::registration::FLOWER_PROFILE_ICON_INDEX, 0);
}

#[test]
fn parser_preserves_non_unicode_dll_paths_exactly() {
    let path = non_unicode_path();
    assert_eq!(
        parse_arguments(vec![
            OsString::from("register"),
            OsString::from("--dll"),
            path.clone(),
        ]),
        Ok(CliCommand::Register { dll: Some(path) })
    );
}

#[cfg(unix)]
fn non_unicode_path() -> OsString {
    OsString::from_vec(b"C:\\Flower\\\xff.dll".to_vec())
}

#[cfg(windows)]
fn non_unicode_path() -> OsString {
    OsString::from_wide(&[
        b'C' as u16,
        b':' as u16,
        b'\\' as u16,
        b'F' as u16,
        0xd800,
        b'.' as u16,
        b'd' as u16,
        b'l' as u16,
        b'l' as u16,
    ])
}

#[test]
fn parser_rejects_unknown_duplicate_and_extra_arguments() {
    assert!(parse_arguments(vec![]).is_err());
    assert!(parse_arguments(vec![OsString::from("unknown")]).is_err());
    assert!(parse_arguments(vec![OsString::from("unregister"), OsString::from("extra")]).is_err());
    assert!(parse_arguments(vec![
        OsString::from("register"),
        OsString::from("--dll"),
        OsString::from(r"C:\a.dll"),
        OsString::from("--dll"),
        OsString::from(r"C:\b.dll"),
    ])
    .is_err());
    assert_eq!(
        parse_arguments(vec![OsString::from("--help")]),
        Ok(CliCommand::Help)
    );
    assert_eq!(
        parse_arguments(vec![OsString::from("register"), OsString::from("--help")]),
        Ok(CliCommand::Help)
    );
}

#[test]
fn dll_path_rules_use_the_executable_directory_and_absolute_utf16() {
    #[cfg(windows)]
    let (executable, expected_default, relative, base, absolute) = (
        Path::new(r"C:\Flower\flower-register.exe"),
        PathBuf::from(r"C:\Flower\flower.dll"),
        Path::new(r"bin\flower.dll"),
        Path::new(r"C:\Flower"),
        PathBuf::from(r"C:\Flower\bin\flower.dll"),
    );
    #[cfg(not(windows))]
    let (executable, expected_default, relative, base, absolute) = (
        Path::new("/opt/flower/flower-register"),
        PathBuf::from("/opt/flower/flower.dll"),
        Path::new("bin/flower.dll"),
        Path::new("/opt/flower"),
        PathBuf::from("/opt/flower/bin/flower.dll"),
    );
    assert_eq!(default_dll_path(executable).unwrap(), expected_default);
    assert_eq!(make_absolute_path(relative, base), absolute);
    assert!(validate_wide_dll_path(&wide(r"C:\Flower\flower.dll")).is_ok());
    assert!(validate_wide_dll_path(&wide(r"relative\flower.dll")).is_err());
    assert!(validate_wide_dll_path(&[b'C' as u16, b':' as u16, b'\\' as u16, 0]).is_err());
}

#[test]
fn pe_validation_accepts_amd64_pe32_plus_dll_and_rejects_malformed_images() {
    let valid = minimal_pe_dll();
    assert_eq!(validate_pe_dll(&valid), Ok(()));

    let mut wrong_machine = valid.clone();
    wrong_machine[0x84..0x86].copy_from_slice(&0x014cu16.to_le_bytes());
    assert!(validate_pe_dll(&wrong_machine).is_err());

    let mut wrong_magic = valid.clone();
    wrong_magic[0x98..0x9a].copy_from_slice(&0x010bu16.to_le_bytes());
    assert!(validate_pe_dll(&wrong_magic).is_err());

    let mut not_dll = valid.clone();
    not_dll[0x86..0x88].copy_from_slice(&0u16.to_le_bytes());
    assert!(validate_pe_dll(&not_dll).is_err());

    let mut truncated_section_table = valid.clone();
    truncated_section_table.truncate(0x1b0);
    assert!(validate_pe_dll(&truncated_section_table).is_err());

    let mut section_overrun = valid;
    section_overrun[0x188 + 16..0x188 + 20].copy_from_slice(&0x1000u32.to_le_bytes());
    assert!(validate_pe_dll(&section_overrun).is_err());
    assert!(validate_pe_dll(&[]).is_err());
}

fn minimal_pe_dll() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x400];
    bytes[0..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");

    let coff = 0x84;
    bytes[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
    bytes[coff + 16..coff + 18].copy_from_slice(&0x00f0u16.to_le_bytes());
    bytes[coff + 18..coff + 20].copy_from_slice(&0x2002u16.to_le_bytes());

    let optional = 0x98;
    bytes[optional..optional + 2].copy_from_slice(&0x020bu16.to_le_bytes());
    bytes[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());

    let section = optional + 0x00f0;
    bytes[section + 16..section + 20].copy_from_slice(&0x0200u32.to_le_bytes());
    bytes[section + 20..section + 24].copy_from_slice(&0x0200u32.to_le_bytes());
    bytes
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}
