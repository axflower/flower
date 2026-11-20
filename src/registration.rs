//! Flower TSF registration identity, command-line parsing, and registration
//! planning shared by Windows and host-platform tests.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Existing Flower TSF class identifier. This value is part of the installed
/// identity and must not change.
pub const FLOWER_TEXT_SERVICE_CLSID_U128: u128 = 0x3d5b7cb1_96cb_4f44_a792_64304f6e7426;
pub const FLOWER_TEXT_SERVICE_CLSID: &str = "{3D5B7CB1-96CB-4F44-A792-64304F6E7426}";
pub const FLOWER_TEXT_SERVICE_DESCRIPTION: &str = "Flower";
/// Group-icon resource embedded in the Windows DLL and executable files.
pub const FLOWER_ICON_RESOURCE_ID: u16 = 101;
/// Index of the first group icon in the registered DLL.
pub const FLOWER_PROFILE_ICON_INDEX: u32 = 0;

/// Fixed zh-CN language profile introduced with the machine registration tool.
pub const FLOWER_LANGUAGE_PROFILE_GUID_U128: u128 = 0x6c7782f4_5f59_42cf_8c29_f9a8d46f1b72;
pub const FLOWER_LANGUAGE_PROFILE_GUID: &str = "{6C7782F4-5F59-42CF-8C29-F9A8D46F1B72}";
pub const FLOWER_LANGUAGE_PROFILE_DESCRIPTION: &str = FLOWER_TEXT_SERVICE_DESCRIPTION;
pub const FLOWER_LANGID_ZH_CN: u16 = 0x0804;

/// The exact current-user profile expression passed to `InstallLayoutOrTip`.
pub const FLOWER_INSTALL_LAYOUT_OR_TIP_PROFILE: &str =
    "0x0804:{3D5B7CB1-96CB-4F44-A792-64304F6E7426}{6C7782F4-5F59-42CF-8C29-F9A8D46F1B72}";

pub const DEFAULT_DLL_FILE_NAME: &str = "flower.dll";
pub const ILOT_UNINSTALL: u32 = 0x0000_0001;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    CheckUserInstallPath { path: OsString },
    InstallUserFiles { path: OsString },
    CheckInstallPath { path: OsString },
    PrepareInstallPath { path: OsString },
    Register { dll: Option<OsString> },
    Unregister,
    Enable,
    Disable,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliError {
    message: &'static str,
}

impl CliError {
    pub fn message(self) -> &'static str {
        self.message
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for CliError {}

/// Parses command-line arguments without converting paths through UTF-8.
pub fn parse_arguments(arguments: Vec<OsString>) -> Result<CliCommand, CliError> {
    if arguments.len() == 1 && is_help_argument(&arguments[0]) {
        return Ok(CliCommand::Help);
    }

    let Some(command) = arguments.first() else {
        return Err(CliError {
            message: "缺少命令。请使用 --help 查看用法。",
        });
    };

    if command == OsStr::new("register") {
        return parse_register_arguments(&arguments[1..]);
    }
    if command == OsStr::new("check-install-path")
        || command == OsStr::new("prepare-install-path")
        || command == OsStr::new("check-user-install-path")
        || command == OsStr::new("install-user-files")
    {
        return if arguments.len() == 3
            && arguments[1] == OsStr::new("--path")
            && !arguments[2].is_empty()
        {
            let path = arguments[2].clone();
            Ok(if command == OsStr::new("check-user-install-path") {
                CliCommand::CheckUserInstallPath { path }
            } else if command == OsStr::new("install-user-files") {
                CliCommand::InstallUserFiles { path }
            } else if command == OsStr::new("prepare-install-path") {
                CliCommand::PrepareInstallPath { path }
            } else {
                CliCommand::CheckInstallPath { path }
            })
        } else {
            Err(CliError {
                message: "目录命令只接受必填的 --path <目录>。",
            })
        };
    }
    if command == OsStr::new("unregister") {
        return no_argument_command(&arguments[1..], CliCommand::Unregister);
    }
    if command == OsStr::new("enable") {
        return no_argument_command(&arguments[1..], CliCommand::Enable);
    }
    if command == OsStr::new("disable") {
        return no_argument_command(&arguments[1..], CliCommand::Disable);
    }

    Err(CliError {
        message: "未知命令。请使用 --help 查看可用命令。",
    })
}

fn is_help_argument(argument: &OsStr) -> bool {
    argument == OsStr::new("--help") || argument == OsStr::new("-h")
}

fn no_argument_command(
    arguments: &[OsString],
    command: CliCommand,
) -> Result<CliCommand, CliError> {
    if arguments.is_empty() {
        Ok(command)
    } else {
        Err(CliError {
            message: "该命令不接受额外参数。",
        })
    }
}

fn parse_register_arguments(arguments: &[OsString]) -> Result<CliCommand, CliError> {
    if arguments.len() == 1 && is_help_argument(&arguments[0]) {
        return Ok(CliCommand::Help);
    }

    let mut dll = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == OsStr::new("--dll") {
            if dll.is_some() || index + 1 >= arguments.len() {
                return Err(CliError {
                    message: "--dll 必须且只能指定一次，并提供 DLL 路径。",
                });
            }
            let path = arguments[index + 1].clone();
            if path.is_empty() {
                return Err(CliError {
                    message: "--dll 路径不能为空。",
                });
            }
            dll = Some(path);
            index += 2;
            continue;
        }

        return Err(CliError {
            message: "register 参数无效；只接受可选的 --dll <path>。",
        });
    }

    Ok(CliCommand::Register { dll })
}

/// Returns `flower.dll` beside the registration executable, independent of the
/// process working directory.
pub fn default_dll_path(executable: &Path) -> Result<PathBuf, DllValidationError> {
    let parent = executable
        .parent()
        .ok_or(DllValidationError::MissingExecutableDirectory)?;
    let path = parent.join(DEFAULT_DLL_FILE_NAME);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(DllValidationError::RelativePath)
    }
}

/// Makes a path absolute and removes `.` and ordinary `..` components without
/// losing non-Unicode path data.
pub fn make_absolute_path(path: &Path, current_directory: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_directory.join(path)
    };

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DllValidationError {
    MissingExecutableDirectory,
    RelativePath,
    NulInPath,
    EmptyPath,
    NotAbsolute,
    MissingFile,
    NotAFile,
    FileTooLarge,
    Io(String),
    InvalidPe(&'static str),
}

impl fmt::Display for DllValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingExecutableDirectory => formatter.write_str("无法确定注册工具所在目录。"),
            Self::RelativePath => formatter.write_str("DLL 路径必须为绝对路径。"),
            Self::NulInPath => formatter.write_str("DLL 路径包含 Windows 不允许的 NUL 字符。"),
            Self::EmptyPath => formatter.write_str("DLL 路径不能为空。"),
            Self::NotAbsolute => formatter.write_str("DLL 路径不是 Windows 绝对路径。"),
            Self::MissingFile => formatter.write_str("指定的 DLL 文件不存在。"),
            Self::NotAFile => formatter.write_str("指定的路径不是普通文件。"),
            Self::FileTooLarge => formatter.write_str("指定的 DLL 文件过大，拒绝读取。"),
            Self::Io(message) => write!(formatter, "无法读取 DLL 文件：{message}"),
            Self::InvalidPe(reason) => write!(formatter, "DLL 文件无效：{reason}"),
        }
    }
}

impl std::error::Error for DllValidationError {}

/// Validates the UTF-16 path representation used by Win32 registration APIs.
pub fn validate_wide_dll_path(path: &[u16]) -> Result<(), DllValidationError> {
    if path.is_empty() {
        return Err(DllValidationError::EmptyPath);
    }
    if path.contains(&0) {
        return Err(DllValidationError::NulInPath);
    }
    if !wide_path_is_absolute(path) {
        return Err(DllValidationError::NotAbsolute);
    }
    Ok(())
}

fn wide_path_is_absolute(path: &[u16]) -> bool {
    let drive_absolute = path.len() >= 3
        && u8::try_from(path[0])
            .map(|character| character.is_ascii_alphabetic())
            .unwrap_or(false)
        && path[1] == u16::from(b':')
        && is_wide_path_separator(path[2]);
    let unc_absolute =
        path.len() >= 2 && is_wide_path_separator(path[0]) && is_wide_path_separator(path[1]);
    drive_absolute || unc_absolute
}

fn is_wide_path_separator(character: u16) -> bool {
    character == u16::from(b'\\') || character == u16::from(b'/')
}

/// Performs strict bounds checks for an AMD64 PE32+ DLL image.
pub fn validate_pe_dll(bytes: &[u8]) -> Result<(), DllValidationError> {
    if !matches!(read_u16(bytes, 0), Ok(Some(0x5a4d))) {
        return Err(DllValidationError::InvalidPe("缺少 MZ 文件头"));
    }

    let pe_offset = read_u32(bytes, 0x3c)
        .map_err(DllValidationError::InvalidPe)?
        .ok_or(DllValidationError::InvalidPe("缺少 PE 头偏移"))? as usize;
    if pe_offset < 0x40 {
        return Err(DllValidationError::InvalidPe("PE 头偏移位于 DOS 头内部"));
    }
    let signature_end = pe_offset
        .checked_add(4)
        .ok_or(DllValidationError::InvalidPe("PE 头偏移溢出"))?;
    if signature_end > bytes.len() || &bytes[pe_offset..signature_end] != b"PE\0\0" {
        return Err(DllValidationError::InvalidPe("缺少 PE 签名"));
    }

    let coff = signature_end;
    let coff_end = coff
        .checked_add(20)
        .ok_or(DllValidationError::InvalidPe("COFF 头偏移溢出"))?;
    if coff_end > bytes.len() {
        return Err(DllValidationError::InvalidPe("COFF 头被截断"));
    }
    if read_u16(bytes, coff).map_err(DllValidationError::InvalidPe)? != Some(0x8664) {
        return Err(DllValidationError::InvalidPe("目标架构不是 x64"));
    }

    let section_count = read_u16(bytes, coff + 2)
        .map_err(DllValidationError::InvalidPe)?
        .ok_or(DllValidationError::InvalidPe("缺少节数量"))? as usize;
    if section_count == 0 || section_count > 96 {
        return Err(DllValidationError::InvalidPe("节数量无效"));
    }
    let optional_size = read_u16(bytes, coff + 16)
        .map_err(DllValidationError::InvalidPe)?
        .ok_or(DllValidationError::InvalidPe("缺少可选头长度"))? as usize;
    let characteristics = read_u16(bytes, coff + 18)
        .map_err(DllValidationError::InvalidPe)?
        .ok_or(DllValidationError::InvalidPe("缺少文件特性"))?;
    if characteristics & 0x2000 == 0 {
        return Err(DllValidationError::InvalidPe("映像不是 DLL"));
    }
    if optional_size < 112 {
        return Err(DllValidationError::InvalidPe("PE32+ 可选头过短"));
    }

    let optional = coff_end;
    let optional_end = optional
        .checked_add(optional_size)
        .ok_or(DllValidationError::InvalidPe("可选头偏移溢出"))?;
    if optional_end > bytes.len() {
        return Err(DllValidationError::InvalidPe("可选头被截断"));
    }
    if read_u16(bytes, optional).map_err(DllValidationError::InvalidPe)? != Some(0x20b) {
        return Err(DllValidationError::InvalidPe("可选头不是 PE32+"));
    }

    let data_directory_count = read_u32(bytes, optional + 108)
        .map_err(DllValidationError::InvalidPe)?
        .ok_or(DllValidationError::InvalidPe("缺少数据目录数量"))?
        as usize;
    let directory_bytes = data_directory_count
        .checked_mul(8)
        .ok_or(DllValidationError::InvalidPe("数据目录长度溢出"))?;
    let required_optional_size = 112usize
        .checked_add(directory_bytes)
        .ok_or(DllValidationError::InvalidPe("数据目录长度溢出"))?;
    if required_optional_size > optional_size {
        return Err(DllValidationError::InvalidPe("数据目录超出可选头边界"));
    }

    let section_table_size = section_count
        .checked_mul(40)
        .ok_or(DllValidationError::InvalidPe("节表长度溢出"))?;
    let section_table_end = optional_end
        .checked_add(section_table_size)
        .ok_or(DllValidationError::InvalidPe("节表偏移溢出"))?;
    if section_table_end > bytes.len() {
        return Err(DllValidationError::InvalidPe("节表被截断"));
    }

    for index in 0..section_count {
        let section = optional_end + index * 40;
        let raw_size = read_u32(bytes, section + 16)
            .map_err(DllValidationError::InvalidPe)?
            .ok_or(DllValidationError::InvalidPe("节缺少原始长度"))?
            as usize;
        let raw_offset = read_u32(bytes, section + 20)
            .map_err(DllValidationError::InvalidPe)?
            .ok_or(DllValidationError::InvalidPe("节缺少原始偏移"))?
            as usize;
        if raw_size == 0 {
            continue;
        }
        let raw_end = raw_offset
            .checked_add(raw_size)
            .ok_or(DllValidationError::InvalidPe("节原始范围溢出"))?;
        if raw_offset < section_table_end || raw_end > bytes.len() {
            return Err(DllValidationError::InvalidPe("节原始数据超出文件边界"));
        }
    }

    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<Option<u16>, &'static str> {
    let end = offset.checked_add(2).ok_or("u16 偏移溢出")?;
    let Some(slice) = bytes.get(offset..end) else {
        return Ok(None);
    };
    Ok(Some(u16::from_le_bytes([slice[0], slice[1]])))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<Option<u32>, &'static str> {
    let end = offset.checked_add(4).ok_or("u32 偏移溢出")?;
    let Some(slice) = bytes.get(offset..end) else {
        return Ok(None);
    };
    Ok(Some(u32::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3],
    ])))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceState {
    Missing,
    PresentOwned,
    Conflict,
}

impl ResourceState {
    pub fn is_present(self) -> bool {
        self == Self::PresentOwned
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallationSnapshot {
    pub com: ResourceState,
    pub language_profile: ResourceState,
    pub keyboard_category: ResourceState,
    pub ui_element_category: ResourceState,
    pub immersive_category: ResourceState,
}

impl InstallationSnapshot {
    pub fn is_complete(self) -> bool {
        RegistrationResource::INSTALL_ORDER
            .into_iter()
            .all(|resource| resource.state(self).is_present())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationResource {
    Com,
    LanguageProfile,
    KeyboardCategory,
    UiElementCategory,
    ImmersiveCategory,
}

impl RegistrationResource {
    const INSTALL_ORDER: [Self; 5] = [
        Self::Com,
        Self::LanguageProfile,
        Self::KeyboardCategory,
        Self::UiElementCategory,
        Self::ImmersiveCategory,
    ];

    fn state(self, snapshot: InstallationSnapshot) -> ResourceState {
        match self {
            Self::Com => snapshot.com,
            Self::LanguageProfile => snapshot.language_profile,
            Self::KeyboardCategory => snapshot.keyboard_category,
            Self::UiElementCategory => snapshot.ui_element_category,
            Self::ImmersiveCategory => snapshot.immersive_category,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Com => "COM 类注册",
            Self::LanguageProfile => "TSF 语言配置",
            Self::KeyboardCategory => "TSF 键盘分类",
            Self::UiElementCategory => "TSF UIElement 能力分类",
            Self::ImmersiveCategory => "TSF 现代界面兼容分类",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationPlan {
    pub actions: Vec<RegistrationResource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistrationConflict {
    pub resource: RegistrationResource,
}

impl fmt::Display for RegistrationConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "现有{}与 Flower 固定身份不一致；未写入任何内容。",
            self.resource.label()
        )
    }
}

impl std::error::Error for RegistrationConflict {}

pub fn plan_registration(
    snapshot: InstallationSnapshot,
) -> Result<RegistrationPlan, RegistrationConflict> {
    let all = RegistrationResource::INSTALL_ORDER;
    if let Some(resource) = all
        .into_iter()
        .find(|resource| resource.state(snapshot) == ResourceState::Conflict)
    {
        return Err(RegistrationConflict { resource });
    }

    Ok(RegistrationPlan {
        actions: all
            .into_iter()
            .filter(|resource| resource.state(snapshot) == ResourceState::Missing)
            .collect(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationOutcome {
    Changed,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationError {
    pub message: String,
    pub rollback_errors: Vec<String>,
}

impl OperationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            rollback_errors: Vec::new(),
        }
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;
        if !self.rollback_errors.is_empty() {
            write!(
                formatter,
                "\n回滚也失败：{}",
                self.rollback_errors.join("；")
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for OperationError {}

pub trait RegistrationBackend {
    fn inspect_registration(
        &mut self,
        expected_dll: Option<&Path>,
    ) -> Result<InstallationSnapshot, String>;
    fn install_resource(
        &mut self,
        resource: RegistrationResource,
        dll: &Path,
    ) -> Result<ResourceInstallation, String>;
    fn remove_resource(&mut self, resource: RegistrationResource) -> Result<(), String>;
}

pub trait ComDeletionBackend {
    fn delete_inproc_server(&mut self) -> Result<(), String>;
    fn restore_inproc_server(&mut self, dll: &Path) -> Result<(), String>;
    fn delete_class_key(&mut self) -> Result<(), String>;
}

pub fn execute_com_deletion<B: ComDeletionBackend>(
    backend: &mut B,
    dll: &Path,
) -> Result<(), OperationError> {
    backend
        .delete_inproc_server()
        .map_err(|error| OperationError::new(format!("删除 InprocServer32 失败：{error}")))?;
    if let Err(delete_error) = backend.delete_class_key() {
        return Err(match backend.restore_inproc_server(dll) {
            Ok(()) => OperationError::new(format!(
                "删除 COM 类根键失败：{delete_error}；已恢复预检确认的 InprocServer32。若存在外部并发修改或未知子项，需人工检查后再重试。"
            )),
            Err(restore_error) => OperationError {
                message: format!("删除 COM 类根键失败：{delete_error}"),
                rollback_errors: vec![format!("恢复 InprocServer32 失败：{restore_error}")],
            },
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceInstallation {
    Created,
    AlreadyPresent,
}

pub fn execute_registration<B: RegistrationBackend>(
    backend: &mut B,
    dll: &Path,
) -> Result<OperationOutcome, OperationError> {
    let snapshot = backend
        .inspect_registration(Some(dll))
        .map_err(OperationError::new)?;
    let plan = plan_registration(snapshot)
        .map_err(|conflict| OperationError::new(conflict.to_string()))?;
    if plan.actions.is_empty() {
        return Ok(OperationOutcome::Unchanged);
    }

    let mut installed = Vec::new();
    for resource in &plan.actions {
        match backend.install_resource(*resource, dll) {
            Ok(ResourceInstallation::Created) => installed.push(*resource),
            Ok(ResourceInstallation::AlreadyPresent) => {}
            Err(error) => {
                return Err(rollback_failed_installation(
                    backend, dll, snapshot, installed, *resource, error,
                ));
            }
        }
    }

    Ok(if installed.is_empty() {
        OperationOutcome::Unchanged
    } else {
        OperationOutcome::Changed
    })
}

fn rollback_failed_installation<B: RegistrationBackend>(
    backend: &mut B,
    dll: &Path,
    original: InstallationSnapshot,
    installed: Vec<RegistrationResource>,
    failed: RegistrationResource,
    original_error: String,
) -> OperationError {
    let mut failure = OperationError::new(format!("{}安装失败：{original_error}", failed.label()));
    let rechecked = match backend.inspect_registration(Some(dll)) {
        Ok(snapshot) => snapshot,
        Err(check_error) => {
            failure.rollback_errors.push(format!(
                "{}失败后的状态复核失败，未继续清理并保留可能依赖的 COM：{check_error}",
                failed.label()
            ));
            return failure;
        }
    };

    let failed_state = failed.state(rechecked);
    if failed_state == ResourceState::Conflict {
        failure.rollback_errors.push(format!(
            "{}失败后的状态无法确定为本次新建项，未继续清理并保留可能依赖的 COM。",
            failed.label()
        ));
        return failure;
    }

    let mut cleanup = installed;
    if failed.state(original).is_present() {
        failure.rollback_errors.push(format!(
            "{}在失败前已由预检确认存在，未清理。",
            failed.label()
        ));
    } else if failed_state.is_present() {
        cleanup.push(failed);
    }

    let mut cleanup_blocked = false;
    for resource in RegistrationResource::INSTALL_ORDER.into_iter().rev() {
        if !cleanup.contains(&resource) {
            continue;
        }
        if cleanup_blocked {
            failure.rollback_errors.push(format!(
                "因后续 TSF 项未能清理，保留依赖项{}以便修复重试。",
                resource.label()
            ));
            continue;
        }

        match resource.state(rechecked) {
            ResourceState::Missing => {}
            ResourceState::PresentOwned => {
                if let Err(error) = backend.remove_resource(resource) {
                    failure
                        .rollback_errors
                        .push(format!("{}清理失败：{error}", resource.label()));
                    if resource != RegistrationResource::Com {
                        cleanup_blocked = true;
                    }
                }
            }
            ResourceState::Conflict => {
                failure.rollback_errors.push(format!(
                    "{}状态不明，未删除并保留其依赖项。",
                    resource.label()
                ));
                cleanup_blocked = true;
            }
        }
    }
    failure
}

pub fn execute_unregistration<B: RegistrationBackend>(
    backend: &mut B,
) -> Result<OperationOutcome, OperationError> {
    let snapshot = backend
        .inspect_registration(None)
        .map_err(OperationError::new)?;
    let all = RegistrationResource::INSTALL_ORDER;
    if let Some(resource) = all
        .into_iter()
        .find(|resource| resource.state(snapshot) == ResourceState::Conflict)
    {
        return Err(OperationError::new(
            RegistrationConflict { resource }.to_string(),
        ));
    }

    let mut changed = false;
    for resource in all.into_iter().rev() {
        if resource.state(snapshot).is_present() {
            backend.remove_resource(resource).map_err(|error| {
                OperationError::new(format!("{}移除失败：{error}", resource.label()))
            })?;
            changed = true;
        }
    }

    Ok(if changed {
        OperationOutcome::Changed
    } else {
        OperationOutcome::Unchanged
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserLayoutOperation {
    Enable,
    Disable,
}

impl UserLayoutOperation {
    pub fn install_layout_or_tip_flags(self) -> u32 {
        match self {
            Self::Enable => 0,
            Self::Disable => ILOT_UNINSTALL,
        }
    }
}

pub trait UserLayoutBackend {
    fn machine_installation_complete(&mut self) -> Result<bool, String>;
    fn apply_user_layout(&mut self, operation: UserLayoutOperation) -> Result<(), String>;
}

pub fn execute_user_layout<B: UserLayoutBackend>(
    backend: &mut B,
    operation: UserLayoutOperation,
) -> Result<OperationOutcome, OperationError> {
    if operation == UserLayoutOperation::Enable
        && !backend
            .machine_installation_complete()
            .map_err(OperationError::new)?
    {
        return Err(OperationError::new(
            "Flower 机器级 COM/TSF 注册不完整。请先由管理员运行 register，再执行 enable。",
        ));
    }

    backend
        .apply_user_layout(operation)
        .map_err(OperationError::new)?;
    Ok(OperationOutcome::Changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_identity_is_shared_by_every_registration_surface() {
        assert_eq!(FLOWER_LANGID_ZH_CN, 0x0804);
        assert_eq!(
            FLOWER_INSTALL_LAYOUT_OR_TIP_PROFILE,
            format!(
                "0x0804:{}{}",
                FLOWER_TEXT_SERVICE_CLSID, FLOWER_LANGUAGE_PROFILE_GUID
            )
        );
        assert_eq!(
            FLOWER_TEXT_SERVICE_CLSID_U128,
            0x3d5b7cb1_96cb_4f44_a792_64304f6e7426
        );
    }

    #[test]
    fn default_dll_uses_the_executable_directory() {
        #[cfg(windows)]
        let (executable, expected) = (
            Path::new(r"C:\Flower\flower-register.exe"),
            PathBuf::from(r"C:\Flower\flower.dll"),
        );
        #[cfg(not(windows))]
        let (executable, expected) = (
            Path::new("/opt/flower/flower-register"),
            PathBuf::from("/opt/flower/flower.dll"),
        );
        assert_eq!(default_dll_path(executable).unwrap(), expected);
    }

    #[test]
    fn wide_path_validation_rejects_nul_and_relative_paths() {
        assert_eq!(
            validate_wide_dll_path(&[]),
            Err(DllValidationError::EmptyPath)
        );
        assert_eq!(
            validate_wide_dll_path(&[u16::from(b'C'), u16::from(b':')]),
            Err(DllValidationError::NotAbsolute)
        );
        assert_eq!(
            validate_wide_dll_path(&[u16::from(b'C'), u16::from(b':'), u16::from(b'\\'), 0,]),
            Err(DllValidationError::NulInPath)
        );
        assert!(validate_wide_dll_path(&[
            u16::from(b'C'),
            u16::from(b':'),
            u16::from(b'\\'),
            u16::from(b'F'),
        ])
        .is_ok());
    }
}
