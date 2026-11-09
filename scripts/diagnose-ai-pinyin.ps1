param(
    [string]$Helper,
    [string]$ReportPath
)
# Fixed public fixtures only. No registration, settings, user dictionary or document access.
# Compatible with Windows PowerShell 5.1 and PowerShell 7, in a normal 64-bit process.
$ErrorActionPreference = 'Stop'
if (-not [Environment]::Is64BitProcess) { throw 'Run this script with 64-bit PowerShell.' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this diagnostic as your normal user, without administrator elevation.'
}

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class FlowerPinyinDiagnostic {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern uint GetFinalPathNameByHandle(SafeFileHandle file, StringBuilder path, uint size, uint flags);
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool GetNamedPipeServerProcessId(SafePipeHandle pipe, out uint process);
    public static string Canonical(string path) {
        using (var file = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete)) {
            var text = new StringBuilder(32768);
            var size = GetFinalPathNameByHandle(file.SafeFileHandle, text, (uint)text.Capacity, 0);
            if (size == 0 || size >= text.Capacity) throw new Win32Exception();
            return text.ToString();
        }
    }
}
'@

function ConvertTo-ProviderPath([string]$Path) {
    # Windows PowerShell 5.1 Split-Path rejects verbatim disk paths from COM.
    # Adapt only this exact local-disk spelling; retain canonical identity for IPC.
    if ($Path -match '^\\\\\?\\[A-Za-z]:\\') { return $Path.Substring(4) }
    return $Path
}
function Resolve-DiagnosticHelper([string]$ExplicitHelper, [string]$RegisteredDll) {
    if (-not $ExplicitHelper) {
        if (-not $RegisteredDll) {
            throw 'Flower registration not found. Pass -Helper with the verified flower-settings.exe path.'
        }
        $dll = ConvertTo-ProviderPath $RegisteredDll
        $ExplicitHelper = Join-Path (Split-Path -Parent $dll) 'flower-settings.exe'
    }
    return (Get-Item -LiteralPath (ConvertTo-ProviderPath $ExplicitHelper)).FullName
}
function Get-Digest([string]$Path) {
    (Get-FileHash -LiteralPath (ConvertTo-ProviderPath $Path) -Algorithm SHA256).Hash.ToLowerInvariant()
}
function Invoke-FixedRequest($PipeName, $Request) {
    # Asynchronous pipe operations let the caller enforce one deadline and close
    # the owned stream on timeout instead of leaving an unbounded blocking read.
    $stream = New-Object IO.Pipes.NamedPipeClientStream(
        '.', $PipeName, [IO.Pipes.PipeDirection]::InOut,
        [IO.Pipes.PipeOptions]::Asynchronous,
        [Security.Principal.TokenImpersonationLevel]::Identification)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $phase = 'connect'
    [uint32]$serverId = 0
    try {
        $stream.Connect(5000)
        $phase = 'verify-server'
        if (-not [FlowerPinyinDiagnostic]::GetNamedPipeServerProcessId($stream.SafePipeHandle, [ref]$serverId)) {
            throw 'Cannot identify AI server.'
        }
        $peer = Get-Process -Id $serverId -ErrorAction Stop
        if ([FlowerPinyinDiagnostic]::Canonical($peer.Path) -cne $canonicalHelper) {
            throw 'AI server executable differs from the selected helper.'
        }
        $body = [Text.Encoding]::UTF8.GetBytes(($Request | ConvertTo-Json -Compress))
        $frame = [byte[]]([BitConverter]::GetBytes([uint32]$body.Length) + $body)
        $phase = 'write-request'
        $write = $stream.WriteAsync($frame, 0, $frame.Length)
        if (-not $write.Wait([Math]::Max(1, 5000 - [int]$clock.ElapsedMilliseconds))) {
            throw 'AI request timed out.'
        }
        [void]$write.GetAwaiter().GetResult()
        $phase = 'read-header'
        $header = New-Object byte[] 4
        $offset = 0
        while ($offset -lt 4) {
            $read = $stream.ReadAsync($header, $offset, 4 - $offset)
            if (-not $read.Wait([Math]::Max(1, 5000 - [int]$clock.ElapsedMilliseconds))) {
                throw 'AI response header timed out.'
            }
            $count = $read.GetAwaiter().GetResult()
            if ($count -eq 0) { throw 'AI pipe closed before response header.' }
            $offset += $count
        }
        $length = [BitConverter]::ToUInt32($header, 0)
        if ($length -eq 0 -or $length -gt 4096) { throw 'Invalid AI response length.' }
        $phase = 'read-body'
        $body = New-Object byte[] $length
        $offset = 0
        while ($offset -lt $length) {
            $read = $stream.ReadAsync($body, $offset, $length - $offset)
            if (-not $read.Wait([Math]::Max(1, 5000 - [int]$clock.ElapsedMilliseconds))) {
                throw 'AI response body timed out.'
            }
            $count = $read.GetAwaiter().GetResult()
            if ($count -eq 0) { throw 'AI pipe closed before response body.' }
            $offset += $count
        }
        $phase = 'decode-response'
        $response = [Text.Encoding]::UTF8.GetString($body) | ConvertFrom-Json
        if ($response.id -ne $Request.id) { throw 'AI response identity differs.' }
        return [ordered]@{
            request = $Request
            response = $response
            server_pid = $serverId
            milliseconds = [Math]::Round($clock.Elapsed.TotalMilliseconds, 2)
        }
    } catch {
        throw ("{0} (server PID {1}, {2}ms): {3}" -f $phase, $serverId,
            [Math]::Round($clock.Elapsed.TotalMilliseconds, 2), $_.Exception.Message)
    } finally {
        $stream.Dispose()
    }
}

$report = [ordered]@{
    diagnostic = 'flower-fixed-pinyin-0.4.15'
    diagnostic_revision = 3
    host_requests_captured = $false
    fixed_fixture_metadata = $true
    utc = [DateTime]::UtcNow.ToString('o')
    powershell = $PSVersionTable.PSVersion.ToString()
    os = [Environment]::OSVersion.VersionString
    processor_architecture = $env:PROCESSOR_ARCHITECTURE
    loaded_modules = @()
    requests = @()
}
try {
try {
    $report.cpu = @(Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors)
} catch { $report.cpu_error = 'CPU query unavailable' }
$registered = $null
$registry = [Microsoft.Win32.RegistryKey]::OpenBaseKey(
    [Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
try {
    $key = $registry.OpenSubKey('SOFTWARE\Classes\CLSID\{3D5B7CB1-96CB-4F44-A792-64304F6E7426}\InprocServer32')
    if ($key) {
        try { $registered = [string]$key.GetValue('') } finally { $key.Dispose() }
    }
} finally { $registry.Dispose() }
$report.registered_dll = $registered
# Enumerate binary modules only; never query window titles or document buffers.
foreach ($process in @(Get-Process -Name Notepad,Weixin,WeChat -ErrorAction SilentlyContinue)) {
    try {
        foreach ($module in $process.Modules) {
            if ($module.ModuleName -ieq 'flower.dll') {
                $report.loaded_modules += [ordered]@{
                    pid = $process.Id
                    host = $process.ProcessName
                    path = $module.FileName
                    disk_sha256 = Get-Digest $module.FileName
                    note = 'Hash of file on disk; does not prove bytes of an already loaded image.'
                }
            }
        }
    } catch { $report.loaded_modules += @{pid=$process.Id; host=$process.ProcessName; error='Module query unavailable'} }
}
$Helper = Resolve-DiagnosticHelper $Helper $registered
$canonicalHelper = [FlowerPinyinDiagnostic]::Canonical($Helper)
$report.helper = $Helper
$report.helper_sha256 = Get-Digest $Helper
$expectedHelper = 'ad011979bbfbb219436f0ba751bc657c1fbe6abe31bdf52ea35631f82cae0d41'
if ($report.helper_sha256 -ne $expectedHelper) {
    throw 'Selected helper does not match the delivered 0.4.15 binary; no executable was started.'
}
$root = Split-Path -Parent $Helper
$report.dll_sha256 = Get-Digest (Join-Path $root 'flower.dll')
$report.model_sha256 = Get-Digest (Join-Path $root 'ai/model_int8.onnx')
$report.runtime_sha256 = Get-Digest (Join-Path $root 'ai/onnxruntime.dll')
$material = [byte[]](
    [Text.Encoding]::Unicode.GetBytes($canonicalHelper + [char]0) +
    [Text.Encoding]::UTF8.GetBytes('0.4.15'))
$sha = [Security.Cryptography.SHA256]::Create()
try { $digest = ([BitConverter]::ToString($sha.ComputeHash($material))).Replace('-', '').ToLowerInvariant() }
finally { $sha.Dispose() }
$pipeName = 'Flower.AI.' + $identity.User.Value + '.' + $digest

# The helper serializes requests. Pause typing while this fixed-fixture probe runs.
# A pre-existing broker is allowed and is never stopped by this script.
$owned = $null
try {
    $probe = New-Object IO.Pipes.NamedPipeClientStream('.', $pipeName, [IO.Pipes.PipeDirection]::InOut)
    try {
        try { $probe.Connect(150); $report.broker_already_running = $true }
        catch [TimeoutException] { $report.broker_already_running = $false }
    } finally { $probe.Dispose() }
    if (-not $report.broker_already_running) {
        $owned = Start-Process -FilePath $Helper -ArgumentList '--ai-server' -WindowStyle Hidden -PassThru
        # Connection may race the bind. Retry only the connection, not a sent request.
        $ready = New-Object IO.Pipes.NamedPipeClientStream('.', $pipeName, [IO.Pipes.PipeDirection]::InOut)
        try { $ready.Connect(5000) } finally { $ready.Dispose() }
    }
    # Use code points so the script remains ASCII and works with PowerShell 5.1.
    $target = -join ([char[]]@(0x73b0,0x5728,0x5728,0x54ea,0x91cc,0x4e0a,0x73ed))
    $cases = @(
        @('replacement','xuanzaizainalishangban',@(0x9009,0x5728,0x5728,0x54ea,0x91cc,0x4e0a,0x73ed),"xuan'zai'zai'na'li'shang'ban",$target),
        @('swap','xainzaizainalishangban',@(0x559c,0x7231,0x5185,0x5728,0x5728,0x54ea,0x91cc,0x4e0a,0x73ed),"xi'ai'nei'zai'zai'na'li'shang'ban",$target),
        @('extra','xiaanzaizainalishangban',@(0x4e0b,0x5cb8,0x5728,0x5728,0x54ea,0x91cc,0x4e0a,0x73ed),"xia'an'zai'zai'na'li'shang'ban",$target),
        @('omission','xinzaizainalishangban',@(0x65b0,0x5728,0x5728,0x54ea,0x91cc,0x4e0a,0x73ed),"xin'zai'zai'na'li'shang'ban",$target),
        @('correct','xianzaizainalishangban',@(0x73b0,0x5728,0x5728,0x54ea,0x91cc,0x4e0a,0x73ed),"xian'zai'zai'na'li'shang'ban",$null),
        @('correct-xuan','womenxuanzainalijianmian',@(0x6211,0x4eec,0x9009,0x5728,0x54ea,0x91cc,0x89c1,0x9762),"wo'men'xuan'zai'na'li'jian'mian",$null),
        @('legacy-csc','',@(0x8bf7,0x95ee,0x73b0,0x5728,0x90a3,0x91cc,0x4e0a,0x73ed),'',(-join ([char[]]@(0x8bf7,0x95ee,0x73b0,0x5728,0x54ea,0x91cc,0x4e0a,0x73ed))))
    )
    $id = 0
    foreach ($case in $cases) {
        $id++
        $request = [ordered]@{id=$id; text=(-join ([char[]]$case[2])); raw=$case[1]; reading=$case[3]}
        try {
            $result = Invoke-FixedRequest $pipeName $request
            $result['kind'] = $case[0]
            $result['expected'] = $case[4]
            $expectedReading = if ($id -le 3) { "xian'zai'zai'na'li'shang'ban" } else { '' }
            $result['matches_expected'] = ($result.response.corrected -ceq $case[4]) -and
                ([string]$result.response.reading -ceq $expectedReading)
            $report.requests += $result
            Write-Host ("{0}: expected={1}, time={2}ms" -f $case[0], $result.matches_expected, $result.milliseconds)
        } catch {
            $report.requests += [ordered]@{kind=$case[0]; request=$request; error=$_.Exception.Message}
            Write-Host ("{0}: {1}" -f $case[0], $_.Exception.Message)
        }
    }
} finally {
    if ($owned) {
        try {
            $report.owned_broker_pid = $owned.Id
            if (-not $owned.HasExited) { $owned.Kill() }
            $report.owned_broker_exit_observed = $owned.WaitForExit(5000)
            if ($report.owned_broker_exit_observed) {
                $report.owned_broker_exit = $owned.ExitCode
            }
        } catch {
            $report.owned_broker_cleanup_error = $_.Exception.Message
        }
    }
}
} catch {
    $report.fatal_error = $_.Exception.Message
    Write-Host ("Diagnostic stopped: {0}" -f $_.Exception.Message)
}
if (-not $ReportPath) {
    $ReportPath = Join-Path $PSScriptRoot ('flower-ai-diagnosis-' + [Guid]::NewGuid().ToString('N') + '.json')
}
$bytes = [Text.Encoding]::UTF8.GetBytes(($report | ConvertTo-Json -Depth 10))
$file = [IO.File]::Open($ReportPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
try { $file.Write($bytes, 0, $bytes.Length) } finally { $file.Dispose() }
Write-Host ("Report: {0}" -f [IO.Path]::GetFullPath($ReportPath))
