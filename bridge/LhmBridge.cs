// LhmBridge – the only part of the Hardware Monitor with admin rights (Toolbox\DESIGN.md 10).
//
// It loads LibreHardwareMonitorLib.dll (unchanged, from lhm\ next to this exe) and passes values on over a named
// pipe that only the logged-in user may open: one JSON object per line in, one per line out. It has no settings,
// no files and no window. It is started by the task \WindowsToolbox\hardware_monitor and lies, with LHM, in
// C:\Program Files\WindowsToolbox\hardware_monitor\, where only admins can write (kit::setup puts it there).
//
// Commands:
//   {"cmd":"open","groups":["IsCpuEnabled",…]}    the first switches LHM on with these groups (fast: CPU and GPU);
//                                                   later ones add groups in the background (the RAM takes ~8 s)
//   {"cmd":"read","types":["Cpu",…]}               values of these hardware types (missing = all), each updated
//   {"cmd":"set_fan","ids":["/…/control/0"],"pct":50}   take the fans of these Control sensors over
//   {"cmd":"release","ids":[…]}                    hand them back to the board / graphics card
//   {"cmd":"drives"}                               drive health through DiskInfoToolkit (the library LHM uses)
// Every answer has "type": "ok", "values", "drives" or "error" (with "message").
//
// `LhmBridge.exe --hardware-data` is something else: no pipe, no LHM, no admin rights – the fixed data of the
// hardware from WMI (manufacturer, model, IDs, serial numbers, drivers) and DXCore (integrated GPU or a card) as one
// JSON object on stdout, then it ends. The detail window runs it for its tab "Hardware data"; what the values mean
// is worked out there (hardware_data.rs).
//
// When the connection closes – for whatever reason – every fan it set goes back to automatic and LHM is closed.
// Nobody connecting within a minute after the start ends it too.
//
// Built by build.ps1 with the csc.exe that is part of Windows (.NET Framework 4, C# 5).
using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Pipes;
using System.Management;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;
using LibreHardwareMonitor.Hardware;

[assembly: AssemblyTitle("Hardware Monitor – sensors")]
[assembly: AssemblyProduct("Windows Toolbox")]

static class Bridge
{
    const string Pipe = "windows-toolbox.hardware_monitor.bridge";
    const int ConnectWait = 60000;

    static string LhmDir;

    static int Main(string[] args)
    {
        LhmDir = Path.Combine(Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location), "lhm");
        // LHM stays in its own folder, untouched; .NET looks for it there.
        AppDomain.CurrentDomain.AssemblyResolve += (sender, e) =>
        {
            string file = Path.Combine(LhmDir, new AssemblyName(e.Name).Name + ".dll");
            return File.Exists(file) ? Assembly.LoadFrom(file) : null;
        };
        if (Array.IndexOf(args, "--hardware-data") >= 0) return HardwareData.Print();
        // A test may give the pipe another name (`--pipe name`); the task never does.
        string name = Pipe;
        int at = Array.IndexOf(args, "--pipe");
        if (at >= 0 && at + 1 < args.Length) name = args[at + 1];
        return Serve(name);
    }

    // Kept apart from Main: the first method that names an LHM type must be compiled after AssemblyResolve is set.
    [MethodImpl(MethodImplOptions.NoInlining)]
    static int Serve(string name)
    {
        var security = new PipeSecurity();
        security.AddAccessRule(new PipeAccessRule(WindowsIdentity.GetCurrent().User, PipeAccessRights.FullControl, AccessControlType.Allow));
        using (var pipe = new NamedPipeServerStream(name, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous, 65536, 65536, security))
        {
            var waiting = pipe.BeginWaitForConnection(null, null);
            if (!waiting.AsyncWaitHandle.WaitOne(ConnectWait)) return 2;
            pipe.EndWaitForConnection(waiting);
            var session = new Session();
            try
            {
                var reader = new StreamReader(pipe, new UTF8Encoding(false));
                var writer = new StreamWriter(pipe, new UTF8Encoding(false));
                writer.AutoFlush = true;
                string line;
                while ((line = reader.ReadLine()) != null)
                {
                    string answer;
                    try { answer = session.Handle(line); }
                    catch (Exception ex) { answer = Session.Json.Serialize(Session.Error(ex.GetType().Name + ": " + ex.Message)); }
                    writer.WriteLine(answer);
                }
            }
            catch (IOException) { }
            finally
            {
                session.Close();
            }
        }
        return 0;
    }
}

class Session
{
    public static readonly JavaScriptSerializer Json = new JavaScriptSerializer { MaxJsonLength = int.MaxValue };

    Computer pc;
    volatile bool opening;
    readonly HashSet<string> taken = new HashSet<string>();
    readonly object fans = new object();

    public static Dictionary<string, object> Error(string message)
    {
        return new Dictionary<string, object> { { "type", "error" }, { "message", message } };
    }

    static Dictionary<string, object> Ok()
    {
        return new Dictionary<string, object> { { "type", "ok" } };
    }

    public string Handle(string line)
    {
        var command = Json.Deserialize<Dictionary<string, object>>(line);
        object cmd;
        command.TryGetValue("cmd", out cmd);
        switch (cmd as string)
        {
            case "open": return Json.Serialize(Open(Strings(command, "groups")));
            case "read": return Json.Serialize(Read(command.ContainsKey("types") ? new HashSet<string>(Strings(command, "types")) : null));
            case "set_fan": return Json.Serialize(SetFan(Strings(command, "ids"), Convert.ToSingle(command["pct"])));
            case "release": return Json.Serialize(Release(Strings(command, "ids")));
            case "drives": return Json.Serialize(Drives.Read());
            default: return Json.Serialize(Error("unknown command: " + cmd));
        }
    }

    static List<string> Strings(Dictionary<string, object> command, string key)
    {
        var list = new List<string>();
        object value;
        if (command.TryGetValue(key, out value) && value is System.Collections.IEnumerable)
            foreach (var item in (System.Collections.IEnumerable)value) list.Add(Convert.ToString(item));
        return list;
    }

    static void Enable(Computer pc, List<string> groups)
    {
        foreach (string group in groups)
        {
            // Older LHM versions do not know every group.
            PropertyInfo property = typeof(Computer).GetProperty(group);
            if (property != null && property.PropertyType == typeof(bool)) property.SetValue(pc, true, null);
        }
    }

    Dictionary<string, object> Open(List<string> groups)
    {
        var answer = Ok();
        if (pc == null)
        {
            pc = new Computer();
            Enable(pc, groups);
            pc.Open();
            answer["elevated"] = new WindowsPrincipal(WindowsIdentity.GetCurrent()).IsInRole(WindowsBuiltInRole.Administrator);
            answer["version"] = typeof(Computer).Assembly.GetName().Version.ToString();
            return answer;
        }
        // The rest in the background: reading the CPU and GPU goes on meanwhile.
        opening = true;
        var computer = pc;
        new Thread(() =>
        {
            try { Enable(computer, groups); }
            finally { opening = false; }
        }) { IsBackground = true }.Start();
        return answer;
    }

    static IEnumerable<IHardware> Walk(IHardware hw)
    {
        yield return hw;
        foreach (var sub in hw.SubHardware)
            foreach (var h in Walk(sub)) yield return h;
    }

    IEnumerable<IHardware> All()
    {
        if (pc == null) yield break;
        foreach (var top in pc.Hardware)
            foreach (var hw in Walk(top)) yield return hw;
    }

    Dictionary<string, object> Read(HashSet<string> types)
    {
        var hardware = new List<object>();
        if (pc != null)
        {
            foreach (var top in pc.Hardware)
            {
                if (types != null && !types.Contains(top.HardwareType.ToString())) continue;
                foreach (var hw in Walk(top))
                {
                    hw.Update();
                    var sensors = new List<object>();
                    foreach (var s in hw.Sensors)
                    {
                        if (!s.Value.HasValue || float.IsNaN(s.Value.Value) || float.IsInfinity(s.Value.Value)) continue;
                        var sensor = new Dictionary<string, object> {
                            { "id", s.Identifier.ToString() }, { "type", s.SensorType.ToString() }, { "name", s.Name }, { "value", s.Value.Value } };
                        if (s.Control != null)
                            sensor["control"] = new Dictionary<string, object> {
                                { "min", s.Control.MinSoftwareValue }, { "max", s.Control.MaxSoftwareValue }, { "mode", s.Control.ControlMode.ToString() } };
                        sensors.Add(sensor);
                    }
                    hardware.Add(new Dictionary<string, object> {
                        { "id", hw.Identifier.ToString() }, { "type", hw.HardwareType.ToString() }, { "name", hw.Name },
                        { "parent", hw.Parent == null ? null : hw.Parent.Identifier.ToString() }, { "sensors", sensors } });
                }
            }
        }
        return new Dictionary<string, object> { { "type", "values" }, { "opening", opening }, { "hardware", hardware } };
    }

    List<IControl> Controls(List<string> ids)
    {
        var wanted = new HashSet<string>(ids);
        var found = new List<IControl>();
        foreach (var hw in All())
            foreach (var s in hw.Sensors)
                if (s.Control != null && wanted.Contains(s.Identifier.ToString())) found.Add(s.Control);
        return found;
    }

    Dictionary<string, object> SetFan(List<string> ids, float pct)
    {
        var controls = Controls(ids);
        if (controls.Count != ids.Count) return Error("not every fan control was found");
        lock (fans)
        {
            foreach (var c in controls)
            {
                float value = Math.Max(c.MinSoftwareValue, Math.Min(c.MaxSoftwareValue, pct));
                taken.Add(c.Sensor.Identifier.ToString());
                c.SetSoftware(value);
            }
        }
        return Ok();
    }

    Dictionary<string, object> Release(List<string> ids)
    {
        lock (fans)
        {
            foreach (var c in Controls(ids))
            {
                c.SetDefault();
                taken.Remove(c.Sensor.Identifier.ToString());
            }
        }
        return Ok();
    }

    // The connection is gone: every fan back, then LHM closed (that also hands back what is left).
    public void Close()
    {
        if (pc == null) return;
        try
        {
            lock (fans)
            {
                foreach (var c in Controls(new List<string>(taken)))
                    try { c.SetDefault(); } catch (Exception) { }
                taken.Clear();
            }
        }
        finally
        {
            pc.Close();
            pc = null;
        }
    }
}

// Drive health like CrystalDiskInfo, through DiskInfoToolkit (shipped with LHM). Only raw facts – what is "good",
// "caution" or "bad" is decided in the monitor. `dynamic`, because the library's types change between versions.
static class Drives
{
    static object Number(dynamic value)
    {
        if (value == null) return null;
        try { return Convert.ToDouble(value); } catch (Exception) { return null; }
    }

    static string Text(dynamic value)
    {
        return value == null ? "" : Convert.ToString(value).Trim();
    }

    public static Dictionary<string, object> Read()
    {
        var drives = new List<object>();
        var errors = new List<string>();
        Type manager = Type.GetType("DiskInfoToolkit.StorageManager, DiskInfoToolkit");
        if (manager == null) return Session.Error("DiskInfoToolkit could not be loaded");
        dynamic storages = manager.GetProperty("Storages").GetValue(null, null);
        foreach (dynamic s in storages)
        {
            try
            {
                s.Update();
                dynamic smart = s.Smart;
                var attributes = new List<object>();
                foreach (dynamic a in smart.SmartAttributes)
                    attributes.Add(new Dictionary<string, object> {
                        { "id", (int)a.Attribute.ID }, { "name", Text(a.Info.Name) }, { "kind", Text(a.Info.Type) },
                        { "current", (int)a.Attribute.CurrentValue }, { "worst", (int)a.Attribute.WorstValue },
                        { "threshold", (int)a.Attribute.Threshold }, { "raw", (ulong)a.Attribute.RawValueULong } });
                object hours = Number(smart.MeasuredPowerOnHours);
                if (hours == null || (double)hours == 0) hours = Number(smart.DetectedPowerOnHours);
                drives.Add(new Dictionary<string, object> {
                    { "number", (int)s.DriveNumber }, { "model", Text(s.Model) }, { "firmware", Text(s.Firmware) },
                    { "serial", Text(s.SerialNumber) }, { "size", Number(s.TotalSize) }, { "bus", Text(s.BusType) },
                    { "nvme", (bool)s.IsNVMe }, { "ssd", (bool)s.IsSSD }, { "status", Text(smart.DiskStatus) },
                    { "temp", Number(smart.Temperature) }, { "temp_warning", Number(smart.TemperatureWarning) },
                    { "temp_critical", Number(smart.TemperatureCritical) }, { "life", Number(smart.Life) },
                    { "hours", hours }, { "count", Number(smart.PowerOnCount) }, { "reads", Number(smart.HostReads) },
                    { "writes", Number(smart.HostWrites) }, { "attributes", attributes } });
            }
            catch (Exception ex)
            {
                errors.Add(Text(s.Model) + ": " + ex.Message);
            }
        }
        return new Dictionary<string, object> { { "type", "drives" }, { "drives", drives }, { "errors", errors } };
    }
}

// The fixed data of the hardware for the window's tab "Hardware data" (`--hardware-data`): the WMI rows as they are
// (every value as text, or null) and what DXCore says about each GPU. No LHM type is named here.
static class HardwareData
{
    static readonly JavaScriptSerializer Json = new JavaScriptSerializer { MaxJsonLength = int.MaxValue };

    // name, WMI namespace, query – the names are what hardware_data.rs asks for.
    static readonly string[][] Queries = {
        new[] { "processor", @"root\cimv2", "SELECT Name, Manufacturer, ProcessorId, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed, SocketDesignation, L2CacheSize, L3CacheSize FROM Win32_Processor" },
        new[] { "video", @"root\cimv2", "SELECT Name, PNPDeviceID, DriverVersion, DriverDate FROM Win32_VideoController" },
        new[] { "board", @"root\cimv2", "SELECT Manufacturer, Product, Version, SerialNumber FROM Win32_BaseBoard" },
        new[] { "bios", @"root\cimv2", "SELECT Manufacturer, SMBIOSBIOSVersion, ReleaseDate FROM Win32_BIOS" },
        new[] { "memory", @"root\cimv2", "SELECT Manufacturer, PartNumber, SerialNumber, BankLabel, DeviceLocator, Capacity, Speed, ConfiguredClockSpeed, SMBIOSMemoryType, FormFactor FROM Win32_PhysicalMemory" },
        new[] { "disks", @"root\Microsoft\Windows\Storage", "SELECT DeviceId, FriendlyName, SerialNumber, FirmwareVersion, Size, BusType, MediaType FROM MSFT_PhysicalDisk" },
        new[] { "net_kinds", @"root\StandardCimv2", "SELECT PnPDeviceID, NdisPhysicalMedium FROM MSFT_NetAdapter" },
        new[] { "network", @"root\cimv2", "SELECT Name, PNPDeviceID, MACAddress FROM Win32_NetworkAdapter WHERE PhysicalAdapter = TRUE" },
        new[] { "sound", @"root\cimv2", "SELECT Name, PNPDeviceID FROM Win32_SoundDevice" },
        new[] { "usb", @"root\cimv2", "SELECT Name, Manufacturer, PNPDeviceID FROM Win32_PnPEntity WHERE PNPDeviceID LIKE 'USB%'" },
    };

    public static int Print()
    {
        var stdout = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false));
        stdout.Write(Json.Serialize(Read()));
        stdout.Flush();
        return 0;
    }

    static Dictionary<string, object> Read()
    {
        var wmi = new Dictionary<string, object>();
        var errors = new List<string>();
        foreach (var q in Queries)
        {
            var rows = new List<object>();
            try
            {
                using (var searcher = new ManagementObjectSearcher(q[1], q[2]))
                    foreach (ManagementBaseObject obj in searcher.Get())
                    {
                        var row = new Dictionary<string, object>();
                        foreach (PropertyData p in obj.Properties)
                            row[p.Name] = p.Value == null ? null : Convert.ToString(p.Value, System.Globalization.CultureInfo.InvariantCulture);
                        rows.Add(row);
                    }
            }
            catch (Exception ex)
            {
                errors.Add(q[0] + ": " + ex.Message);
            }
            wmi[q[0]] = rows;
        }
        var gpus = new List<object>();
        try { Gpus(gpus); }
        catch (Exception ex) { errors.Add("dxcore: " + ex.Message); }
        return new Dictionary<string, object> { { "wmi", wmi }, { "gpus", gpus }, { "computer", Environment.MachineName }, { "errors", errors } };
    }

    // ---------- DXCore: is a GPU part of the processor, an external one (eGPU) or a card? The name doesn't tell
    // ("Intel Arc Graphics" is integrated, "Intel Arc A770" a card). Missing before Windows 10 2004.
    [DllImport("dxcore.dll")]
    static extern int DXCoreCreateAdapterFactory(ref Guid riid, out IntPtr factory);

    delegate int CreateAdapterList(IntPtr self, uint count, ref Guid attribute, ref Guid riid, out IntPtr list);
    delegate int GetAdapter(IntPtr self, uint index, ref Guid riid, out IntPtr adapter);
    delegate uint GetAdapterCount(IntPtr self);
    delegate int GetProperty(IntPtr self, uint property, UIntPtr size, IntPtr buffer);
    delegate uint Release(IntPtr self);

    // Method no. `n` of a COM object, counted as in dxcore_interface.h (0–2: QueryInterface, AddRef, Release).
    static T Method<T>(IntPtr obj, int n) where T : class
    {
        IntPtr vtable = Marshal.ReadIntPtr(obj);
        return Marshal.GetDelegateForFunctionPointer(Marshal.ReadIntPtr(vtable, n * IntPtr.Size), typeof(T)) as T;
    }

    static void Gpus(List<object> gpus)
    {
        var factoryId = new Guid("78ee5945-c36e-4b13-a669-005dd11c0f06");
        var compute = new Guid("248e2800-a793-4724-abaa-23a6de1be090");
        var listId = new Guid("526c7776-40e9-459b-b711-f32ad76dfc28");
        var adapterId = new Guid("f0db4c7f-fe5a-42a2-bd62-f2a6cf6fc83e");
        IntPtr factory, list;
        if (DXCoreCreateAdapterFactory(ref factoryId, out factory) != 0) return;
        try
        {
            if (Method<CreateAdapterList>(factory, 3)(factory, 1, ref compute, ref listId, out list) != 0) return;
            try
            {
                uint count = Method<GetAdapterCount>(list, 4)(list);
                IntPtr buffer = Marshal.AllocHGlobal(16);
                try
                {
                    for (uint i = 0; i < count; i++)
                    {
                        IntPtr adapter;
                        if (Method<GetAdapter>(list, 3)(list, i, ref adapterId, out adapter) != 0) continue;
                        try
                        {
                            var property = Method<GetProperty>(adapter, 6);
                            // 3 = HardwareID: vendor, device, subsystem, revision
                            if (property(adapter, 3, (UIntPtr)16, buffer) != 0) continue;
                            uint vendor = (uint)Marshal.ReadInt32(buffer), device = (uint)Marshal.ReadInt32(buffer, 4), subsys = (uint)Marshal.ReadInt32(buffer, 8);
                            Marshal.WriteByte(buffer, 0, 0);
                            property(adapter, 12, (UIntPtr)1, buffer); // IsIntegrated
                            bool integrated = Marshal.ReadByte(buffer) != 0;
                            Marshal.WriteByte(buffer, 0, 0);
                            property(adapter, 13, (UIntPtr)1, buffer); // IsDetachable
                            bool detachable = Marshal.ReadByte(buffer) != 0;
                            gpus.Add(new Dictionary<string, object> {
                                { "vendor", vendor.ToString("X4") }, { "device", device.ToString("X4") }, { "subsys", subsys.ToString("X8") },
                                { "integrated", integrated }, { "detachable", detachable } });
                        }
                        finally { Method<Release>(adapter, 2)(adapter); }
                    }
                }
                finally { Marshal.FreeHGlobal(buffer); }
            }
            finally { Method<Release>(list, 2)(list); }
        }
        finally { Method<Release>(factory, 2)(factory); }
    }
}
