//! Fail-closed checks and explicit preparation of machine installation paths.
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    GetAce, IsValidSid, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, GetDriveTypeW, GetFileInformationByHandle, QueryDosDeviceW,
    BY_HANDLE_FILE_INFORMATION, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
};
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;

const SYSTEM: &str = "S-1-5-18";
const ADMINS: &str = "S-1-5-32-544";
const INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
const CREATOR_OWNER: &str = "S-1-3-0";
const APP_PACKAGES: &str = "S-1-15-2-1";
const RESTRICTED_APP_PACKAGES: &str = "S-1-15-2-2";
const OBJECT_AND_CONTAINER_INHERIT: u8 = 0x03;
const INHERIT_ONLY: u8 = 0x08;
const REPARSE_POINT: u32 = 0x400;
// DELETE, WRITE_DAC, WRITE_OWNER, GENERIC_ALL, MAXIMUM_ALLOWED, FILE_DELETE_CHILD.
const ANCESTOR_MUTATION: u32 = 0x120d_0040;
const TREE_MUTATION: u32 = ANCESTOR_MUTATION | 0x4000_0116;
const INSTALL_DIRECTORY_SDDL: &str =
    "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)(A;OICI;GRGX;;;AC)(A;OICI;GRGX;;;S-1-15-2-2)";

fn trusted(sid: &str) -> bool {
    matches!(sid, SYSTEM | ADMINS | INSTALLER)
}

fn allow_is_safe(sid: &str, mask: u32, flags: u8, strict: bool) -> bool {
    if !strict && flags & INHERIT_ONLY != 0 {
        return true;
    }
    if trusted(sid) || (sid == CREATOR_OWNER && flags & INHERIT_ONLY != 0) {
        return true;
    }
    mask & if strict {
        TREE_MUTATION
    } else {
        ANCESTOR_MUTATION
    } == 0
}

fn path_allow_is_safe(sid: &str, mask: u32, flags: u8, strict: bool, volume_root: bool) -> bool {
    // A verified local volume root cannot itself be removed or renamed like a
    // directory. DELETE on it does not grant FILE_DELETE_CHILD. Never ignore
    // deletion on ordinary ancestors, or DACL/owner/child replacement rights.
    let mask = if volume_root && !strict {
        mask & !0x0001_0000
    } else {
        mask
    };
    allow_is_safe(sid, mask, flags, strict)
}

fn validate_spelling(text: &str) -> Result<(), String> {
    let b = text.as_bytes();
    if b.len() < 4 || !b[0].is_ascii_alphabetic() || &b[1..3] != b":\\" {
        return Err("只接受本地盘符绝对目录（例如 D:\\Software\\Flower），不能为卷根。".into());
    }
    if text.len() > 220 {
        return Err("安装根目录过长（最多 220 UTF-8 字节）。".into());
    }
    for part in text[3..].split('\\') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || stem.strip_prefix("COM").is_some_and(reserved_number)
            || stem.strip_prefix("LPT").is_some_and(reserved_number);
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with([' ', '.'])
            || part.chars().any(|c| c < ' ' || r#"/:*?"<>|"#.contains(c))
            || reserved
        {
            return Err("目录含空段、保留名称、点路径或 Windows 不允许的字符。".into());
        }
    }
    Ok(())
}

/// Adapt runtime module-directory spelling only, without filesystem access.
/// This is not a safety proof: callers must still use `check_install_path`
/// before checking their fixed helper and model paths. Never canonicalize here,
/// since that could hide reparse points from the existing tree inspection.
pub(crate) fn normalize_runtime_directory(path: &Path) -> Result<PathBuf, String> {
    let text = path.to_str().ok_or("目录必须为有效 Unicode")?;
    let normalized = match text.strip_prefix(r"\\?\") {
        Some(local)
            if local
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
                && local.as_bytes().get(1..3) == Some(b":\\") =>
        {
            local
        }
        _ => text,
    };
    validate_spelling(normalized)?;
    Ok(PathBuf::from(normalized))
}

fn reserved_number(s: &str) -> bool {
    matches!(
        s,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(HLOCAL(self.0));
        }
    }
}

unsafe fn sid_string(sid: PSID) -> Result<String, String> {
    if sid.0.is_null() || !IsValidSid(sid).as_bool() {
        return Err("无效 SID".into());
    }
    let mut result = PWSTR::null();
    ConvertSidToStringSidW(sid, &mut result).map_err(|e| e.to_string())?;
    let _memory = LocalAllocation(result.0.cast());
    result.to_string().map_err(|e| e.to_string())
}

fn inspect(path: &Path, strict: bool) -> Result<(bool, File), String> {
    inspect_with_package_access(path, strict, None)
}

fn inspect_with_package_access(
    path: &Path,
    strict: bool,
    required_package_flags: Option<u8>,
) -> Result<(bool, File), String> {
    // Do not follow the final component; every ancestor is independently inspected.
    // Keep a no-delete-share handle open throughout the descriptor inspection.
    let file = OpenOptions::new()
        .access_mode(0x0002_0000) // READ_CONTROL
        .share_mode(3)
        .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err("不允许重解析点、符号链接或目录联接".into());
    }
    if !metadata.is_dir() && !metadata.is_file() {
        return Err("不允许未知文件类型".into());
    }
    unsafe {
        let handle = HANDLE(file.as_raw_handle());
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        GetFileInformationByHandle(handle, &mut info).map_err(|e| e.to_string())?;
        if !metadata.is_dir() && info.nNumberOfLinks != 1 {
            return Err("不允许多硬链接文件".into());
        }
        let mut owner = PSID::default();
        let mut acl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let status = GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            Some(&mut descriptor),
        );
        let _memory = LocalAllocation(descriptor.0);
        status.ok().map_err(|e| e.to_string())?;
        let volume_root = path.parent().is_none() && path.has_root();
        inspect_acl(owner, acl, strict, volume_root, required_package_flags)?;
    }
    Ok((metadata.is_dir(), file))
}

// The pointers must belong to a live, OS-validated security descriptor.
unsafe fn inspect_acl(
    owner: PSID,
    acl: *const ACL,
    strict: bool,
    volume_root: bool,
    required_package_flags: Option<u8>,
) -> Result<(), String> {
    if !trusted(&sid_string(owner)?) {
        return Err("owner 必须为 SYSTEM、Administrators 或 TrustedInstaller".into());
    }
    if acl.is_null() {
        return Err("不允许 null DACL".into());
    }
    let mut packages = [false; 2];
    for index in 0..(*acl).AceCount {
        let mut ace = std::ptr::null_mut();
        GetAce(acl, u32::from(index), &mut ace).map_err(|e| e.to_string())?;
        let header = std::slice::from_raw_parts(ace.cast::<u8>(), 4);
        let size = u16::from_le_bytes([header[2], header[3]]) as usize;
        match header[0] {
            1 => continue, // ACCESS_DENIED_ACE never grants rights.
            0 => {}
            _ => return Err("无法识别的 DACL ACE，保守拒绝".into()),
        }
        if size < 16 {
            return Err("无效 allow ACE".into());
        }
        let bytes = std::slice::from_raw_parts(ace.cast::<u8>(), size);
        let mask = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let sid = sid_string(PSID(ace.cast::<u8>().add(8).cast()))?;
        if !path_allow_is_safe(&sid, mask, header[1], strict, volume_root) {
            return Err(format!("非可信 SID {sid} 具有写入或替换权限"));
        }
        if let Some(flags) = required_package_flags {
            // Require an allow ACE that applies to this object. For the install
            // root it must also propagate to files and subdirectories.
            if matches_package_read_execute(mask, header[1], flags) {
                match sid.as_str() {
                    APP_PACKAGES => packages[0] = true,
                    RESTRICTED_APP_PACKAGES => packages[1] = true,
                    _ => {}
                }
            }
        }
    }
    if required_package_flags.is_some() && !packages.into_iter().all(|allowed| allowed) {
        return Err("缺少 Windows 应用容器的读取和执行权限".into());
    }
    Ok(())
}

fn matches_package_read_execute(mask: u32, flags: u8, required_flags: u8) -> bool {
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_EXECUTE: u32 = 0x2000_0000;
    const NO_PROPAGATE_INHERIT: u8 = 0x04;
    let read = mask & GENERIC_READ != 0 || mask & FILE_GENERIC_READ.0 == FILE_GENERIC_READ.0;
    let execute =
        mask & GENERIC_EXECUTE != 0 || mask & FILE_GENERIC_EXECUTE.0 == FILE_GENERIC_EXECUTE.0;
    flags & INHERIT_ONLY == 0
        && flags & required_flags == required_flags
        && (required_flags == 0 || flags & NO_PROPAGATE_INHERIT == 0)
        && read
        && execute
}

/// Require sandbox read/execute grants on the actual image before advertising
/// immersive support. This does not modify the DLL or its descriptor.
pub(super) fn check_input_method_image_access(path: &Path) -> Result<(), String> {
    let (is_directory, _) = inspect_with_package_access(path, true, Some(0))?;
    if is_directory {
        return Err("输入法 DLL 路径不能是目录".into());
    }
    Ok(())
}

/// Validate a prospective root or an existing installation tree, without writes.
pub fn check_install_path(path: &Path) -> Result<(), String> {
    check_path(path, false).map(|_| ())
}

/// Create only missing directories with their final protected descriptor.
/// Existing objects are never repaired, re-owned or removed.
pub fn prepare_install_path(path: &Path) -> Result<(), String> {
    let (mut guards, components) = check_path(path, false)?;
    for component in components {
        create_protected_directory(&component)?;
        let (directory, guard) = inspect(&component, true)?;
        if !directory {
            return Err("新建组件不是目录。".into());
        }
        guards.push(guard);
    }
    // Keep every earlier no-delete handle alive until the strict recheck ends.
    let final_guards = check_path(path, true)?;
    inspect_with_package_access(path, true, Some(OBJECT_AND_CONTAINER_INHERIT))
        .map_err(|error| format!("安装目标 {}：{error}", path.display()))?;
    drop(final_guards);
    drop(guards);
    Ok(())
}

fn create_protected_directory(path: &Path) -> Result<(), String> {
    unsafe {
        let input = wide(OsStr::new(INSTALL_DIRECTORY_SDDL));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(input.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
        .map_err(|e| e.to_string())?;
        let _memory = LocalAllocation(descriptor.0);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let name = wide(path.as_os_str());
        // Unlike create_dir_all, ERROR_ALREADY_EXISTS is a failure, not adoption.
        CreateDirectoryW(PCWSTR(name.as_ptr()), Some(&attributes))
            .map_err(|e| format!("无法安全创建 {}：{e}", path.display()))
    }
}

fn validate_local_path(path: &Path) -> Result<(), String> {
    let text = path.to_str().ok_or("目录必须为有效 Unicode")?;
    validate_spelling(text)?;
    unsafe {
        let root = wide(OsStr::new(&text[..3]));
        if GetDriveTypeW(PCWSTR(root.as_ptr())) != 3 {
            return Err("只接受本地固定磁盘。".into());
        }
        let drive = wide(OsStr::new(&text[..2]));
        let mut device = [0u16; 1024];
        let count = QueryDosDeviceW(PCWSTR(drive.as_ptr()), Some(&mut device));
        if count == 0 {
            return Err("无法确认磁盘设备。".into());
        }
        let device = String::from_utf16_lossy(&device[..count as usize]);
        if !device.starts_with("\\Device\\HarddiskVolume") {
            return Err("不允许 SUBST、网络映射或未知磁盘设备。".into());
        }
        let mut windows = [0u16; 32768];
        let count = GetWindowsDirectoryW(Some(&mut windows));
        if count == 0 || count as usize >= windows.len() {
            return Err("无法确定 Windows 系统目录。".into());
        }
        let system = String::from_utf16_lossy(&windows[..count as usize]).to_lowercase();
        let candidate = text.to_lowercase();
        if candidate == system || candidate.starts_with(&(system + "\\")) {
            return Err("不能安装到 Windows 系统目录树。".into());
        }
    }
    Ok(())
}

fn check_path(path: &Path, require_existing: bool) -> Result<(Vec<File>, Vec<PathBuf>), String> {
    validate_local_path(path)?;
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    let mut existing = Vec::new();
    let mut missing = false;
    let mut missing_components = Vec::new();
    for ancestor in ancestors {
        match fs::symlink_metadata(ancestor) {
            Ok(_) if !missing => existing.push(ancestor),
            Ok(_) => return Err("目录结构在检查期间发生变化。".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing = true;
                missing_components.push(ancestor.to_path_buf());
            }
            Err(e) => return Err(format!("{}：{e}", ancestor.display())),
        }
    }
    if require_existing && missing {
        return Err("准备后的安装目录不存在。".into());
    }
    let mut guards = Vec::new();
    for (index, ancestor) in existing.iter().enumerate() {
        let strict = !missing && index + 1 == existing.len();
        let (directory, guard) = inspect(ancestor, strict).map_err(|e| {
            format!(
                "{}不安全：{}：{e}",
                if strict {
                    "已有安装目标"
                } else {
                    "安装路径祖先"
                },
                ancestor.display()
            )
        })?;
        guards.push(guard);
        if !directory {
            return Err(format!("{} 不是目录", ancestor.display()));
        }
    }
    if !missing {
        let mut pending = vec![PathBuf::from(path)];
        let mut count = 0usize;
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                count += 1;
                if count > 4096 {
                    return Err("安装树过大，拒绝检查未知目录。".into());
                }
                let (directory, guard) = inspect(&entry.path(), true)
                    .map_err(|e| format!("{}：{e}", entry.path().display()))?;
                guards.push(guard);
                if directory {
                    pending.push(entry.path());
                }
            }
        }
    }
    Ok((guards, missing_components))
}

const USER_FILES: [&str; 4] = [
    "flower-settings.exe",
    "README.md",
    "LICENSE-MIT",
    "LICENSE-APACHE",
];

fn user_directory_guard(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .access_mode(0x80) // FILE_READ_ATTRIBUTES, no ACL/owner requirement
        .share_mode(1) // deny write and delete, including reparse mutation
        .custom_flags(0x0220_0000)
        .open(path)
        .map_err(|e| format!("{}：{e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !ordinary_user_directory(metadata.is_dir(), metadata.file_attributes()) {
        return Err(format!("{} 不是普通目录（不允许重解析点）", path.display()));
    }
    Ok(file)
}

fn ordinary_user_directory(is_directory: bool, attributes: u32) -> bool {
    is_directory && attributes & REPARSE_POINT == 0
}

fn user_path_guards(path: &Path, create: bool) -> Result<Vec<File>, String> {
    validate_local_path(path)?;
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    let mut guards = Vec::new();
    let mut missing = false;
    for directory in ancestors {
        match fs::symlink_metadata(directory) {
            Ok(_) if missing => return Err("用户目录在检查期间发生变化。".into()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    missing = true;
                    continue;
                }
                // No create_dir_all and no adoption of ERROR_ALREADY_EXISTS.
                fs::create_dir(directory)
                    .map_err(|e| format!("无法以普通用户创建 {}：{e}", directory.display()))?;
            }
            Err(e) => return Err(e.to_string()),
        }
        guards.push(user_directory_guard(directory)?);
    }
    if !missing {
        for name in USER_FILES {
            match fs::symlink_metadata(path.join(name)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => return Err(format!("目标文件 {name} 已存在，拒绝覆盖。")),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Ok(guards)
}

/// Read-only syntax/volume/reparse checks, intentionally without machine ACL rules.
pub fn check_user_install_path(path: &Path) -> Result<(), String> {
    user_path_guards(path, false).map(|_| ())
}

fn require_non_elevated() -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| e.to_string())?;
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let _ = CloseHandle(token);
        result.map_err(|e| e.to_string())?;
        if elevation.TokenIsElevated != 0 {
            return Err(
                "用户文件部署拒绝提升令牌；请由原始普通用户运行，不会回退管理员写入。".into(),
            );
        }
    }
    Ok(())
}

/// Copy only fixed names beside this executable. Never overwrite or traverse
/// arbitrary payload entries. Cleanup uses owned handles, never a replaceable name.
pub fn install_user_files(path: &Path) -> Result<(), String> {
    require_non_elevated()?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    // current_exe may return the Win32 extended-length spelling. Normalize only
    // this OS-provided source; user-supplied extended/device paths stay rejected.
    let executable = match executable.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(dos_path) => PathBuf::from(dos_path),
        None => executable,
    };
    let source = executable.parent().ok_or("无法确定工具目录")?;
    // Also pin the trusted source tree for the duration of all reads.
    let (source_guards, _) = check_path(source, true)?;
    let target_guards = user_path_guards(path, true)?;
    let result = copy_fixed_user_files(source, path);
    drop(target_guards);
    drop(source_guards);
    result
}

// Private implementation, not a trust-bypassing CLI. The caller holds source
// and destination guards; production must first pass token and source checks.
fn copy_fixed_user_files(source: &Path, path: &Path) -> Result<(), String> {
    use windows::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };
    let mut created: Vec<File> = Vec::new();
    let result = (|| {
        for name in USER_FILES {
            let mut input = OpenOptions::new()
                .read(true)
                .share_mode(1)
                .custom_flags(0x0020_0000)
                .open(source.join(name))
                .map_err(|e| format!("无法读取系统副本 {name}：{e}"))?;
            let metadata = input.metadata().map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
                return Err(format!("系统副本 {name} 不是普通文件"));
            }
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .access_mode(0x4001_0000) // GENERIC_WRITE | DELETE for handle-only rollback
                .share_mode(0)
                .custom_flags(0x0020_0000)
                .open(path.join(name))
                .map_err(|e| format!("无法新建 {name}（拒绝覆盖/竞争）：{e}"))?;
            created.push(output);
            let output = created.last_mut().unwrap();
            std::io::copy(&mut input, output).map_err(|e| e.to_string())?;
            output.sync_all().map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        for file in &created {
            let disposition = FILE_DISPOSITION_INFO {
                DeleteFile: true.into(),
            };
            // Owned exclusive handles remain open: cannot delete a competitor's file.
            let cleanup = unsafe {
                SetFileInformationByHandle(
                    HANDLE(file.as_raw_handle()),
                    FileDispositionInfo,
                    (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            };
            if let Err(error) = cleanup {
                eprintln!("本次创建文件清理失败，保留供用户检查：{error}");
            }
        }
    }
    drop(created);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Explicit opt-in keeps ordinary test runs from writing to a global temp
    /// directory. The runner must supply this Thread's scratch, never another
    /// session's directory. Each fixture exclusively owns one newly created child.
    struct UserCopyFixture {
        root: PathBuf,
        source: PathBuf,
        target: PathBuf,
    }

    impl UserCopyFixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            require_non_elevated()
                .expect("隔离复制测试必须非提升运行；提升令牌已被拒绝，未执行文件 I/O");
            let scratch = PathBuf::from(
                std::env::var_os("FLOWER_INSTALL_PATH_TEST_SCRATCH")
                    .expect("请显式指定本 Thread 的 scratch；没有全局临时目录回退"),
            );
            assert!(scratch.is_absolute(), "scratch 必须为绝对路径");
            assert_eq!(scratch.file_name(), Some(OsStr::new("scratch")));
            let root = scratch.join(format!(
                "user-copy-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            // No adoption, overwrite, or cleanup of a pre-existing test root.
            fs::create_dir(&root).expect("无法独占创建测试目录");
            let fixture = Self {
                source: root.join("source"),
                target: root.join("deep").join("nested").join("target"),
                root,
            };
            fs::create_dir(&fixture.source).unwrap();
            for (index, name) in USER_FILES.iter().enumerate() {
                // The .exe is inert data; nothing in these tests launches it.
                fs::write(
                    fixture.source.join(name),
                    format!("inert fixture {index}: {name}\r\n\0payload"),
                )
                .unwrap();
            }
            fixture
        }
    }

    impl Drop for UserCopyFixture {
        fn drop(&mut self) {
            // This newly created fixture contains only our regular directories
            // and inert files. Tests never create live junctions or symlinks.
            if let Err(error) = fs::remove_dir_all(&self.root) {
                eprintln!("保留本次测试目录 {}：{error}", self.root.display());
            }
        }
    }

    #[test]
    #[ignore = "requires non-elevated token and explicit Thread scratch"]
    fn isolated_user_copy_creates_deep_target_with_matching_content_hashes() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let fixture = UserCopyFixture::new();
        let source_guard = user_directory_guard(&fixture.source).unwrap();
        let guards = user_path_guards(&fixture.target, true).unwrap();
        copy_fixed_user_files(&fixture.source, &fixture.target).unwrap();
        for name in USER_FILES {
            let original = fs::read(fixture.source.join(name)).unwrap();
            let copied = fs::read(fixture.target.join(name)).unwrap();
            // Hash equality is supplemented with exact bytes, not used as a
            // substitute for the production cryptographic payload verification.
            let hash = |bytes: &[u8]| {
                let mut hasher = DefaultHasher::new();
                bytes.hash(&mut hasher);
                hasher.finish()
            };
            assert_eq!(hash(&original), hash(&copied), "{name}");
            assert_eq!(original, copied, "{name}");
        }
        assert_eq!(fs::read_dir(&fixture.target).unwrap().count(), 4);
        drop(guards);
        drop(source_guard);
    }

    #[test]
    #[ignore = "requires non-elevated token and explicit Thread scratch"]
    fn isolated_user_copy_existing_target_is_never_overwritten() {
        let fixture = UserCopyFixture::new();
        fs::create_dir_all(&fixture.target).unwrap();
        fs::write(fixture.target.join("README.md"), b"existing owner").unwrap();
        assert!(user_path_guards(&fixture.target, true).is_err());
        assert_eq!(
            fs::read(fixture.target.join("README.md")).unwrap(),
            b"existing owner"
        );
        assert!(!fixture.target.join(USER_FILES[0]).exists());
    }

    #[test]
    #[ignore = "requires non-elevated token and explicit Thread scratch"]
    fn isolated_user_copy_later_missing_source_rolls_back_only_owned_files() {
        let fixture = UserCopyFixture::new();
        fs::remove_file(fixture.source.join(USER_FILES[2])).unwrap();
        fs::create_dir_all(fixture.target.join("unrelated")).unwrap();
        let unknown = fixture.target.join("unrelated").join("keep.txt");
        fs::write(&unknown, b"unknown data").unwrap();
        let source_guard = user_directory_guard(&fixture.source).unwrap();
        let guards = user_path_guards(&fixture.target, true).unwrap();
        let error = copy_fixed_user_files(&fixture.source, &fixture.target).unwrap_err();
        assert!(error.contains(USER_FILES[2]), "{error}");
        // The first two files were really created, copied and then removed via
        // their exclusive handles when opening the third source failed.
        for name in USER_FILES {
            assert!(!fixture.target.join(name).exists(), "{name}");
        }
        assert_eq!(fs::read(unknown).unwrap(), b"unknown data");
        drop(guards);
        drop(source_guard);
    }

    #[test]
    #[ignore = "requires non-elevated token and explicit Thread scratch"]
    fn isolated_user_copy_create_new_rejects_file_racing_after_precheck() {
        let fixture = UserCopyFixture::new();
        let source_guard = user_directory_guard(&fixture.source).unwrap();
        let guards = user_path_guards(&fixture.target, true).unwrap();
        // Deterministic competitor between the real target precheck and copy.
        let raced = fixture.target.join(USER_FILES[1]);
        fs::write(&raced, b"competitor").unwrap();
        let error = copy_fixed_user_files(&fixture.source, &fixture.target).unwrap_err();
        assert!(error.contains("拒绝覆盖/竞争"), "{error}");
        assert!(!fixture.target.join(USER_FILES[0]).exists());
        assert_eq!(fs::read(raced).unwrap(), b"competitor");
        assert!(!fixture.target.join(USER_FILES[2]).exists());
        drop(guards);
        drop(source_guard);
    }

    #[test]
    #[ignore = "requires non-elevated token and explicit Thread scratch"]
    fn isolated_user_copy_create_new_rejects_directory_racing_after_precheck() {
        let fixture = UserCopyFixture::new();
        let source_guard = user_directory_guard(&fixture.source).unwrap();
        let guards = user_path_guards(&fixture.target, true).unwrap();
        let raced = fixture.target.join(USER_FILES[1]);
        fs::create_dir(&raced).unwrap();
        fs::write(raced.join("keep.txt"), b"competitor directory").unwrap();
        assert!(copy_fixed_user_files(&fixture.source, &fixture.target).is_err());
        assert!(!fixture.target.join(USER_FILES[0]).exists());
        assert_eq!(
            fs::read(raced.join("keep.txt")).unwrap(),
            b"competitor directory"
        );
        drop(guards);
        drop(source_guard);
    }

    #[test]
    fn user_directory_reparse_attributes_are_rejected_without_creating_links() {
        assert!(ordinary_user_directory(true, 0x10));
        assert!(!ordinary_user_directory(true, 0x10 | REPARSE_POINT));
        assert!(!ordinary_user_directory(false, 0x20));
        assert!(!ordinary_user_directory(false, 0x20 | REPARSE_POINT));
    }

    #[test]
    fn user_files_are_fixed_and_never_include_machine_components() {
        assert_eq!(
            USER_FILES,
            [
                "flower-settings.exe",
                "README.md",
                "LICENSE-MIT",
                "LICENSE-APACHE",
            ]
        );
        assert!(!USER_FILES.contains(&"flower.dll"));
        assert!(!USER_FILES.contains(&"flower-register.exe"));
    }

    #[test]
    fn user_path_spelling_accepts_deep_paths_without_acl_assumptions() {
        assert!(validate_spelling(r"D:\software\Flower").is_ok());
        for invalid in [
            r"D:\",
            r"D:\software\..\Flower",
            r"D:\software\Flower.",
            r"\\server\Flower",
            r"D:\software\NUL",
            r"D:\software\file:stream",
        ] {
            assert!(validate_spelling(invalid).is_err(), "{invalid}");
        }
    }

    fn check_sddl(sddl: &str, strict: bool) -> Result<(), String> {
        check_sddl_with_package_access(sddl, strict, None)
    }

    fn check_sddl_with_package_access(
        sddl: &str,
        strict: bool,
        required_package_flags: Option<u8>,
    ) -> Result<(), String> {
        use windows::Win32::Foundation::BOOL;
        use windows::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
        use windows::Win32::Security::{GetSecurityDescriptorDacl, GetSecurityDescriptorOwner};
        unsafe {
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            let input = wide(OsStr::new(sddl));
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(input.as_ptr()),
                1,
                &mut descriptor,
                None,
            )
            .unwrap();
            let _memory = LocalAllocation(descriptor.0);
            let mut owner = PSID::default();
            let mut defaulted = BOOL::default();
            let mut present = BOOL::default();
            let mut acl = std::ptr::null_mut();
            GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted).unwrap();
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted).unwrap();
            if sddl == INSTALL_DIRECTORY_SDDL {
                use windows::Win32::Security::{GetSecurityDescriptorControl, SE_DACL_PROTECTED};
                let mut control = 0u16;
                let mut revision = 0u32;
                GetSecurityDescriptorControl(descriptor, &mut control, &mut revision).unwrap();
                assert_ne!(control & SE_DACL_PROTECTED.0, 0);
                assert_eq!(sid_string(owner).unwrap(), ADMINS);
                assert!(present.as_bool());
                assert_eq!((*acl).AceCount, 5);
                for index in 0..5 {
                    let mut ace = std::ptr::null_mut();
                    GetAce(acl, index, &mut ace).unwrap();
                    let header = std::slice::from_raw_parts(ace.cast::<u8>(), 4);
                    assert_eq!(header[0], 0); // allow
                    assert_eq!(header[1], 3); // object + container inheritance
                }
            }
            inspect_acl(owner, acl, strict, false, required_package_flags)
        }
    }

    #[test]
    fn native_descriptors_are_fail_closed_without_changing_filesystem_acl() {
        assert!(check_sddl(INSTALL_DIRECTORY_SDDL, true).is_ok());
        assert!(check_sddl("O:BAD:(A;;GW;;;BU)", false).is_ok());
        assert!(check_sddl("O:BAD:(A;;GW;;;BU)", true).is_err());
        assert!(check_sddl("O:BAD:(A;;FA;;;BA)(A;;FRFX;;;BU)", true).is_ok());
        assert!(check_sddl("O:SYD:", true).is_ok()); // empty DACL
        assert!(check_sddl("O:BAD:NO_ACCESS_CONTROL", true).is_err());
        assert!(check_sddl("O:BUD:(A;;FA;;;BA)", true).is_err()); // untrusted owner
        assert!(check_sddl("O:BAD:(A;;FW;;;AU)", true).is_err());
        assert!(check_sddl("O:BAD:(D;;FA;;;AU)(A;;FA;;;AU)", true).is_err());
        assert!(check_sddl("O:BAD:(A;OICIIO;FA;;;CO)(A;;FA;;;BA)", true).is_ok());
        assert!(check_sddl("O:BAD:(A;OICIIO;FA;;;AU)", true).is_err());
        assert!(check_sddl("O:BAD:(A;OICIIO;FA;;;AU)", false).is_ok());
        // FILE_DELETE_CHILD (SDDL "DC" is a directory-service right, not this bit).
        assert!(check_sddl("O:BAD:(A;;0x40;;;AU)", false).is_err());
        // Object ACEs are intentionally outside this conservative policy.
        assert!(check_sddl(
            "O:BAD:(OA;;FA;00112233-4455-6677-8899-aabbccddeeff;;BA)",
            true
        )
        .is_err());
    }

    #[test]
    fn app_container_access_requires_both_groups_and_never_allows_mutation() {
        let directory = Some(OBJECT_AND_CONTAINER_INHERIT);
        let image = Some(0);
        assert!(check_sddl_with_package_access(INSTALL_DIRECTORY_SDDL, true, directory).is_ok());

        let old_install = "O:BAD:P(A;OICI;FA;;;BA)(A;OICI;GRGX;;;BU)";
        assert!(
            check_sddl(old_install, true).is_ok(),
            "still safe to uninstall"
        );
        assert!(check_sddl_with_package_access(old_install, true, directory).is_err());
        let one_level_only = "O:BAD:(A;OICINP;FRFX;;;AC)(A;OICI;FRFX;;;S-1-15-2-2)";
        assert!(check_sddl_with_package_access(one_level_only, true, directory).is_err());

        // The inherited DLL ACEs usually carry mapped file rights rather than GR/GX.
        let inherited_image = "O:BAD:(A;ID;FRFX;;;AC)(A;ID;FRFX;;;S-1-15-2-2)";
        assert!(check_sddl_with_package_access(inherited_image, true, image).is_ok());
        assert!(check_sddl_with_package_access(inherited_image, true, directory).is_err());
        for invalid in [
            "O:BAD:(A;;FRFX;;;AC)",
            "O:BAD:(A;;FRFX;;;AC)(A;;FR;;;S-1-15-2-2)",
            "O:BAD:(A;OICIIO;FRFX;;;AC)(A;;FRFX;;;S-1-15-2-2)",
            "O:BAD:(A;;FRFWFX;;;AC)(A;;FRFX;;;S-1-15-2-2)",
        ] {
            assert!(
                check_sddl_with_package_access(invalid, true, image).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn normalize_runtime_directory_accepts_normal_and_verbatim_native_paths() {
        for normal in [
            r"C:\Program Files\Flower",
            r"d:\软件 和 空格\Flower",
            r"D:\Flower",
        ] {
            let verbatim = format!(r"\\?\{normal}");
            for input in [normal, verbatim.as_str()] {
                let normalized = normalize_runtime_directory(Path::new(input)).unwrap();
                assert_eq!(normalized.as_os_str(), OsStr::new(normal));
            }
            // Runtime adaptation must not widen the installation CLI grammar.
            assert!(validate_spelling(&verbatim).is_err());
        }
        let boundary = format!("C:\\{}", "a".repeat(217));
        assert_eq!(boundary.len(), 220);
        assert!(normalize_runtime_directory(Path::new(&boundary)).is_ok());
        assert!(normalize_runtime_directory(Path::new(&format!(r"\\?\{boundary}"))).is_ok());
    }

    #[test]
    fn normalize_runtime_directory_rejects_unsupported_spellings() {
        for invalid in [
            r"\\server\share\Flower",
            r"\\?\UNC\server\share\Flower",
            r"\\.\C:\Flower",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\Flower",
            r"\\?\Volume{00112233-4455-6677-8899-aabbccddeeff}\Flower",
            r"C:\",
            r"C:Flower",
            r"Flower",
            r"\Flower",
            r"C:/Flower",
            r"\\?\1:\Flower",
            r"\\?\é:\Flower",
            r"\\?\C:/Flower",
            r"\\?\",
            r"\??\C:\Flower",
        ] {
            assert!(
                normalize_runtime_directory(Path::new(invalid)).is_err(),
                "{invalid}"
            );
        }
        for invalid in [
            r"C:\",
            r"C:Flower",
            r"C:\a\.\Flower",
            r"C:\a\..\Flower",
            r"C:\a.\Flower",
            r"C:\a \Flower",
            r"C:\a:stream",
            r"C:\CON.txt",
            r"C:\COM1",
            r"C:\LPT²",
            r"C:\\Flower",
            r"C:\Flower\",
            r"C:\a/b",
            r"C:\a*b",
            r#"C:\a"b"#,
            r"C:\a?b",
            r"C:\a<b",
            r"C:\a>b",
            r"C:\a|b",
            "C:\\a\0b",
            "C:\\a\nb",
            &format!("C:\\{}", "a".repeat(218)),
            &format!("C:\\{}", "字".repeat(73)),
        ] {
            for input in [invalid.to_owned(), format!(r"\\?\{invalid}")] {
                assert!(
                    normalize_runtime_directory(Path::new(&input)).is_err(),
                    "{input}"
                );
            }
        }
        use std::os::windows::ffi::OsStringExt;
        let non_unicode =
            std::ffi::OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]);
        assert!(normalize_runtime_directory(Path::new(&non_unicode)).is_err());
    }

    #[test]
    fn accepts_drive_paths_not_unsafe_windows_spellings() {
        for p in [
            r"C:\Program Files\Flower",
            r"D:\Software\Flower",
            r"D:\软件\Flower",
        ] {
            assert!(validate_spelling(p).is_ok(), "{p}");
        }
        for p in [
            r"D:\",
            r"D:Flower",
            r"\\server\share\Flower",
            r"\\?\D:\Flower",
            r"D:\a\..\Flower",
            r"D:\a.\Flower",
            r"D:\a \Flower",
            r"D:\a:ads",
            r"D:\CON.txt",
            r"D:\COM1",
            r"D:\LPT²",
            r"D:\\Flower",
            "D:\\a\0b",
            r"D:/Flower",
        ] {
            assert!(validate_spelling(p).is_err(), "{p}");
        }
    }

    #[test]
    fn acl_policy_rejects_untrusted_writes_and_ancestor_replacement() {
        for mask in [
            2, 4, 0x10, 0x40, 0x100, 0x10000, 0x40000, 0x80000, 0x40000000, 0x10000000,
        ] {
            assert!(!allow_is_safe("S-1-5-11", mask, 0, true));
            assert!(allow_is_safe(ADMINS, mask, 0, true));
        }
        assert!(allow_is_safe("S-1-5-11", 0x1200a9, 0, true)); // read/execute
        assert!(allow_is_safe("S-1-5-11", 4, 0, false)); // root may create new dirs
        for mask in [2, 4, 0x10, 0x100, 0x40000000] {
            assert!(allow_is_safe("S-1-5-11", mask, 0, false));
            assert!(!allow_is_safe("S-1-5-11", mask, 0, true));
        }
        for mask in [0x40, 0x10000, 0x40000, 0x80000, 0x10000000, 0x02000000] {
            assert!(!allow_is_safe("S-1-5-11", mask, 0, false));
            assert!(!allow_is_safe("S-1-5-11", mask, 0, true));
        }
        assert!(!allow_is_safe("S-1-5-11", 0x40, 0, false)); // not replace children
        assert!(allow_is_safe("S-1-5-11", 0x1f01ff, INHERIT_ONLY, false));
        assert!(!allow_is_safe("S-1-5-11", 0x1f01ff, INHERIT_ONLY, true));
        assert!(allow_is_safe(CREATOR_OWNER, 0x1f01ff, INHERIT_ONLY, true));
        assert!(!allow_is_safe(CREATOR_OWNER, 0x1f01ff, 0, true));
    }

    #[test]
    fn volume_root_delete_is_not_a_child_replacement_right() {
        let users = "S-1-5-11";
        // Typical data-volume root: Modify without FILE_DELETE_CHILD.
        assert!(path_allow_is_safe(users, 0x1301bf, 0, false, true));
        assert!(!path_allow_is_safe(users, 0x1301bf, 0, false, false));
        assert!(!path_allow_is_safe(users, 0x1301bf, 0, true, true));
        for mask in [0x40, 0x40000, 0x80000, 0x10000000, 0x02000000] {
            assert!(!path_allow_is_safe(users, mask, 0, false, true));
        }
        assert!(Path::new(r"D:\").parent().is_none());
        assert!(Path::new(r"D:\").has_root());
        assert!(Path::new(r"D:\Apps").parent().is_some());
    }
}
