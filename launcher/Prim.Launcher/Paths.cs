using System.Text.Json;
using Velopack.Logging;

namespace Prim.Launcher;

internal static class Paths
{
    public static string Root { get; private set; } = "";
    public static string UpdateTemp { get; private set; } = "";
    public static string Content => AppContext.BaseDirectory;
    public static string Game => Path.Combine(Content, "game");
    public static string Make(string name) { var path = Path.Combine(Root, name); Directory.CreateDirectory(path); return path; }
    public static void Initialize()
    {
        Root = Path.Combine(OperatingSystem.IsWindows()
            ? Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData)
            : Environment.GetEnvironmentVariable("XDG_DATA_HOME") ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".local", "share"), "prim", "launcher");
        Directory.CreateDirectory(Root);
        if (!OperatingSystem.IsWindows()) File.SetUnixFileMode(Root, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        // The detached updater must outlive nix-shell and remain on the AppImage filesystem.
        var image = Environment.GetEnvironmentVariable("APPIMAGE");
        UpdateTemp = image is not null && File.Exists(image)
            ? Path.Combine(Path.GetDirectoryName(Path.GetFullPath(image))!, ".prim-update") : Make("tmp");
        Directory.CreateDirectory(UpdateTemp);
        if (!OperatingSystem.IsWindows()) File.SetUnixFileMode(UpdateTemp, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        Environment.SetEnvironmentVariable("TMPDIR", UpdateTemp);
    }
    public static void AtomicJson<T>(string path, T data)
    {
        var tmp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        File.WriteAllText(tmp, JsonSerializer.Serialize(data, new JsonSerializerOptions { WriteIndented = true }));
        File.Move(tmp, path, true);
    }
}

internal sealed class LauncherLog : IVelopackLogger
{
    private readonly object gate = new();
    public void Write(string message)
    {
        lock (gate) {
            var file = Path.Combine(Paths.Make("logs"), "launcher.log");
            if (File.Exists(file) && new FileInfo(file).Length > 2 * 1024 * 1024)
                File.Move(file, file + ".previous", true);
            File.AppendAllText(file, $"{DateTimeOffset.UtcNow:O} {message}\n");
        }
    }
    public void Log(VelopackLogLevel level, string? message, Exception? exception) => Write($"{level}: {message} {exception}");
}
