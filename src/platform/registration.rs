//! Windows machine registration, current-user input profile management, and
//! DLL validation for the `flower-register` command-line tool.

use std::ffi::{c_void, OsString};
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr;
use std::slice;

use crate::registration::{
    execute_com_deletion, execute_registration, execute_unregistration, execute_user_layout,
    validate_pe_dll, validate_wide_dll_path, ComDeletionBackend, DllValidationError,
    InstallationSnapshot, OperationError, OperationOutcome, RegistrationBackend,
    RegistrationResource, ResourceInstallation, ResourceState, UserLayoutBackend,
    UserLayoutOperation, FLOWER_LANGID_ZH_CN, FLOWER_LANGUAGE_PROFILE_DESCRIPTION,
    FLOWER_LANGUAGE_PROFILE_GUID, FLOWER_LANGUAGE_PROFILE_GUID_U128, FLOWER_PROFILE_ICON_INDEX,
    FLOWER_TEXT_SERVICE_CLSID, FLOWER_TEXT_SERVICE_CLSID_U128, FLOWER_TEXT_SERVICE_DESCRIPTION,
};
use windows::core::{s, w, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, FreeLibrary, BOOL, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
    ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, FALSE, HANDLE, HMODULE, WAIT_ABANDONED, WAIT_FAILED,
    WAIT_OBJECT_0,
};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyExW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
    RegQueryInfoKeyW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    KEY_WOW64_64KEY, KEY_WRITE, REG_CREATED_NEW_KEY, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex, WaitForSingleObject, INFINITE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, ITfCategoryMgr,
    ITfInputProcessorProfileMgr, ITfInputProcessorProfiles, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED, GUID_TFCAT_TIP_KEYBOARD, TF_INPUTPROCESSORPROFILE,
    TF_PROFILETYPE_INPUTPROCESSOR,
};

const MAX_DLL_SIZE: u64 = 512 * 1024 * 1024;
const INPROC_SERVER_KEY: &str = "InprocServer32";
const THREADING_MODEL_KEY: &str = "ThreadingModel";
const APARTMENT_THREADING_MODEL: &str = "Apartment";

const FLOWER_CLSID: GUID = GUID::from_u128(FLOWER_TEXT_SERVICE_CLSID_U128);
const FLOWER_LANGUAGE_PROFILE: GUID = GUID::from_u128(FLOWER_LANGUAGE_PROFILE_GUID_U128);

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        // SAFETY: This process initializes COM once on the command-line thread
        // before any TSF object is created.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_err() {
            return Err(format!(
                "无法初始化 COM apartment（HRESULT 0x{:08X}）。",
                result.0
            ));
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: `CoUninitialize` is paired with a successful
        // `CoInitializeEx` in `initialize`.
        unsafe { CoUninitialize() };
    }
}

struct ValidatedDll {
    path: PathBuf,
    wide: Vec<u16>,
}

fn validate_dll(path: &Path) -> Result<ValidatedDll, DllValidationError> {
    if path.as_os_str().is_empty() {
        return Err(DllValidationError::EmptyPath);
    }
    if path
        .as_os_str()
        .encode_wide()
        .any(|character| character == 0)
    {
        return Err(DllValidationError::NulInPath);
    }
    let canonical = fs::canonicalize(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            DllValidationError::MissingFile
        } else {
            DllValidationError::Io(error.to_string())
        }
    })?;
    let metadata =
        fs::metadata(&canonical).map_err(|error| DllValidationError::Io(error.to_string()))?;
    if !metadata.is_file() {
        return Err(DllValidationError::NotAFile);
    }
    if metadata.len() > MAX_DLL_SIZE {
        return Err(DllValidationError::FileTooLarge);
    }

    let wide = wide_path(&canonical);
    validate_wide_dll_path(&wide)?;

    let bytes = fs::read(&canonical).map_err(|error| DllValidationError::Io(error.to_string()))?;
    validate_pe_dll(&bytes)?;
    Ok(ValidatedDll {
        path: canonical,
        wide,
    })
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}

fn require_elevated_terminal() -> Result<(), String> {
    let mut token = HANDLE::default();
    // SAFETY: The pseudo-handle from `GetCurrentProcess` is valid for the
    // duration of the call, and `token` is a writable output handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|error| format!("无法读取当前进程令牌：{error}"))?;
    let token = OwnedHandle(token);

    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    // SAFETY: `token` is live, the output points to a correctly sized
    // `TOKEN_ELEVATION`, and `returned` is writable.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    }
    .map_err(|error| format!("无法检查终端提升状态：{error}"))?;

    if elevation.TokenIsElevated == 0 {
        return Err(
            "register 和 unregister 会修改 HKLM，必须在“以管理员身份运行”的终端中执行；本工具不会自动提权。"
                .to_owned(),
        );
    }
    Ok(())
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: The handle was returned by `OpenProcessToken` and is owned by
        // this guard.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct MachineRegistrationLock(HANDLE);

impl MachineRegistrationLock {
    fn acquire() -> Result<Self, String> {
        // SAFETY: The Global namespace and fixed name make this lock shared by
        // elevated registration instances across sessions.
        let handle =
            unsafe { CreateMutexW(None, FALSE, w!("Global\\FlowerRegistrationMachineState")) }
                .map_err(|error| format!("无法创建机器注册互斥锁：{error}"))?;

        // SAFETY: `handle` is a live named mutex owned by this process.
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            return Ok(Self(handle));
        }

        let error = if wait == WAIT_FAILED {
            windows::core::Error::from_win32().to_string()
        } else {
            format!("意外等待结果 {}", wait.0)
        };
        // SAFETY: This function still owns the mutex handle but did not acquire
        // ownership, so only the handle is closed here.
        let _ = unsafe { CloseHandle(handle) };
        Err(format!("无法取得机器注册互斥锁：{error}"))
    }
}

impl Drop for MachineRegistrationLock {
    fn drop(&mut self) {
        // SAFETY: This guard acquired ownership of the named mutex and owns the
        // handle for its full lifetime.
        let _ = unsafe { ReleaseMutex(self.0) };
        // SAFETY: The handle was returned by `CreateMutexW` and is not used
        // after this close.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns the registry handle opened or created by
        // the helpers in this module.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn flower_class_key() -> String {
    format!(r"Software\Classes\CLSID\{FLOWER_TEXT_SERVICE_CLSID}")
}

fn flower_inproc_key() -> String {
    format!(
        r"Software\Classes\CLSID\{}\{}",
        FLOWER_TEXT_SERVICE_CLSID, INPROC_SERVER_KEY
    )
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn wide_z(text: &str) -> Vec<u16> {
    let mut value = wide(text);
    value.push(0);
    value
}

fn registry_error(context: &str, error: windows::Win32::Foundation::WIN32_ERROR) -> String {
    let detail = std::io::Error::from_raw_os_error(error.0 as i32);
    format!("{context}失败（Windows 错误 {}：{detail}）", error.0)
}

fn open_registry_key(
    path: &str,
    access: windows::Win32::System::Registry::REG_SAM_FLAGS,
) -> Result<Option<RegKey>, String> {
    let path = wide_z(path);
    let mut key = HKEY(ptr::null_mut());
    // SAFETY: Both strings are null-terminated UTF-16 buffers that remain live
    // for the call, and `key` is a writable output handle.
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            0,
            access | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        return Ok(None);
    }
    if result != ERROR_SUCCESS {
        return Err(registry_error("打开 64 位注册表项", result));
    }
    Ok(Some(RegKey(key)))
}

fn create_registry_key(path: &str) -> Result<(RegKey, bool), String> {
    let path = wide_z(path);
    let mut key = HKEY(ptr::null_mut());
    let mut disposition = Default::default();
    // SAFETY: `path` is null-terminated, no security attributes are supplied,
    // and both output pointers are writable and live for the call.
    let result = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE | KEY_WOW64_64KEY,
            None,
            &mut key,
            Some(&mut disposition),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(registry_error("创建 64 位注册表项", result));
    }
    Ok((RegKey(key), disposition == REG_CREATED_NEW_KEY))
}

fn set_registry_string(key: &RegKey, name: Option<&str>, value: &str) -> Result<(), String> {
    let value = wide_z(value);
    set_registry_wide_string(key, name, &value[..value.len() - 1])
}

fn set_registry_wide_string(key: &RegKey, name: Option<&str>, value: &[u16]) -> Result<(), String> {
    let mut terminated = Vec::with_capacity(value.len() + 1);
    terminated.extend_from_slice(value);
    terminated.push(0);
    let value_bytes = unsafe {
        // SAFETY: The `u16` buffer is contiguous, initialized, and remains
        // alive for the duration of `RegSetValueExW`.
        slice::from_raw_parts(terminated.as_ptr().cast::<u8>(), terminated.len() * 2)
    };
    let name = name.map(wide_z);
    let name = name
        .as_ref()
        .map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    // SAFETY: `key` is live, `name` is null-terminated or the default value,
    // and `value_bytes` describes a live REG_SZ buffer.
    let result = unsafe { RegSetValueExW(key.0, name, 0, REG_SZ, Some(value_bytes)) };
    if result != ERROR_SUCCESS {
        return Err(registry_error("写入注册表字符串", result));
    }
    Ok(())
}

struct RegistryString {
    kind: REG_VALUE_TYPE,
    wide: Vec<u16>,
}

fn read_registry_string(
    key: &RegKey,
    name: Option<&str>,
) -> Result<Option<RegistryString>, String> {
    let name = name.map(wide_z);
    let name = name
        .as_ref()
        .map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0;
    // SAFETY: `key` is live, `name` is a valid value name, and `kind`/`size`
    // are writable outputs. No data buffer is supplied during size probing.
    let result =
        unsafe { RegQueryValueExW(key.0, name, None, Some(&mut kind), None, Some(&mut size)) };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if result != ERROR_SUCCESS && result != ERROR_MORE_DATA {
        return Err(registry_error("查询注册表字符串", result));
    }
    if size % 2 != 0 {
        return Err("注册表字符串长度不是 UTF-16 对齐长度。".to_owned());
    }

    let mut bytes = vec![0u8; size as usize];
    if size != 0 {
        // SAFETY: `key` and `name` remain live. The output buffer has the exact
        // byte count returned by the size probe.
        let result = unsafe {
            RegQueryValueExW(
                key.0,
                name,
                None,
                Some(&mut kind),
                Some(bytes.as_mut_ptr()),
                Some(&mut size),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(registry_error("读取注册表字符串", result));
        }
        bytes.truncate(size as usize);
    }

    let mut wide = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let Some(terminator) = wide.iter().position(|character| *character == 0) else {
        return Err("REG_SZ 字符串缺少 NUL 终止符。".to_owned());
    };
    if wide[terminator + 1..]
        .iter()
        .any(|character| *character != 0)
    {
        return Err("REG_SZ 字符串在终止符后仍有数据。".to_owned());
    }
    wide.truncate(terminator);
    Ok(Some(RegistryString { kind, wide }))
}

fn registry_string_equals(value: &RegistryString, expected: &str) -> bool {
    value.kind == REG_SZ
        && String::from_utf16(&value.wide)
            .map(|value| value == expected)
            .unwrap_or(false)
}

fn registry_path_is_absolute(value: &RegistryString) -> bool {
    value.kind == REG_SZ && validate_wide_dll_path(&value.wide).is_ok()
}

fn registry_paths_equal(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            *left == *right
                || u8::try_from(*left)
                    .ok()
                    .zip(u8::try_from(*right).ok())
                    .is_some_and(|(left, right)| left.eq_ignore_ascii_case(&right))
        })
}

fn registry_value_names(key: &RegKey) -> Result<Vec<String>, String> {
    let mut value_count = 0;
    let mut max_name_length = 0;
    // SAFETY: `key` is live and the requested count/length outputs are
    // writable; all other outputs are intentionally omitted.
    let result = unsafe {
        RegQueryInfoKeyW(
            key.0,
            PWSTR::null(),
            None,
            None,
            None,
            None,
            None,
            Some(&mut value_count),
            Some(&mut max_name_length),
            None,
            None,
            None,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(registry_error("查询注册表值数量", result));
    }

    let mut names = Vec::with_capacity(value_count as usize);
    let mut buffer = vec![0u16; max_name_length as usize + 1];
    for index in 0..value_count {
        let mut name_length = buffer.len() as u32;
        // SAFETY: `buffer` is writable for the declared capacity, and the
        // other optional outputs are intentionally omitted.
        let result = unsafe {
            RegEnumValueW(
                key.0,
                index,
                PWSTR(buffer.as_mut_ptr()),
                &mut name_length,
                None,
                None,
                None,
                None,
            )
        };
        if result == ERROR_NO_MORE_ITEMS {
            break;
        }
        if result != ERROR_SUCCESS {
            return Err(registry_error("枚举注册表值", result));
        }
        let name = String::from_utf16(&buffer[..name_length as usize])
            .map_err(|_| "注册表值名称不是有效 UTF-16。".to_owned())?;
        names.push(name);
    }
    Ok(names)
}

fn registry_subkey_names(key: &RegKey) -> Result<Vec<String>, String> {
    let mut subkey_count = 0;
    let mut max_name_length = 0;
    // SAFETY: `key` is live and the requested count/length outputs are
    // writable; all other outputs are intentionally omitted.
    let result = unsafe {
        RegQueryInfoKeyW(
            key.0,
            PWSTR::null(),
            None,
            None,
            Some(&mut subkey_count),
            Some(&mut max_name_length),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(registry_error("查询注册表子项数量", result));
    }

    let mut names = Vec::with_capacity(subkey_count as usize);
    let mut buffer = vec![0u16; max_name_length as usize + 1];
    for index in 0..subkey_count {
        let mut name_length = buffer.len() as u32;
        // SAFETY: `buffer` is writable for the declared capacity, and the
        // other optional outputs are intentionally omitted.
        let result = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                PWSTR(buffer.as_mut_ptr()),
                &mut name_length,
                None,
                PWSTR::null(),
                None,
                None,
            )
        };
        if result == ERROR_NO_MORE_ITEMS {
            break;
        }
        if result != ERROR_SUCCESS {
            return Err(registry_error("枚举注册表子项", result));
        }
        let name = String::from_utf16(&buffer[..name_length as usize])
            .map_err(|_| "注册表子项名称不是有效 UTF-16。".to_owned())?;
        names.push(name);
    }
    Ok(names)
}

fn registry_names_are_only(names: &[String], allowed: &[&str]) -> bool {
    names.len() == allowed.len()
        && allowed
            .iter()
            .all(|allowed| names.iter().any(|name| name.eq_ignore_ascii_case(allowed)))
}

struct ComInspection {
    state: ResourceState,
    dll_path: Option<PathBuf>,
}

impl ComInspection {
    fn missing() -> Self {
        Self {
            state: ResourceState::Missing,
            dll_path: None,
        }
    }

    fn conflict() -> Self {
        Self {
            state: ResourceState::Conflict,
            dll_path: None,
        }
    }

    fn present(dll_path: PathBuf) -> Self {
        Self {
            state: ResourceState::PresentOwned,
            dll_path: Some(dll_path),
        }
    }
}

fn inspect_com(expected_dll: Option<&Path>) -> Result<ComInspection, String> {
    let Some(class_key) = open_registry_key(&flower_class_key(), KEY_READ)? else {
        return Ok(ComInspection::missing());
    };
    if !registry_names_are_only(&registry_value_names(&class_key)?, &[""]) {
        return Ok(ComInspection::conflict());
    }
    let Some(description) = read_registry_string(&class_key, None)? else {
        return Ok(ComInspection::conflict());
    };
    if !registry_string_equals(&description, FLOWER_TEXT_SERVICE_DESCRIPTION) {
        return Ok(ComInspection::conflict());
    }
    if !registry_names_are_only(&registry_subkey_names(&class_key)?, &[INPROC_SERVER_KEY]) {
        return Ok(ComInspection::conflict());
    }

    let Some(inproc_key) = open_registry_key(&flower_inproc_key(), KEY_READ)? else {
        return Ok(ComInspection::conflict());
    };
    if !registry_names_are_only(&registry_subkey_names(&inproc_key)?, &[]) {
        return Ok(ComInspection::conflict());
    }
    if !registry_names_are_only(
        &registry_value_names(&inproc_key)?,
        &["", THREADING_MODEL_KEY],
    ) {
        return Ok(ComInspection::conflict());
    }
    let Some(dll_path) = read_registry_string(&inproc_key, None)? else {
        return Ok(ComInspection::conflict());
    };
    let Some(threading_model) = read_registry_string(&inproc_key, Some(THREADING_MODEL_KEY))?
    else {
        return Ok(ComInspection::conflict());
    };
    if !registry_path_is_absolute(&dll_path)
        || !registry_string_equals(&threading_model, APARTMENT_THREADING_MODEL)
    {
        return Ok(ComInspection::conflict());
    }

    if let Some(expected_dll) = expected_dll {
        let expected = expected_dll.as_os_str().encode_wide().collect::<Vec<_>>();
        validate_wide_dll_path(&expected).map_err(|error| error.to_string())?;
        if !registry_paths_equal(&dll_path.wide, &expected) {
            return Ok(ComInspection::conflict());
        }
    }
    Ok(ComInspection::present(PathBuf::from(OsString::from_wide(
        &dll_path.wide,
    ))))
}

fn registered_dll_exists() -> Result<bool, String> {
    let inspection = inspect_com(None)?;
    if inspection.state != ResourceState::PresentOwned {
        return Ok(false);
    }
    let Some(dll_path) = inspection.dll_path else {
        return Ok(false);
    };
    Ok(fs::metadata(dll_path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false))
}

fn create_com_registration(dll: &ValidatedDll) -> Result<ResourceInstallation, String> {
    match inspect_com(Some(&dll.path))?.state {
        ResourceState::PresentOwned => return Ok(ResourceInstallation::AlreadyPresent),
        ResourceState::Conflict => {
            return Err("现有 COM 类注册不属于此 Flower 身份；未覆盖现有值。".to_owned());
        }
        ResourceState::Missing => {}
    }

    let (class_key, created) = create_registry_key(&flower_class_key())?;
    if !created {
        return Err("COM 类注册在预检后出现并发冲突；未覆盖现有项。".to_owned());
    }

    let mut child_created = false;
    let result = (|| {
        set_registry_string(&class_key, None, FLOWER_TEXT_SERVICE_DESCRIPTION)?;
        let (inproc_key, created) = create_registry_key(&flower_inproc_key())?;
        if !created {
            return Err("InprocServer32 在预检后出现并发冲突；未覆盖现有项。".to_owned());
        }
        child_created = true;
        set_registry_wide_string(&inproc_key, None, &dll.wide)?;
        set_registry_string(
            &inproc_key,
            Some(THREADING_MODEL_KEY),
            APARTMENT_THREADING_MODEL,
        )
    })();
    drop(class_key);

    if let Err(error) = result {
        return match cleanup_created_com(child_created, &dll.path) {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!("{error}\nCOM 注册回滚失败：{rollback}")),
        };
    }
    Ok(ResourceInstallation::Created)
}

fn cleanup_created_com(delete_inproc_key: bool, dll_path: &Path) -> Result<(), String> {
    if delete_inproc_key {
        execute_com_deletion(&mut WindowsComDeletionBackend, dll_path)
            .map_err(|error| error.to_string())
    } else {
        delete_registry_key(&flower_class_key())
    }
}

fn remove_com_registration() -> Result<(), String> {
    let inspection = inspect_com(None)?;
    match inspection.state {
        ResourceState::Missing => return Ok(()),
        ResourceState::Conflict => {
            return Err("COM 类注册不是可安全识别的 Flower 64 位安装；未删除。".to_owned());
        }
        ResourceState::PresentOwned => {}
    }
    let dll_path = inspection
        .dll_path
        .ok_or_else(|| "完整 COM 安装缺少 DLL 路径。".to_owned())?;
    execute_com_deletion(&mut WindowsComDeletionBackend, &dll_path)
        .map_err(|error| error.to_string())
}

fn delete_registry_key(path: &str) -> Result<(), String> {
    let path = wide_z(path);
    // SAFETY: `path` is null-terminated and `RegDeleteKeyExW` is explicitly
    // restricted to the 64-bit registry view.
    let result = unsafe {
        RegDeleteKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path.as_ptr()),
            KEY_WOW64_64KEY.0,
            0,
        )
    };
    if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        Ok(())
    } else {
        Err(registry_error("删除 Flower 注册表项", result))
    }
}

struct WindowsComDeletionBackend;

impl ComDeletionBackend for WindowsComDeletionBackend {
    fn delete_inproc_server(&mut self) -> Result<(), String> {
        delete_registry_key(&flower_inproc_key())
    }

    fn restore_inproc_server(&mut self, dll: &Path) -> Result<(), String> {
        let (inproc_key, created) = create_registry_key(&flower_inproc_key())?;
        if !created {
            return Err("恢复 InprocServer32 时发现并发出现的未知项；未覆盖。".to_owned());
        }
        let result = (|| {
            let wide = dll.as_os_str().encode_wide().collect::<Vec<_>>();
            validate_wide_dll_path(&wide).map_err(|error| error.to_string())?;
            set_registry_wide_string(&inproc_key, None, &wide)?;
            set_registry_string(
                &inproc_key,
                Some(THREADING_MODEL_KEY),
                APARTMENT_THREADING_MODEL,
            )
        })();
        drop(inproc_key);
        if let Err(error) = result {
            return match delete_registry_key(&flower_inproc_key()) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}；清理失败的恢复项也失败：{cleanup}")),
            };
        }
        Ok(())
    }

    fn delete_class_key(&mut self) -> Result<(), String> {
        delete_registry_key(&flower_class_key())
    }
}

fn com_error(context: &str, error: windows::core::Error) -> String {
    format!("{context}失败（HRESULT 0x{:08X}：{error}）", error.code().0)
}

fn profile_manager() -> Result<ITfInputProcessorProfileMgr, String> {
    // SAFETY: COM is initialized on this thread, and the requested TSF class is
    // an in-process system component.
    unsafe {
        CoCreateInstance(
            &CLSID_TF_InputProcessorProfiles,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
    }
    .map_err(|error| com_error("创建 TSF 语言配置管理器", error))
}

fn category_manager() -> Result<ITfCategoryMgr, String> {
    // SAFETY: COM is initialized on this thread, and the requested TSF class is
    // an in-process system component.
    unsafe {
        CoCreateInstance(
            &CLSID_TF_CategoryMgr,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
    }
    .map_err(|error| com_error("创建 TSF 分类管理器", error))
}

fn input_processor_profiles() -> Result<ITfInputProcessorProfiles, String> {
    // SAFETY: COM is initialized on this thread, and the requested TSF class is
    // an in-process system component.
    unsafe {
        CoCreateInstance(
            &CLSID_TF_InputProcessorProfiles,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
    }
    .map_err(|error| com_error("创建 TSF 语言配置接口", error))
}

fn inspect_language_profile() -> Result<ResourceState, String> {
    let manager = profile_manager()?;
    // SAFETY: The manager is live. LANGID 0 asks TSF for profiles across
    // languages so a conflicting Flower CLSID cannot be missed.
    let profiles = unsafe { manager.EnumProfiles(0) }
        .map_err(|error| com_error("枚举 TSF 语言配置", error))?;
    let mut found_exact = false;
    loop {
        let mut profile = [TF_INPUTPROCESSORPROFILE::default()];
        let mut fetched = 0;
        // SAFETY: The output slice and fetched count are writable and live for
        // the call.
        unsafe { profiles.Next(&mut profile, &mut fetched) }
            .map_err(|error| com_error("读取 TSF 语言配置", error))?;
        if fetched == 0 {
            break;
        }
        let profile = profile[0];
        if profile.clsid != FLOWER_CLSID {
            continue;
        }
        if profile.dwProfileType == TF_PROFILETYPE_INPUTPROCESSOR
            && profile.langid == FLOWER_LANGID_ZH_CN
            && profile.guidProfile == FLOWER_LANGUAGE_PROFILE
        {
            found_exact = true;
        } else {
            return Ok(ResourceState::Conflict);
        }
    }
    // EnumProfiles can retain a user/session cache after UnregisterProfile.
    // That ghost has no description and must not prevent installing again.
    let machine_profile = open_registry_key(
        &format!(
            r"Software\Microsoft\CTF\TIP\{FLOWER_TEXT_SERVICE_CLSID}\LanguageProfile\0x00000804\{FLOWER_LANGUAGE_PROFILE_GUID}"
        ),
        KEY_READ,
    )?;
    if !profile_requires_description(found_exact, machine_profile.is_some())? {
        return Ok(ResourceState::Missing);
    }

    let profiles = input_processor_profiles()?;
    // SAFETY: The profile identifiers are fixed values and the returned BSTR is
    // owned by the local binding until it is dropped.
    let description = unsafe {
        profiles.GetLanguageProfileDescription(
            &FLOWER_CLSID,
            FLOWER_LANGID_ZH_CN,
            &FLOWER_LANGUAGE_PROFILE,
        )
    }
    .map_err(|error| com_error("读取 TSF 语言配置描述", error))?;
    let description = String::from_utf16(description.as_wide())
        .map_err(|_| "TSF 语言配置描述不是有效 UTF-16。".to_owned())?;
    Ok(if description == FLOWER_LANGUAGE_PROFILE_DESCRIPTION {
        ResourceState::PresentOwned
    } else {
        ResourceState::Conflict
    })
}

fn profile_requires_description(enumerated: bool, machine_present: bool) -> Result<bool, String> {
    match (enumerated, machine_present) {
        (_, false) => Ok(false),
        (true, true) => Ok(true),
        (false, true) => Err("机器语言配置存在但 TSF 未能枚举；未覆盖。".to_owned()),
    }
}

#[cfg(test)]
#[test]
fn cached_profile_without_machine_registration_does_not_require_description() {
    assert_eq!(profile_requires_description(true, false), Ok(false));
    assert_eq!(profile_requires_description(false, false), Ok(false));
    assert_eq!(profile_requires_description(true, true), Ok(true));
    assert!(profile_requires_description(false, true).is_err());
}

const FLOWER_CATEGORIES: [GUID; 3] = [
    GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
];

fn category_state(categories: &[GUID], requested: GUID) -> ResourceState {
    if categories
        .iter()
        .any(|category| !FLOWER_CATEGORIES.contains(category))
    {
        ResourceState::Conflict
    } else if categories.contains(&requested) {
        ResourceState::PresentOwned
    } else {
        ResourceState::Missing
    }
}

fn inspect_category(requested: GUID) -> Result<ResourceState, String> {
    Ok(category_state(&read_categories()?, requested))
}

fn read_categories() -> Result<Vec<GUID>, String> {
    let manager = category_manager()?;
    // SAFETY: The manager and fixed class identifier are live for the call.
    let categories = unsafe { manager.EnumCategoriesInItem(&FLOWER_CLSID) }
        .map_err(|error| com_error("枚举 Flower TSF 分类", error))?;
    let mut found = Vec::new();
    loop {
        let mut category = [GUID::default()];
        let mut fetched = 0;
        // SAFETY: The output slice and fetched count are writable and live for
        // the call.
        let result = unsafe { categories.Next(&mut category, Some(&mut fetched)) };
        result
            .ok()
            .map_err(|error| com_error("读取 Flower TSF 分类", error))?;
        if fetched == 0 {
            break;
        }
        found.push(category[0]);
    }
    Ok(found)
}

#[cfg(test)]
mod category_pure_tests {
    use super::*;

    #[test]
    fn registrationpure_categories_are_exact_and_independent() {
        let keyboard = GUID_TFCAT_TIP_KEYBOARD;
        let ui = GUID_TFCAT_TIPCAP_UIELEMENTENABLED;
        let immersive = GUID::from_u128(0x13a016df_560b_46cd_947a_4c3af1e0e35d);
        assert_eq!(category_state(&[], keyboard), ResourceState::Missing);
        assert_eq!(category_state(&[], ui), ResourceState::Missing);
        assert_eq!(
            category_state(&[keyboard], keyboard),
            ResourceState::PresentOwned
        );
        assert_eq!(category_state(&[keyboard], ui), ResourceState::Missing);
        assert_eq!(category_state(&[ui], ui), ResourceState::PresentOwned);
        assert_eq!(category_state(&[ui], keyboard), ResourceState::Missing);
        assert_eq!(
            category_state(&[keyboard, ui], immersive),
            ResourceState::Missing
        );
        for category in [keyboard, ui, immersive] {
            assert_eq!(
                category_state(&[keyboard, ui, immersive], category),
                ResourceState::PresentOwned
            );
            assert_eq!(
                category_state(&[keyboard, ui, immersive, GUID::from_u128(1)], category),
                ResourceState::Conflict
            );
        }
    }
}

fn install_language_profile(dll: &Path) -> Result<ResourceInstallation, String> {
    match inspect_language_profile()? {
        ResourceState::PresentOwned => return Ok(ResourceInstallation::AlreadyPresent),
        ResourceState::Conflict => {
            return Err("现有 Flower CLSID 的其他 TSF 语言配置发生冲突；未覆盖。".to_owned());
        }
        ResourceState::Missing => {}
    }

    let manager = profile_manager()?;
    let description = wide(FLOWER_LANGUAGE_PROFILE_DESCRIPTION);
    let icon_file = wide_path(dll);
    // SAFETY: The fixed identifiers, description, and validated DLL icon path
    // remain live for the call. HKL substitute is NULL, preferred layout is 0,
    // and the profile is not enabled for all users by default.
    unsafe {
        manager.RegisterProfile(
            &FLOWER_CLSID,
            FLOWER_LANGID_ZH_CN,
            &FLOWER_LANGUAGE_PROFILE,
            &description,
            &icon_file,
            FLOWER_PROFILE_ICON_INDEX,
            HKL(ptr::null_mut()),
            0,
            FALSE,
            0,
        )
    }
    .map_err(|error| com_error("注册 TSF 语言配置", error))?;
    Ok(ResourceInstallation::Created)
}

fn remove_language_profile() -> Result<(), String> {
    match inspect_language_profile()? {
        ResourceState::Missing => return Ok(()),
        ResourceState::Conflict => {
            return Err("TSF 中存在同一 Flower CLSID 的未知语言配置；未删除。".to_owned());
        }
        ResourceState::PresentOwned => {}
    }

    let manager = profile_manager()?;
    // SAFETY: The manager and fixed identifiers remain live for the call.
    let result = unsafe {
        manager.UnregisterProfile(
            &FLOWER_CLSID,
            FLOWER_LANGID_ZH_CN,
            &FLOWER_LANGUAGE_PROFILE,
            0,
        )
    };
    if let Err(error) = result {
        match inspect_language_profile() {
            Ok(ResourceState::Missing) => return Ok(()),
            Ok(_) => return Err(com_error("注销 TSF 语言配置", error)),
            Err(check_error) => {
                return Err(format!(
                    "{}；复核 TSF 语言配置状态失败：{check_error}",
                    com_error("注销 TSF 语言配置", error)
                ));
            }
        }
    }
    Ok(())
}

fn install_category(category: GUID) -> Result<ResourceInstallation, String> {
    match inspect_category(category)? {
        ResourceState::PresentOwned => return Ok(ResourceInstallation::AlreadyPresent),
        ResourceState::Conflict => {
            return Err("Flower TSF 分类状态冲突；未覆盖。".to_owned());
        }
        ResourceState::Missing => {}
    }

    let manager = category_manager()?;
    // SAFETY: The manager and all fixed GUIDs remain live for the call.
    unsafe { manager.RegisterCategory(&FLOWER_CLSID, &category, &FLOWER_CLSID) }
        .map_err(|error| com_error("注册 TSF 分类", error))?;
    Ok(ResourceInstallation::Created)
}

fn remove_category(category: GUID) -> Result<(), String> {
    match inspect_category(category)? {
        ResourceState::Missing => return Ok(()),
        ResourceState::Conflict => {
            return Err("Flower TSF 分类状态冲突；未删除。".to_owned());
        }
        ResourceState::PresentOwned => {}
    }

    let manager = category_manager()?;
    // SAFETY: The manager and all fixed GUIDs remain live for the call.
    let result = unsafe { manager.UnregisterCategory(&FLOWER_CLSID, &category, &FLOWER_CLSID) };
    if let Err(error) = result {
        match inspect_category(category) {
            Ok(ResourceState::Missing) => return Ok(()),
            Ok(_) => return Err(com_error("注销 TSF 分类", error)),
            Err(check_error) => {
                return Err(format!(
                    "{}；复核 TSF 分类状态失败：{check_error}",
                    com_error("注销 TSF 分类", error)
                ));
            }
        }
    }
    Ok(())
}

struct WindowsRegistrationBackend;

impl RegistrationBackend for WindowsRegistrationBackend {
    fn inspect_registration(
        &mut self,
        expected_dll: Option<&Path>,
    ) -> Result<InstallationSnapshot, String> {
        let categories = read_categories()?;
        Ok(InstallationSnapshot {
            com: inspect_com(expected_dll)?.state,
            language_profile: inspect_language_profile()?,
            keyboard_category: category_state(&categories, GUID_TFCAT_TIP_KEYBOARD),
            ui_element_category: category_state(&categories, GUID_TFCAT_TIPCAP_UIELEMENTENABLED),
            immersive_category: category_state(&categories, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT),
        })
    }

    fn install_resource(
        &mut self,
        resource: RegistrationResource,
        dll: &Path,
    ) -> Result<ResourceInstallation, String> {
        match resource {
            RegistrationResource::Com => {
                let validated = validate_dll(dll).map_err(|error| error.to_string())?;
                create_com_registration(&validated)
            }
            RegistrationResource::LanguageProfile => install_language_profile(dll),
            RegistrationResource::KeyboardCategory => install_category(GUID_TFCAT_TIP_KEYBOARD),
            RegistrationResource::UiElementCategory => {
                install_category(GUID_TFCAT_TIPCAP_UIELEMENTENABLED)
            }
            RegistrationResource::ImmersiveCategory => {
                install_category(GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT)
            }
        }
    }

    fn remove_resource(&mut self, resource: RegistrationResource) -> Result<(), String> {
        match resource {
            RegistrationResource::Com => remove_com_registration(),
            RegistrationResource::LanguageProfile => remove_language_profile(),
            RegistrationResource::KeyboardCategory => remove_category(GUID_TFCAT_TIP_KEYBOARD),
            RegistrationResource::UiElementCategory => {
                remove_category(GUID_TFCAT_TIPCAP_UIELEMENTENABLED)
            }
            RegistrationResource::ImmersiveCategory => {
                remove_category(GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT)
            }
        }
    }
}

struct OwnedModule(HMODULE);

impl Drop for OwnedModule {
    fn drop(&mut self) {
        // SAFETY: The module handle was returned by `LoadLibraryExW` and is
        // owned by this guard.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

type InstallLayoutOrTip = unsafe extern "system" fn(*const u16, u32) -> BOOL;

struct WindowsUserLayoutBackend;

impl UserLayoutBackend for WindowsUserLayoutBackend {
    fn machine_installation_complete(&mut self) -> Result<bool, String> {
        let mut backend = WindowsRegistrationBackend;
        Ok(backend.inspect_registration(None)?.is_complete() && registered_dll_exists()?)
    }

    fn apply_user_layout(&mut self, operation: UserLayoutOperation) -> Result<(), String> {
        // SAFETY: The loader resolves `input.dll` only from System32 and does
        // not use the process working directory or Flower's DLL directory.
        let module = unsafe {
            LoadLibraryExW(
                w!("input.dll"),
                HANDLE::default(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map_err(|error| format!("无法从 System32 加载 input.dll：{error}"))?;
        let module = OwnedModule(module);

        // SAFETY: `input.dll` is a system module and the requested export has
        // the documented `BOOL WINAPI InstallLayoutOrTip(PCWSTR, DWORD)` ABI.
        let procedure = unsafe { GetProcAddress(module.0, s!("InstallLayoutOrTip")) }
            .ok_or_else(|| "System32\\input.dll 未导出 InstallLayoutOrTip。".to_owned())?;
        // SAFETY: `GetProcAddress` returned a live function address. The
        // function signature is fixed by the documented input.dll ABI.
        let install_layout_or_tip: InstallLayoutOrTip = unsafe { std::mem::transmute(procedure) };

        let profile = wide_z(crate::registration::FLOWER_INSTALL_LAYOUT_OR_TIP_PROFILE);
        // SAFETY: The profile is a live null-terminated UTF-16 string and the
        // flags are restricted to 0 or ILOT_UNINSTALL.
        let succeeded = unsafe {
            install_layout_or_tip(profile.as_ptr(), operation.install_layout_or_tip_flags())
        };
        if !succeeded.as_bool() {
            return Err(
                "InstallLayoutOrTip 返回 FALSE。Windows 未说明失败原因；本工具不把 GetLastError 当作可信诊断。"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

pub fn register(dll_path: &Path) -> Result<OperationOutcome, OperationError> {
    require_elevated_terminal().map_err(|message| OperationError {
        message,
        rollback_errors: Vec::new(),
    })?;
    let _machine_lock = MachineRegistrationLock::acquire().map_err(|message| OperationError {
        message,
        rollback_errors: Vec::new(),
    })?;
    let validated = validate_dll(dll_path).map_err(|error| OperationError {
        message: error.to_string(),
        rollback_errors: Vec::new(),
    })?;
    super::install_path::check_input_method_image_access(&validated.path).map_err(|error| {
        OperationError {
            message: format!("输入法 DLL 无法满足现代 Windows 宿主的访问要求：{error}。未注册。"),
            rollback_errors: Vec::new(),
        }
    })?;
    let _apartment = ComApartment::initialize().map_err(|message| OperationError {
        message,
        rollback_errors: Vec::new(),
    })?;
    let mut backend = WindowsRegistrationBackend;
    execute_registration(&mut backend, &validated.path)
}

pub fn unregister() -> Result<OperationOutcome, OperationError> {
    require_elevated_terminal().map_err(|message| OperationError {
        message,
        rollback_errors: Vec::new(),
    })?;
    let _machine_lock = MachineRegistrationLock::acquire().map_err(|message| OperationError {
        message,
        rollback_errors: Vec::new(),
    })?;
    let _apartment = ComApartment::initialize().map_err(|message| OperationError {
        message,
        rollback_errors: Vec::new(),
    })?;
    let mut backend = WindowsRegistrationBackend;
    execute_unregistration(&mut backend)
}

/// Read-only ownership gate for the current installed settings executable.
/// Does not register, enable, or write current-user configuration.
pub fn validate_current_settings_installation() -> Result<PathBuf, String> {
    let executable = fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if !executable.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .eq_ignore_ascii_case("flower-settings.exe")
    }) {
        return Err("当前程序不是 Flower 设置载荷。".into());
    }
    let directory = super::install_path::normalize_runtime_directory(
        executable.parent().ok_or("缺少载荷目录。")?,
    )?;
    super::install_path::check_install_path(&directory)?;
    let validated = validate_dll(&directory.join("flower.dll")).map_err(|e| e.to_string())?;
    let _apartment = ComApartment::initialize()?;
    let mut backend = WindowsRegistrationBackend;
    if !backend
        .inspect_registration(Some(&validated.path))?
        .is_complete()
    {
        return Err("机器注册不属于当前完整 Flower 安装。".into());
    }
    Ok(directory.join("flower-settings.exe"))
}

pub fn enable() -> Result<OperationOutcome, OperationError> {
    let _apartment = ComApartment::initialize().map_err(|message| OperationError {
        message,
        rollback_errors: Vec::new(),
    })?;
    execute_user_layout(&mut WindowsUserLayoutBackend, UserLayoutOperation::Enable)
}

pub fn disable() -> Result<OperationOutcome, OperationError> {
    execute_user_layout(&mut WindowsUserLayoutBackend, UserLayoutOperation::Disable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_extended_paths_remain_absolute_utf16() {
        let path = PathBuf::from(r"\\?\C:\输入法\flower.dll");
        let encoded = wide_path(&path);
        assert_eq!(encoded, wide(r"\\?\C:\输入法\flower.dll"));
        assert!(validate_wide_dll_path(&encoded).is_ok());
    }

    #[test]
    fn registry_paths_compare_ascii_case_insensitively() {
        assert!(registry_paths_equal(
            &wide(r"C:\Flower\flower.dll"),
            &wide(r"c:\flower\FLOWER.DLL")
        ));
    }
}
