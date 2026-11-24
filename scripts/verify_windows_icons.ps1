param([Parameter(Mandatory = $true)][string]$BinaryRoot)
$ErrorActionPreference = 'Stop'

# Load as resources only: this must never execute DLL initialization or exports.
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
public static class FlowerIconVerifier {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr LoadLibraryExW(string file, IntPtr reserved, uint flags);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern IntPtr FindResourceW(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern uint SizeofResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll")] static extern IntPtr LockResource(IntPtr resource);
    [DllImport("kernel32.dll")] static extern bool FreeLibrary(IntPtr module);
    static byte[] Resource(IntPtr module, int name, int type) {
        var resource = FindResourceW(module, new IntPtr(name), new IntPtr(type));
        if (resource == IntPtr.Zero) throw new Exception("Missing icon resource: " + Marshal.GetLastWin32Error());
        uint size = SizeofResource(module, resource);
        var data = LockResource(LoadResource(module, resource));
        if (data == IntPtr.Zero || size == 0 || size > int.MaxValue) throw new Exception("Invalid icon resource");
        var bytes = new byte[(int)size];
        Marshal.Copy(data, bytes, 0, bytes.Length);
        return bytes;
    }
    public static void Verify(string binary, string icon) {
        byte[] expected = File.ReadAllBytes(icon);
        int count = BitConverter.ToUInt16(expected, 4);
        if (count != 9 || BitConverter.ToUInt16(expected, 2) != 1) throw new Exception("Invalid source ICO");
        var module = LoadLibraryExW(binary, IntPtr.Zero, 0x02 | 0x20);
        if (module == IntPtr.Zero) throw new Exception("Resource-only load failed: " + Marshal.GetLastWin32Error());
        try {
            byte[] group = Resource(module, 101, 14);
            if (group.Length != 6 + count * 14 || BitConverter.ToUInt16(group, 4) != count)
                throw new Exception("Wrong icon group");
            for (int i = 0; i < count; i++) {
                int source = 6 + i * 16, entry = 6 + i * 14;
                int size = checked((int)BitConverter.ToUInt32(expected, source + 8));
                int offset = checked((int)BitConverter.ToUInt32(expected, source + 12));
                if (group[entry] != expected[source] || group[entry+1] != expected[source+1])
                    throw new Exception("Icon dimensions differ");
                byte[] actual = Resource(module, BitConverter.ToUInt16(group, entry+12), 3);
                // Windows PowerShell 5.1 uses .NET Framework, without Span<T>.
                if (actual.Length != size || offset < 0 || size < 0 || offset > expected.Length - size)
                    throw new Exception("Embedded image differs from source ICO");
                for (int j = 0; j < size; j++) {
                    if (actual[j] != expected[offset + j])
                        throw new Exception("Embedded image differs from source ICO");
                }
            }
            Console.WriteLine(Path.GetFileName(binary) + ": 9 embedded icon images match source ICO");
        } finally { FreeLibrary(module); }
    }
}
'@
$icon = (Resolve-Path (Join-Path $PSScriptRoot '../assets/flower.ico')).Path
foreach ($name in @('flower.dll', 'flower-register.exe', 'flower-settings.exe')) {
    $binary = (Resolve-Path -LiteralPath (Join-Path $BinaryRoot $name)).Path
    [FlowerIconVerifier]::Verify($binary, $icon)
}
