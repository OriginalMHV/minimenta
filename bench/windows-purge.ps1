# Clears the Windows file cache, as RAMMap does: it empties the working sets
# (the system cache included) and then purges the standby list. Needs an
# administrator. Used by bench/windows.sh for cold-cache runs.
$ErrorActionPreference = "Stop"
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Purge {
    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    struct TokenPrivileges { public int Count; public long Luid; public int Attributes; }
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool LookupPrivilegeValue(string system, string name, out long luid);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll, ref TokenPrivileges state, int length, IntPtr previous, IntPtr returnLength);
    [DllImport("kernel32.dll")]
    static extern IntPtr GetCurrentProcess();
    [DllImport("ntdll.dll")]
    static extern uint NtSetSystemInformation(int infoClass, ref int info, int length);
    const int SystemMemoryListInformation = 80;
    const int MemoryEmptyWorkingSets = 2;
    const int MemoryPurgeStandbyList = 4;
    public static void Run() {
        IntPtr token;
        if (!OpenProcessToken(GetCurrentProcess(), 0x0028, out token)) throw new Exception("OpenProcessToken failed");
        long luid;
        if (!LookupPrivilegeValue(null, "SeProfileSingleProcessPrivilege", out luid)) throw new Exception("LookupPrivilegeValue failed");
        var state = new TokenPrivileges { Count = 1, Luid = luid, Attributes = 2 };
        if (!AdjustTokenPrivileges(token, false, ref state, 0, IntPtr.Zero, IntPtr.Zero)) throw new Exception("AdjustTokenPrivileges failed");
        foreach (var command in new[] { MemoryEmptyWorkingSets, MemoryPurgeStandbyList }) {
            int value = command;
            uint status = NtSetSystemInformation(SystemMemoryListInformation, ref value, 4);
            if (status != 0) throw new Exception("NtSetSystemInformation(" + command + ") failed: 0x" + status.ToString("X"));
        }
    }
}
"@
[Purge]::Run()
