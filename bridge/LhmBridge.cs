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
// When the connection closes – for whatever reason – every fan it set goes back to automatic and LHM is closed.
// Nobody connecting within a minute after the start ends it too.
//
// Built by build.ps1 with the csc.exe that is part of Windows (.NET Framework 4, C# 5).
using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Pipes;
using System.Reflection;
using System.Runtime.CompilerServices;
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
