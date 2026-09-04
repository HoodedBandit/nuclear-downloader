// Test infrastructure only: run exact release bytes with a normal-user token.
// No credential, account, UAC policy, or installed application is changed.
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;

public static class WindowsUserProcess
{
    [StructLayout(LayoutKind.Sequential)]
    private struct SidAndAttributes { public IntPtr Sid; public uint Attributes; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct StartupInfo
    {
        public int Size;
        public string Reserved, Desktop, Title;
        public uint X, Y, Width, Height, XChars, YChars, FillAttribute, Flags;
        public ushort ShowWindow, ReservedSize;
        public IntPtr ReservedPointer, Stdin, Stdout, Stderr;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct ProcessInfo { public IntPtr Process, Thread; public uint ProcessId, ThreadId; }
    [StructLayout(LayoutKind.Sequential)]
    private struct JobLimits
    {
        public long ProcessTime, JobTime;
        public uint Flags;
        public UIntPtr MinimumWorkingSet, MaximumWorkingSet;
        public uint ActiveProcessLimit;
        public UIntPtr Affinity;
        public uint Priority, SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct IoCounters { public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes; }
    [StructLayout(LayoutKind.Sequential)]
    private struct ExtendedJobLimits
    {
        public JobLimits Basic;
        public IoCounters Io;
        public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
    }

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool CreateRestrictedToken(IntPtr token, uint flags, uint disableCount, IntPtr disable,
        uint deleteCount, IntPtr delete, uint restrictCount, IntPtr restrict, out IntPtr restricted);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool ConvertStringSidToSid(string value, out IntPtr sid);
    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool SetTokenInformation(IntPtr token, int kind, ref SidAndAttributes value, uint size);
    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool SetTokenInformation(IntPtr token, int kind, ref IntPtr value, uint size);
    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern bool GetTokenInformation(IntPtr token, int kind, IntPtr value, uint size, out uint needed);
    [DllImport("advapi32.dll")] private static extern uint GetLengthSid(IntPtr sid);
    [DllImport("advapi32.dll")] private static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
    [DllImport("advapi32.dll")] private static extern IntPtr GetSidSubAuthority(IntPtr sid, uint index);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CreateProcessAsUser(IntPtr token, string application, StringBuilder command,
        IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags,
        IntPtr environment, string directory, ref StartupInfo startup, out ProcessInfo process);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetInformationJobObject(IntPtr job, int kind, ref ExtendedJobLimits limits, uint size);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool TerminateJobObject(IntPtr job, uint code);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern uint WaitForSingleObject(IntPtr handle, uint timeout);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("kernel32.dll")] private static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] private static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] private static extern IntPtr GetProcessWindowStation();
    [DllImport("user32.dll")] private static extern IntPtr GetThreadDesktop(uint threadId);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool GetUserObjectInformation(IntPtr handle, int kind, StringBuilder value, uint size, out uint needed);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetUserObjectSecurity(IntPtr handle, ref uint information, byte[] descriptor, uint size, out uint needed);
    [DllImport("kernel32.dll")] private static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr memory);

    private static void Check(bool success, string action)
    {
        if (!success) throw new Win32Exception(Marshal.GetLastWin32Error(), action);
    }

    private static string UserObjectName(IntPtr handle)
    {
        var name = new StringBuilder(1024);
        uint needed;
        Check(GetUserObjectInformation(handle, 2, name, (uint)name.Capacity * 2, out needed), "Read desktop name");
        return name.ToString();
    }

    public static string DesktopName()
    {
        return UserObjectName(GetProcessWindowStation()) + "\\" + UserObjectName(GetThreadDesktop(GetCurrentThreadId()));
    }

    private static byte[] DesktopDacl(IntPtr handle, uint information = 4)
    {
        uint needed;
        GetUserObjectSecurity(handle, ref information, null, 0, out needed);
        if (needed == 0 || needed > 65536) throw new InvalidOperationException("Invalid desktop security descriptor size");
        var descriptor = new byte[needed];
        Check(GetUserObjectSecurity(handle, ref information, descriptor, needed, out needed), "Read desktop access");
        return descriptor;
    }

    public static string DesktopSecuritySnapshot()
    {
        return Convert.ToBase64String(DesktopDacl(GetProcessWindowStation())) + ":" +
            Convert.ToBase64String(DesktopDacl(GetThreadDesktop(GetCurrentThreadId())));
    }

    public static uint IntegrityRid()
    {
        IntPtr token = IntPtr.Zero, buffer = IntPtr.Zero;
        try
        {
            Check(OpenProcessToken(GetCurrentProcess(), 0x8, out token), "Read process token");
            uint size;
            GetTokenInformation(token, 25, IntPtr.Zero, 0, out size);
            if (size == 0 || size > 65536) throw new InvalidOperationException("Invalid token label size");
            buffer = Marshal.AllocHGlobal((int)size);
            Check(GetTokenInformation(token, 25, buffer, size, out size), "Read integrity label");
            var label = Marshal.PtrToStructure<SidAndAttributes>(buffer);
            byte count = Marshal.ReadByte(GetSidSubAuthorityCount(label.Sid));
            if (count == 0) throw new InvalidOperationException("Missing token integrity RID");
            return unchecked((uint)Marshal.ReadInt32(GetSidSubAuthority(label.Sid, (uint)count - 1)));
        }
        finally
        {
            if (buffer != IntPtr.Zero) Marshal.FreeHGlobal(buffer);
            if (token != IntPtr.Zero) CloseHandle(token);
        }
    }

    private static void SetWorkerObjectAccess(IntPtr token)
    {
        IntPtr buffer = IntPtr.Zero, aclBuffer = IntPtr.Zero, userSid = IntPtr.Zero;
        try
        {
            using (var identity = WindowsIdentity.GetCurrent())
            {
                uint size;
                GetTokenInformation(token, 6, IntPtr.Zero, 0, out size);
                if (size < IntPtr.Size || size > 65536) throw new InvalidOperationException("Invalid default DACL size");
                buffer = Marshal.AllocHGlobal((int)size);
                Check(GetTokenInformation(token, 6, buffer, size, out size), "Read worker default object access");
                IntPtr aclPointer = Marshal.ReadIntPtr(buffer);
                if (aclPointer != IntPtr.Zero)
                {
                    int aclSize = (ushort)Marshal.ReadInt16(aclPointer, 2);
                    var bytes = new byte[aclSize];
                    Marshal.Copy(aclPointer, bytes, 0, bytes.Length);
                    var acl = new RawAcl(bytes, 0);
                    acl.InsertAce(acl.Count, new CommonAce(AceFlags.None, AceQualifier.AccessAllowed,
                        0x10000000, identity.User, false, null));
                    bytes = new byte[acl.BinaryLength];
                    acl.GetBinaryForm(bytes, 0);
                    aclBuffer = Marshal.AllocHGlobal(bytes.Length);
                    Marshal.Copy(bytes, 0, aclBuffer, bytes.Length);
                    Check(SetTokenInformation(token, 6, ref aclBuffer, (uint)IntPtr.Size), "Set worker default object access");
                }
                // Objects created by the worker must be owned by its enabled user,
                // not the Administrators group that is now deny-only.
                Check(ConvertStringSidToSid(identity.User.Value, out userSid), "Read worker owner SID");
                Check(SetTokenInformation(token, 4, ref userSid, (uint)IntPtr.Size), "Set worker object owner");
            }
        }
        finally
        {
            if (userSid != IntPtr.Zero) LocalFree(userSid);
            if (aclBuffer != IntPtr.Zero) Marshal.FreeHGlobal(aclBuffer);
            if (buffer != IntPtr.Zero) Marshal.FreeHGlobal(buffer);
        }
    }

    public static int Run(string executable, string arguments, string directory, int timeoutSeconds)
    {
        if (!Path.IsPathFullyQualified(executable) || !File.Exists(executable))
            throw new ArgumentException("The worker executable must be an existing absolute path");
        if (timeoutSeconds < 1 || timeoutSeconds > 3600) throw new ArgumentOutOfRangeException("timeoutSeconds");
        IntPtr original = IntPtr.Zero, restricted = IntPtr.Zero, sid = IntPtr.Zero, job = IntPtr.Zero;
        ProcessInfo child = default(ProcessInfo);
        try
        {
            // QUERY | DUPLICATE | ASSIGN_PRIMARY | ADJUST_DEFAULT. A restricted version of our own
            // token does not require the assign-primary-token privilege.
            Check(OpenProcessToken(GetCurrentProcess(), 0x8b, out original), "Open launcher token");
            // Explicitly disable Administrators rather than using the legacy
            // LUA_TOKEN flag, whose extra filtering can invalidate service tokens.
            IntPtr administratorSid = IntPtr.Zero, disabledGroup = IntPtr.Zero;
            try
            {
                Check(ConvertStringSidToSid("S-1-5-32-544", out administratorSid), "Create Administrators SID");
                disabledGroup = Marshal.AllocHGlobal(Marshal.SizeOf<SidAndAttributes>());
                Marshal.StructureToPtr(new SidAndAttributes { Sid = administratorSid }, disabledGroup, false);
                Check(CreateRestrictedToken(original, 0x1, 1, disabledGroup, 0, IntPtr.Zero,
                    0, IntPtr.Zero, out restricted), "Create normal-user token");
            }
            finally
            {
                if (disabledGroup != IntPtr.Zero) Marshal.FreeHGlobal(disabledGroup);
                if (administratorSid != IntPtr.Zero) LocalFree(administratorSid);
            }
            Check(ConvertStringSidToSid("S-1-16-8192", out sid), "Create Medium integrity SID");
            var label = new SidAndAttributes { Sid = sid, Attributes = 0x20 };
            Check(SetTokenInformation(restricted, 25, ref label,
                (uint)Marshal.SizeOf<SidAndAttributes>() + GetLengthSid(sid)), "Set Medium integrity");
            SetWorkerObjectAccess(restricted);

            job = CreateJobObject(IntPtr.Zero, null);
            Check(job != IntPtr.Zero, "Create owned worker job");
            var limits = new ExtendedJobLimits { Basic = new JobLimits { Flags = 0x2000 } };
            Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf<ExtendedJobLimits>()),
                "Enable worker tree kill-on-close");
            // Inherit the actual runner desktop. Services need not be attached to
            // winsta0\\default, and forcing it can cause STATUS_DLL_INIT_FAILED.
            var startup = new StartupInfo { Size = Marshal.SizeOf<StartupInfo>() };
            var command = new StringBuilder("\"" + executable + "\" " + arguments);
            // Suspended until job assignment prevents descendants escaping ownership.
            Check(CreateProcessAsUser(restricted, executable, command, IntPtr.Zero, IntPtr.Zero,
                false, 0x4 | 0x400 | 0x08000000, IntPtr.Zero, directory, ref startup, out child),
                "Start normal-user worker");
            Check(AssignProcessToJobObject(job, child.Process), "Assign worker to owned job");
            Check(ResumeThread(child.Thread) != uint.MaxValue, "Resume worker");
            uint wait = WaitForSingleObject(child.Process, checked((uint)timeoutSeconds * 1000));
            if (wait == 258) throw new TimeoutException("Normal-user worker exceeded its time limit");
            Check(wait == 0, "Wait for worker exit");
            uint exitCode;
            Check(GetExitCodeProcess(child.Process, out exitCode), "Read worker exit code");
            return unchecked((int)exitCode);
        }
        finally
        {
            if (job != IntPtr.Zero) { TerminateJobObject(job, 1); CloseHandle(job); }
            if (child.Process != IntPtr.Zero)
            {
                // Also covers failure before job assignment. Handles, never PID lookup.
                TerminateProcess(child.Process, 1);
                WaitForSingleObject(child.Process, 10000);
                CloseHandle(child.Process);
            }
            if (child.Thread != IntPtr.Zero) CloseHandle(child.Thread);
            if (sid != IntPtr.Zero) LocalFree(sid);
            if (restricted != IntPtr.Zero) CloseHandle(restricted);
            if (original != IntPtr.Zero) CloseHandle(original);
        }
    }
}
