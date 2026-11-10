#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::ExitCode;

use flower::user_dictionary::DictionaryStore;

#[path = "../startup.rs"]
mod startup;

#[cfg(target_os = "windows")]
#[path = "../ai/inference.rs"]
mod ai_inference;

const USAGE: &str = "Flower 设置程序

用法：
  flower-settings
  flower-settings --help
  flower-settings --start-input-method
  flower-settings --startup-input-method
  flower-settings --enable-startup
  flower-settings --disable-startup
  flower-settings ai probe <bundle-directory>
  flower-settings dictionary status
  flower-settings dictionary import <path> [--domain <name>]
  flower-settings dictionary export <path> [--domain <name>]
  flower-settings dictionary remove <canonical-pinyin> <text> [--domain <name>]
  flower-settings dictionary domain <name> on|off
  flower-settings dictionary learning on|off
  flower-settings dictionary clear-learning
  flower-settings unicode U+<hex>

无参数打开设置窗口。词典管理仅操作当前用户本地数据，不提权。
导入/导出使用三列 TSV（拼音、文字、正整数权重）；读音以单引号分音。
选词顺序默认本地保存；成功选中前移一位，完整逐段组词可记忆。
关闭停止新增；清空删除自动词和排序，保留手工个人词条及专业词库。
修改后请重新切换到 Flower 输入法使设置生效。";

#[derive(Debug, PartialEq)]
enum Command {
    Gui,
    StartInputMethod,
    StartupInputMethod,
    EnableStartup,
    DisableStartup,
    AiServer,
    AiProbe(PathBuf),
    Help,
    Unicode(char),
    Status,
    Import(PathBuf, Option<String>),
    Export(PathBuf, Option<String>),
    Remove(String, String, Option<String>),
    Domain(String, bool),
    Learning(bool),
    ClearLearning,
}

fn text(value: &OsStr) -> Result<String, String> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "命令、词语和域名必须是有效 Unicode。".to_owned())
}

fn switch(value: &OsStr) -> Result<bool, String> {
    match value.to_str() {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err("开关参数只能是 on 或 off。".to_owned()),
    }
}

fn domain(args: &[OsString]) -> Result<Option<String>, String> {
    match args {
        [] => Ok(None),
        [flag, name] if flag == "--domain" => {
            let name = text(name)?;
            if name.is_empty() {
                return Err("域名不能为空。".to_owned());
            }
            Ok(Some(name))
        }
        _ => Err("可选参数必须为 --domain <name>。".to_owned()),
    }
}

fn unicode_scalar(value: &str) -> Result<char, String> {
    let invalid = || "码点必须为 U+<十六进制>，且为非控制字符的 Unicode 标量。".to_owned();
    let hex = value.strip_prefix("U+").ok_or_else(invalid)?;
    if hex.is_empty() || hex.len() > 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let scalar = u32::from_str_radix(hex, 16)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(invalid)?;
    if scalar.is_control() {
        return Err(invalid());
    }
    Ok(scalar)
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Gui),
        [arg] if arg == "--start-input-method" => Ok(Command::StartInputMethod),
        [arg] if arg == "--startup-input-method" => Ok(Command::StartupInputMethod),
        [arg] if arg == "--enable-startup" => Ok(Command::EnableStartup),
        [arg] if arg == "--disable-startup" => Ok(Command::DisableStartup),
        [arg] if arg == "--ai-server" => Ok(Command::AiServer),
        [cmd, op, path] if cmd == "ai" && op == "probe" && !path.is_empty() => {
            Ok(Command::AiProbe(PathBuf::from(path)))
        }
        [arg] if arg == "--help" || arg == "-h" => Ok(Command::Help),
        [cmd, value] if cmd == "unicode" => Ok(Command::Unicode(unicode_scalar(&text(value)?)?)),
        [cmd, rest @ ..] if cmd == "dictionary" => match rest {
            [op] if op == "status" => Ok(Command::Status),
            [op] if op == "clear-learning" => Ok(Command::ClearLearning),
            [op, value] if op == "learning" => Ok(Command::Learning(switch(value)?)),
            [op, name, value] if op == "domain" => {
                let name = text(name)?;
                if name.is_empty() {
                    return Err("域名不能为空。".to_owned());
                }
                Ok(Command::Domain(name, switch(value)?))
            }
            [op, path, tail @ ..] if op == "import" || op == "export" => {
                if path.is_empty() {
                    return Err("文件路径不能为空。".to_owned());
                }
                let name = domain(tail)?;
                if op == "import" {
                    Ok(Command::Import(PathBuf::from(path), name))
                } else {
                    Ok(Command::Export(PathBuf::from(path), name))
                }
            }
            [op, pinyin, word, tail @ ..] if op == "remove" => {
                Ok(Command::Remove(text(pinyin)?, text(word)?, domain(tail)?))
            }
            _ => Err("词典子命令或参数无效。".to_owned()),
        },
        _ => Err("不支持的命令或参数。".to_owned()),
    }
}

#[cfg(target_os = "windows")]
fn require_non_elevated() -> Result<(), String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
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
        if returned != std::mem::size_of::<TOKEN_ELEVATION>() as u32
            || elevation.TokenIsElevated != 0
        {
            return Err("启动 Flower 拒绝提升令牌；请由原始普通用户运行。".into());
        }
    }
    Ok(())
}

fn start_input_method() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        require_non_elevated()?;
        flower::platform::registration::enable().map_err(|e| e.to_string())?;
        println!("已为当前用户启用 Flower，请按 Win+Space 手动切换；尚未切换当前焦点的输入法。");
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    Err("启动 Flower 输入法仅支持 Windows 10/11 x64。".to_owned())
}

fn execute(command: Command) -> Result<(), String> {
    match command {
        Command::StartupInputMethod | Command::EnableStartup | Command::DisableStartup => {
            #[cfg(windows)]
            {
                require_non_elevated()?;
                let path =
                    flower::platform::registration::validate_current_settings_installation()?;
                return match command {
                    Command::StartupInputMethod => flower::platform::registration::enable()
                        .map(|_| ())
                        .map_err(|e| e.to_string()),
                    Command::EnableStartup => startup::set(&path, true),
                    Command::DisableStartup => startup::set(&path, false),
                    _ => unreachable!(),
                };
            }
            #[cfg(not(windows))]
            return Err("自启动仅支持 Windows。".into());
        }
        Command::AiServer => {
            #[cfg(target_os = "windows")]
            {
                require_non_elevated()?;
                return ai_inference::run_server();
            }
            #[cfg(not(target_os = "windows"))]
            return Err("离线 AI 仅支持 Windows x64。".to_owned());
        }
        Command::AiProbe(bundle) => {
            #[cfg(target_os = "windows")]
            {
                use std::io::Read;
                require_non_elevated()?;
                let mut input = String::new();
                std::io::stdin()
                    .take(4097)
                    .read_to_string(&mut input)
                    .map_err(|_| "无法读取测试输入。")?;
                if input.len() > 4096 {
                    return Err("测试输入过长。".into());
                }
                let corrected = ai_inference::probe(&bundle, input.trim_end_matches(['\r', '\n']))?;
                println!(
                    "{}",
                    serde_json::to_string(&flower::ai::Response {
                        id: 0,
                        corrected,
                        reading: String::new()
                    })
                    .map_err(|_| "无法序列化测试结果。")?
                );
                return Ok(());
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = bundle;
                return Err("离线 AI 仅支持 Windows x64。".to_owned());
            }
        }
        Command::StartInputMethod => return start_input_method(),
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Command::Unicode(scalar) => {
            print!("{scalar}");
            return Ok(());
        }
        Command::Gui => {
            #[cfg(target_os = "windows")]
            return flower::platform::settings_ui::run_settings_app();
            #[cfg(not(target_os = "windows"))]
            return Err("Flower 设置窗口目前仅支持 Windows 10/11 x64。".to_owned());
        }
        _ => {}
    }
    let store = DictionaryStore::for_current_user().map_err(|e| e.to_string())?;
    let result = match command {
        Command::Status => {
            let data = store.load().map_err(|e| e.to_string())?;
            println!(
                "格式版本：{}\n代次：{}\n记住选词顺序：{}\n个人词条：{}\n排序作用域：{}\n自动记忆词语：{}",
                data.schema_version,
                data.generation,
                if data.adaptive_ranking_enabled {
                    "开启"
                } else {
                    "关闭"
                },
                data.entries.len(),
                data.rankings.len(),
                data.auto_phrases.len()
            );
            for item in data.domains {
                println!(
                    "专业词库：{}（{}，{} 条）",
                    item.name,
                    if item.enabled { "启用" } else { "停用" },
                    item.entries.len()
                );
            }
            return Ok(());
        }
        Command::Import(path, name) => store.import_tsv(&path, name.as_deref()),
        Command::Export(path, name) => store.export_tsv(&path, name.as_deref()),
        Command::Remove(pinyin, word, name) => store.remove_entry(&pinyin, &word, name.as_deref()),
        Command::Domain(name, enabled) => store.set_domain_enabled(&name, enabled),
        Command::Learning(enabled) => store.set_learning_enabled(enabled),
        Command::ClearLearning => store.clear_learning(),
        _ => unreachable!(),
    };
    result.map_err(|e| e.to_string())?;
    println!("操作完成。修改后请重新切换到 Flower 输入法使设置生效。");
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
fn wants_cli_console(command: &Result<Command, String>, silent: bool) -> bool {
    !silent
        && !matches!(
            command,
            Ok(Command::Gui
                | Command::AiServer
                | Command::StartInputMethod
                | Command::StartupInputMethod
                | Command::EnableStartup
                | Command::DisableStartup)
        )
}

#[cfg(target_os = "windows")]
fn prepare_command_console(command: &Result<Command, String>, silent: bool) {
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    if !wants_cli_console(command, silent) {
        return;
    }
    // Keep explicitly inherited pipes/files (including CLI tests and AI probes)
    // intact. A GUI-subsystem executable must not allocate a console just to
    // print help; only an existing parent console may be attached.
    let has_output = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .any(|kind| unsafe { GetStdHandle(kind) }.is_ok_and(|handle| !handle.is_invalid()));
    if !has_output {
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let command = parse(&args);
    // Even malformed login invocations must never show UI or print user paths.
    let silent = args.iter().any(|arg| arg == "--startup-input-method");
    #[cfg(target_os = "windows")]
    prepare_command_console(&command, silent);
    let only_start = matches!(
        &command,
        Ok(Command::StartInputMethod | Command::EnableStartup | Command::DisableStartup)
    );
    match command.and_then(execute) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if silent {
                return ExitCode::FAILURE;
            }
            eprintln!("Flower 设置程序失败：{error}\n\n{USAGE}");
            if only_start {
                #[cfg(target_os = "windows")]
                unsafe {
                    use windows::core::w;
                    use windows::Win32::UI::WindowsAndMessaging::{
                        MessageBoxW, MB_ICONERROR, MB_OK,
                    };
                    MessageBoxW(
                        None,
                        w!("Flower 操作失败。请以普通用户身份运行，确认本次安装完整且没有其他安装的自启动项；未更改默认输入法。"),
                        w!("Flower 启动失败"),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_and_background_modes_never_request_a_console() {
        for mode in [
            Command::Gui,
            Command::AiServer,
            Command::StartInputMethod,
            Command::StartupInputMethod,
            Command::EnableStartup,
            Command::DisableStartup,
        ] {
            assert!(!wants_cli_console(&Ok(mode), false));
        }
        assert!(wants_cli_console(&Ok(Command::Help), false));
        assert!(wants_cli_console(&Err("invalid command".into()), false));
        assert!(!wants_cli_console(&Err("invalid startup".into()), true));
    }
    fn command(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }
    #[test]
    fn strict_arguments() {
        for (flag, expected) in [
            ("--startup-input-method", Command::StartupInputMethod),
            ("--enable-startup", Command::EnableStartup),
            ("--disable-startup", Command::DisableStartup),
        ] {
            assert_eq!(command(&[flag]).unwrap(), expected);
            assert!(command(&[flag, "extra"]).is_err());
            assert!(command(&[flag, "--help"]).is_err());
            assert!(command(&[flag, flag]).is_err());
        }
        assert_eq!(command(&[]).unwrap(), Command::Gui);
        assert_eq!(
            command(&["--start-input-method"]).unwrap(),
            Command::StartInputMethod
        );
        assert!(command(&["--start-input-method", "extra"]).is_err());
        assert!(command(&["--start-input-method", "--help"]).is_err());
        assert_eq!(command(&["--ai-server"]).unwrap(), Command::AiServer);
        assert!(command(&["--ai-server", "extra"]).is_err());
        assert_eq!(
            command(&["ai", "probe", "fixed-bundle"]).unwrap(),
            Command::AiProbe(PathBuf::from("fixed-bundle"))
        );
        assert!(command(&["ai", "probe"]).is_err());
        assert!(command(&["dictionary", "status", "--help"]).is_err());
        assert!(command(&["--help", "extra"]).is_err());
        assert!(command(&["dictionary", "learning", "yes"]).is_err());
        assert!(command(&["dictionary", "import", "a.tsv", "--domain"]).is_err());
        assert_eq!(
            command(&["dictionary", "remove", "ni'hao", "你好", "--domain", "医学"]).unwrap(),
            Command::Remove("ni'hao".into(), "你好".into(), Some("医学".into()))
        );
    }
    #[test]
    fn unicode_validation() {
        assert_eq!(unicode_scalar("U+20000").unwrap(), '\u{20000}');
        for value in [
            "U+D800", "U+110000", "U+0000", "U+007F", "U+009F", "U+", "U+-1", "20000",
        ] {
            assert!(unicode_scalar(value).is_err(), "{value}");
        }
    }
    #[cfg(windows)]
    #[test]
    fn non_unicode_paths_are_preserved() {
        use std::os::windows::ffi::OsStringExt;
        let path = OsString::from_wide(&[0xd800, 0x2e, 0x74]);
        assert_eq!(
            parse(&["dictionary".into(), "import".into(), path.clone()]).unwrap(),
            Command::Import(PathBuf::from(path.clone()), None)
        );
        assert!(parse(&["dictionary".into(), "domain".into(), path, "on".into()]).is_err());
    }
}
