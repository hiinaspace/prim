using System.Diagnostics;
using System.Text.Json;

namespace Prim.Launcher;

internal static class GameSession
{
    private static Process? child;
    private static FileStream? ownership;
    private sealed record ActiveGame(int Pid, long Started);
    public static bool IsRunning {
        get {
            if (child is not null && !child.HasExited) return true;
            try {
                var state = JsonSerializer.Deserialize<ActiveGame>(File.ReadAllText(Path.Combine(Paths.Root, "active-game.json")));
                if (state is null) return false;
                using var process = Process.GetProcessById(state.Pid);
                return !process.HasExited && process.StartTime.ToUniversalTime().Ticks == state.Started;
            } catch (Exception ex) when (ex is IOException or ArgumentException or InvalidOperationException or System.ComponentModel.Win32Exception) { return false; }
        }
    }
    public static async Task<int> Run(bool vr, string version, IEnumerable<string>? extra = null)
    {
        if (IsRunning) throw new InvalidOperationException("prim is already running.");
        ownership = new FileStream(Path.Combine(Paths.Root, "game.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        try {
            var session = DateTime.UtcNow.ToString("yyyyMMdd-HHmmss") + "-" + Guid.NewGuid().ToString("N")[..6];
            var dir = Path.Combine(Paths.Make("sessions"), session); Directory.CreateDirectory(dir);
            var start = new ProcessStartInfo {
                FileName = Path.Combine(Paths.Game, OperatingSystem.IsWindows() ? "prim.exe" : "prim"),
                WorkingDirectory = Paths.Game, UseShellExecute = false,
                RedirectStandardOutput = true, RedirectStandardError = true, CreateNoWindow = true
            };
            start.Environment["PATH"] = Paths.Game + Path.PathSeparator + Path.Combine(Paths.Game, "tools") + Path.PathSeparator + Environment.GetEnvironmentVariable("PATH");
            start.ArgumentList.Add("--log-file"); start.ArgumentList.Add(Path.Combine(dir, "godot.log"));
            if (!vr) { start.ArgumentList.Add("--xr-mode"); start.ArgumentList.Add("off"); }
            if (extra is not null) foreach (var arg in extra) start.ArgumentList.Add(arg);
            if (!vr) { start.ArgumentList.Add("--"); start.ArgumentList.Add("--desktop"); }
            child = Process.Start(start) ?? throw new IOException("Failed to start prim.");
            Paths.AtomicJson(Path.Combine(Paths.Root, "active-game.json"), new ActiveGame(child.Id, child.StartTime.ToUniversalTime().Ticks));
            var began = DateTimeOffset.UtcNow;
            Paths.AtomicJson(Path.Combine(dir, "session.json"), new { Version = version, Mode = vr ? "VR" : "Desktop", Start = began, Pid = child.Id });
            Program.Log.Write($"Started prim {version}, {(vr ? "VR" : "desktop")}, session {session}");
            await Task.WhenAll(Capture(child.StandardOutput, Path.Combine(dir, "stdout.log")), Capture(child.StandardError, Path.Combine(dir, "stderr.log")), child.WaitForExitAsync());
            Paths.AtomicJson(Path.Combine(dir, "session.json"), new { Version = version, Mode = vr ? "VR" : "Desktop", Start = began, End = DateTimeOffset.UtcNow, Exit = child.ExitCode });
            Program.Log.Write($"prim exited with {child.ExitCode}");
            return child.ExitCode;
        } finally { child?.Dispose(); child = null; File.Delete(Path.Combine(Paths.Root, "active-game.json")); ownership?.Dispose(); ownership = null; }
    }
    private static async Task Capture(StreamReader input, string output)
    {
        await using var f = new StreamWriter(output);
        var buffer = new char[8192]; int count; long kept = 0;
        while ((count = await input.ReadAsync(buffer)) != 0) {
            if (kept < 4 * 1024 * 1024) { await f.WriteAsync(buffer.AsMemory(0, count)); await f.FlushAsync(); kept += count; }
            // Continue draining after the cap so a verbose native library cannot deadlock.
        }
    }
}
