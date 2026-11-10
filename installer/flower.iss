#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef BuildId
  #error BuildId is required
#endif
#ifndef FileVersion
  #error FileVersion is required
#endif
#ifndef PackageDir
  #error PackageDir is required
#endif
#ifndef FilesInclude
  #error FilesInclude is required
#endif
#ifndef OutputDir
  #error OutputDir is required
#endif
#ifndef ExpectedDllHash
  #error ExpectedDllHash is required
#endif
#ifndef ExpectedToolHash
  #error ExpectedToolHash is required
#endif
#ifndef ExpectedSettingsHash
  #error ExpectedSettingsHash is required
#endif
#ifndef EngineLicense
  #error EngineLicense is required
#endif

[Setup]
AppId={{1B8DAF2E-D5F7-49AC-A9BF-4713A0D57224}
AppName=Flower
AppVersion={#AppVersion}
VersionInfoVersion={#FileVersion}
VersionInfoTextVersion={#AppVersion}
VersionInfoProductName=Flower
VersionInfoDescription=Flower 测试版安装器
DefaultDirName={commonpf64}\Flower
DefaultGroupName=Flower
DisableDirPage=yes
UninstallFilesDir={app}
AllowUNCPath=no
DisableProgramGroupPage=yes
UsePreviousAppDir=no
PrivilegesRequired=admin
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=flower-{#AppVersion}-windows-x64-setup
SetupIconFile=..\assets\flower.ico
UninstallDisplayIcon={app}\versions\{#BuildId}\flower-settings.exe
LicenseFile={#PackageDir}\LICENSE-MIT
InfoBeforeFile=notice.zh-CN.txt
Compression=lzma2
LZMADictionarySize=32768
SolidCompression=yes
WizardStyle=modern
DisableWelcomePage=no
CloseApplications=no
RestartApplications=no
AllowCancelDuringInstall=no
RestartIfNeededByRun=no
SetupLogging=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "zh"; MessagesFile: "compiler:Default.isl,zh-CN.isl"

[Files]
Source: "{#PackageDir}\flower-register.exe"; DestName: "flower-path-check.exe"; Flags: dontcopy
#include FilesInclude
Source: "notice.zh-CN.txt"; DestDir: "{app}\versions\{#BuildId}"; DestName: "INSTALLER-README.txt"; Flags: ignoreversion
Source: "{#EngineLicense}"; DestDir: "{app}\versions\{#BuildId}\licenses\installer"; DestName: "INNO-SETUP.txt"; Flags: ignoreversion

[Icons]
Name: "{commonprograms}\Flower\启动 Flower"; Filename: "{app}\versions\{#BuildId}\flower-register.exe"; Parameters: "enable"; IconFilename: "{app}\versions\{#BuildId}\flower-settings.exe"
Name: "{commonprograms}\Flower\停用 Flower"; Filename: "{app}\versions\{#BuildId}\flower-register.exe"; Parameters: "disable"; IconFilename: "{app}\versions\{#BuildId}\flower-settings.exe"
Name: "{commonprograms}\Flower\Flower 设置"; Filename: "{app}\versions\{#BuildId}\flower-settings.exe"
Name: "{commonprograms}\Flower\关闭开机自启动"; Filename: "{app}\versions\{#BuildId}\flower-settings.exe"; Parameters: "--disable-startup"
Name: "{commonprograms}\Flower\Flower 安装说明"; Filename: "{app}\versions\{#BuildId}\INSTALLER-README.txt"

[Run]
Filename: "{app}\versions\{#BuildId}\flower-settings.exe"; Parameters: "--start-input-method"; Description: "启动 Flower"; Flags: postinstall nowait skipifsilent runasoriginaluser; Check: CanStartFlower
Filename: "{app}\versions\{#BuildId}\flower-settings.exe"; Parameters: "--enable-startup"; Description: "开机自启动"; Flags: postinstall unchecked skipifsilent runasoriginaluser; Check: CanStartFlower

[Code]
const
  FlowerClass = '{3D5B7CB1-96CB-4F44-A792-64304F6E7426}';
  ClassKey = 'Software\Classes\CLSID\{3D5B7CB1-96CB-4F44-A792-64304F6E7426}';
  TipKey = 'Software\Microsoft\CTF\TIP\{3D5B7CB1-96CB-4F44-A792-64304F6E7426}';
  KeyboardCategory = '{34745C63-B2F0-4784-8B67-5E12C8701A31}';
  UIElementCategory = '{49D2F9CF-1F5E-11D7-A6D3-00065B84435C}';
  InstallerKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{1B8DAF2E-D5F7-49AC-A9BF-4713A0D57224}_is1';
var
  RegistrationFailed: Boolean;
  PathCheckerReady: Boolean;

function CreateFileW(Name: String; Access, Share: LongWord; Security: LongWord;
  Creation, Attributes: LongWord; Template: THandle): THandle;
  external 'CreateFileW@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function GetFinalPathNameByHandleW(Handle: THandle; Buffer: String;
  BufferLength, Flags: LongWord): LongWord;
  external 'GetFinalPathNameByHandleW@kernel32.dll stdcall';
function CompareStringOrdinal(Left: String; LeftLength: Integer;
  Right: String; RightLength: Integer; IgnoreCase: Boolean): Integer;
  external 'CompareStringOrdinal@kernel32.dll stdcall';

function PayloadDir: String;
begin
  Result := ExpandConstant('{app}\versions\{#BuildId}');
end;

function PathChecker: String;
begin
  Result := ExpandConstant('{tmp}\flower-path-check.exe');
end;

function SafeInstallPath(Name: String): Boolean;
var
  ExitCode: Integer;
begin
  Result := False;
  if not PathCheckerReady then Exit;
  if CompareText(GetSHA256OfFile(PathChecker), '{#ExpectedToolHash}') <> 0 then Exit;
  if Exec(PathChecker, 'check-install-path --path "' + Name + '"',
    ExpandConstant('{tmp}'), SW_HIDE, ewWaitUntilTerminated, ExitCode) then
    Result := ExitCode = 0;
end;

function FixedInstallRoot: Boolean;
begin
  { ExpandFileName normalizes dot segments before the ordinal comparison.
    SafeInstallPath separately rejects links, unsafe ACLs and nonlocal paths. }
  Result := CompareStringOrdinal(
    RemoveBackslashUnlessRoot(ExpandFileName(ExpandConstant('{app}'))), -1,
    RemoveBackslashUnlessRoot(ExpandFileName(ExpandConstant('{commonpf64}\Flower'))),
    -1, True) = 2;
end;

function PrepareInstallPath(Name: String): Boolean;
var
  ExitCode: Integer;
begin
  Result := False;
  if not PathCheckerReady then Exit;
  if CompareText(GetSHA256OfFile(PathChecker), '{#ExpectedToolHash}') <> 0 then Exit;
  if Exec(PathChecker, 'prepare-install-path --path "' + Name + '"',
    ExpandConstant('{tmp}'), SW_HIDE, ewWaitUntilTerminated, ExitCode) then
    Result := ExitCode = 0;
end;

function HasSubkeys(Key: String): Boolean;
var
  Names: TArrayOfString;
begin
  Result := False;
  if not RegKeyExists(HKLM64, Key) then Exit;
  if not RegGetSubkeyNames(HKLM64, Key, Names) then begin
    Result := True;
    Exit;
  end;
  Result := GetArrayLength(Names) > 0;
end;

function HasFlowerRegistration: Boolean;
var
  Languages: TArrayOfString;
  I: Integer;
begin
  Result := True;
  if RegKeyExists(HKLM64, ClassKey) then Exit;
  if HasSubkeys(TipKey + '\Category\Item\' + FlowerClass) then Exit;
  if RegKeyExists(HKLM64, TipKey + '\Category\Category\' +
    KeyboardCategory + '\' + FlowerClass) then Exit;
  if RegKeyExists(HKLM64, TipKey + '\Category\Category\' +
    UIElementCategory + '\' + FlowerClass) then Exit;
  if RegKeyExists(HKLM64, TipKey + '\LanguageProfile') then begin
    if not RegGetSubkeyNames(HKLM64, TipKey + '\LanguageProfile', Languages) then Exit;
    for I := 0 to GetArrayLength(Languages) - 1 do
      if HasSubkeys(TipKey + '\LanguageProfile\' + Languages[I]) then Exit;
  end;
  Result := False;
end;

function CanonicalFile(Name: String): String;
var
  Handle: THandle;
  Buffer: String;
  Count: LongWord;
begin
  Result := '';
  Handle := CreateFileW(Name, $80000000, 7, 0, 3, 0, 0);
  if Handle = THandle(-1) then Exit;
  try
    SetLength(Buffer, 32768);
    Count := GetFinalPathNameByHandleW(Handle, Buffer, Length(Buffer), 0);
    if (Count > 0) and (Count < LongWord(Length(Buffer))) then begin
      SetLength(Buffer, Count);
      Result := Buffer;
    end;
  finally
    CloseHandle(Handle);
  end;
end;

function DllNotInUse: Boolean;
var
  Name: String;
  Handle: THandle;
begin
  Name := PayloadDir + '\flower.dll';
  Result := True;
  if not FileExists(Name) then Exit;
  { Request write access exclusively, but never write or truncate the file. }
  Handle := CreateFileW(Name, $C0000000, 0, 0, 3, 0, 0);
  Result := Handle <> THandle(-1);
  if Result then CloseHandle(Handle);
end;

function TrustedPayload: Boolean;
begin
  Result := False;
  if not FileExists(PayloadDir + '\flower.dll') then Exit;
  if not FileExists(PayloadDir + '\flower-register.exe') then Exit;
  Result :=
    (CompareText(GetSHA256OfFile(PayloadDir + '\flower.dll'), '{#ExpectedDllHash}') = 0) and
    (CompareText(GetSHA256OfFile(PayloadDir + '\flower-register.exe'), '{#ExpectedToolHash}') = 0);
end;

function OwnedRegistrationComplete: Boolean;
var
  Registered, Expected, Description: String;
begin
  Result := False;
  if not RegQueryStringValue(HKLM64, ClassKey + '\InprocServer32', '', Registered) then Exit;
  Expected := CanonicalFile(PayloadDir + '\flower.dll');
  if (Expected = '') or (CompareStringOrdinal(Registered, -1, Expected, -1, True) <> 2) then Exit;
  if not RegQueryStringValue(HKLM64, TipKey +
    '\LanguageProfile\0x00000804\{6C7782F4-5F59-42CF-8C29-F9A8D46F1B72}',
    'Description', Description) then Exit;
  Result := (Description = 'Flower') and RegKeyExists(HKLM64,
    TipKey + '\Category\Category\' + KeyboardCategory + '\' + FlowerClass) and
    RegKeyExists(HKLM64, TipKey + '\Category\Category\' + UIElementCategory + '\' + FlowerClass);
end;

function CanStartFlower: Boolean;
begin
  Result := False;
  if RegistrationFailed then Exit;
  if not FixedInstallRoot then Exit;
  if not SafeInstallPath(ExpandConstant('{app}')) then Exit;
  if not TrustedPayload then Exit;
  if not OwnedRegistrationComplete then Exit;
  if not FileExists(PayloadDir + '\flower-settings.exe') then Exit;
  Result := CompareText(GetSHA256OfFile(PayloadDir + '\flower-settings.exe'),
    '{#ExpectedSettingsHash}') = 0;
end;

function InitializeSetup: Boolean;
begin
  RegistrationFailed := True;
  Result := not WizardSilent;
  if not Result then
    SuppressibleMsgBox('此测试版安装器仅支持交互安装。', mbError, MB_OK, IDOK);
end;

function HasNativeRuntime: Boolean;
var
  Previous: Boolean;
begin
  Previous := EnableFsRedirection(False);
  try
    Result := FileExists(ExpandConstant('{sys}\vcruntime140.dll'));
  finally
    EnableFsRedirection(Previous);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not FixedInstallRoot then begin
    Result := 'Flower 固定安装到 Program Files\Flower，不支持 /DIR 更改安装目录。';
    Exit;
  end;
  if not PathCheckerReady then begin
    ExtractTemporaryFile('flower-path-check.exe');
    PathCheckerReady := True;
  end;
  if HasFlowerRegistration or RegKeyExists(HKLM64, InstallerKey) then
    Result := '已发现 Flower 注册或安装记录。请先由各用户停用 Flower、退出旧宿主，再使用旧安装工具注销或卸载；本安装器不会覆盖旧安装。'
  else if DirExists(PayloadDir) then
    Result := '目标版本目录已存在。为保护未知或未完成的安装，拒绝覆盖。'
  else if not HasNativeRuntime then
    Result := '请先安装 Microsoft Visual C++ v14 x64 运行库。本离线安装器不会下载它。'
  else if not PrepareInstallPath(WizardDirValue) then
    Result := '安全准备目录失败：祖先或已有目标不安全，或创建时发生竞争。已有权限未更改；本次创建的空安全目录可能保留。'
  else if not SafeInstallPath(ExpandConstant('{app}')) then
    Result := '固定安装目录安全检查失败。已有父目录权限和所有者不会更改。';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ExitCode: Integer;
begin
  if CurStep <> ssPostInstall then Exit;
  RegistrationFailed := True;
  if SafeInstallPath(ExpandConstant('{app}')) and TrustedPayload then begin
    if Exec(PayloadDir + '\flower-register.exe',
      'register --dll "' + PayloadDir + '\flower.dll"', PayloadDir,
      SW_HIDE, ewWaitUntilTerminated, ExitCode) then begin
      Log('Flower register exit code: ' + IntToStr(ExitCode));
      RegistrationFailed := (ExitCode <> 0) or (not OwnedRegistrationComplete);
    end;
  end;
  if RegistrationFailed then begin
    Log('Flower registration failed; keeping installed files for repair.');
    WizardForm.FinishedLabel.Caption :=
      'Flower 注册失败。文件和卸载入口已保留，不能将本次安装视为成功。请查看安装日志并修复或卸载后重试。';
    SuppressibleMsgBox(WizardForm.FinishedLabel.Caption, mbError, MB_OK, IDOK);
  end else
    WizardForm.FinishedLabel.Caption :=
      'Flower 机器级注册已完成。“启动 Flower”仅为原始普通用户启用后退出；“开机自启动”独立设置该用户登录时静默启用，不打开设置或更改默认输入法。请按 Win+Space 手动选择 Flower。本测试版尚未通过实际输入验收。';
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and RegistrationFailed then
    WizardForm.FinishedLabel.Caption :=
      'Flower 注册失败。文件和卸载入口已保留，不能将本次安装视为成功。请查看安装日志并修复或卸载后重试。';
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if RegistrationFailed then Result := 15;
end;

function InitializeUninstall: Boolean;
var
  Registered, Expected: String;
  ExitCode: Integer;
begin
  Result := False;
  if not FixedInstallRoot then begin
    MsgBox('卸载目录不是固定 Program Files\Flower，已停止卸载并保留文件。', mbError, MB_OK);
    Exit;
  end;
  if UninstallSilent then begin
    SuppressibleMsgBox('此测试版仅支持交互卸载。', mbError, MB_OK, IDOK);
    Exit;
  end;
  if SuppressibleMsgBox(
    '请确认每个用户已先运行“关闭开机自启动”再“停用 Flower”，并保存工作、退出所有使用过 Flower 的应用。卸载不会替任何账户清理自启动或停用；未关闭的项可能成为失效引用。用户词典和设置均保留。是否继续？',
    mbConfirmation, MB_YESNO, IDNO) <> IDYES then Exit;
  { Never execute a helper directly out of a tree whose ACL may have changed.
    Copy to Inno's private temporary directory, verify the copy, then inspect. }
  if not FileCopy(PayloadDir + '\flower-register.exe', PathChecker, False) then begin
    MsgBox('无法准备只读目录检查工具，已停止卸载并保留文件。', mbError, MB_OK);
    Exit;
  end;
  PathCheckerReady := True;
  if not SafeInstallPath(ExpandConstant('{app}')) then begin
    MsgBox('安装目录权限已改变或文件校验失败，已停止卸载并保留文件。请先由管理员检查目录安全性。', mbError, MB_OK);
    Exit;
  end;
  if not DllNotInUse then begin
    MsgBox('Flower DLL 仍被占用或不可写。请退出宿主或注销登录后重试；未开始清理文件。', mbError, MB_OK);
    Exit;
  end;
  if not HasFlowerRegistration then begin
    Result := True;
    Exit;
  end;
  if not RegQueryStringValue(HKLM64, ClassKey + '\InprocServer32', '', Registered) then begin
    MsgBox('Flower 注册残缺，无法确认路径所有权。已停止卸载并保留文件。', mbError, MB_OK);
    Exit;
  end;
  Expected := CanonicalFile(PayloadDir + '\flower.dll');
  if (Expected = '') or
     (CompareStringOrdinal(Registered, -1, Expected, -1, True) <> 2) or
     (not TrustedPayload) then begin
    MsgBox('当前 Flower 注册不属于本次文件，或文件校验失败。已停止卸载，未触及其他安装。', mbError, MB_OK);
    Exit;
  end;
  if not Exec(PayloadDir + '\flower-register.exe', 'unregister', PayloadDir,
    SW_HIDE, ewWaitUntilTerminated, ExitCode) then begin
    MsgBox('无法启动 Flower 注册清理工具，已停止卸载并保留文件。', mbError, MB_OK);
    Exit;
  end;
  if (ExitCode <> 0) or HasFlowerRegistration or (not DllNotInUse) then begin
    MsgBox('Flower 注册清理或占用复查失败。已停止文件清理，保留 DLL 和工具供修复后重试。', mbError, MB_OK);
    Exit;
  end;
  Result := True;
end;
