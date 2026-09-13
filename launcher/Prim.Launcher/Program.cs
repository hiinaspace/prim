using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Themes.Fluent;
using Velopack;
using Velopack.Locators;

namespace Prim.Launcher;

internal static class Program
{
    public static readonly LauncherLog Log = new();
    [STAThread]
    public static int Main(string[] args)
    {
        try {
            Paths.Initialize();
            var app = VelopackApp.Build().SetLogger(Log).SetAutoApplyOnStartup(false);
            if (OperatingSystem.IsLinux()) app.SetLocator(new UserLinuxLocator(Log));
            app.Run();
            using var instance = new FileStream(Path.Combine(Paths.Root, "launcher.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
            if (args.Length > 0 && args[0].StartsWith("--")) {
                var result = Commands.Run(args).GetAwaiter().GetResult();
                if (result.HasValue) return result.Value;
            }
            AppBuilder.Configure<LauncherApp>().UsePlatformDetect()
                .With(new X11PlatformOptions { RenderingMode = [X11RenderingMode.Software] })
                .With(new Win32PlatformOptions { RenderingMode = [Win32RenderingMode.Software], CompositionMode = [Win32CompositionMode.RedirectionSurface] })
                .StartWithClassicDesktopLifetime(args);
            return 0;
        } catch (Exception ex) {
            Log.Write(ex.ToString());
            Console.Error.WriteLine(ex.Message);
            return 1;
        }
    }
}

public sealed class LauncherApp : Application
{
    public override void Initialize() => Styles.Add(new FluentTheme());
    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
            desktop.MainWindow = new MainWindow();
        base.OnFrameworkInitializationCompleted();
    }
}

[System.Runtime.Versioning.SupportedOSPlatform("linux")]
internal sealed class UserLinuxLocator : LinuxVelopackLocator
{
    public UserLinuxLocator(LauncherLog log) : base(new PersistentProcess(log), log) { }
    public override string PackagesDir => Directory.CreateDirectory(Path.Combine(Paths.UpdateTemp, "packages")).FullName;
    public override string AppTempDir => Paths.UpdateTemp;
}

internal sealed class PersistentProcess(LauncherLog log) : IProcessImpl
{
    private readonly DefaultProcessImpl inner = new(log);
    public string GetCurrentProcessPath() => inner.GetCurrentProcessPath();
    public uint GetCurrentProcessId() => inner.GetCurrentProcessId();
    public void Exit(int code) => inner.Exit(code);
    public void StartProcess(string exe, IEnumerable<string> args, string workDir, bool showWindow)
    {
        if (OperatingSystem.IsLinux() && Path.GetFileName(exe) == "UpdateNix") {
            // The native Linux locator requires an AppDir-shaped helper path.
            var directory = Path.Combine(Paths.UpdateTemp, "helpers", Guid.NewGuid().ToString("N"), "usr", "bin");
            Directory.CreateDirectory(directory);
            var copy = Path.Combine(directory, "UpdateNix"); File.Copy(exe, copy);
            File.Copy(Path.Combine(Path.GetDirectoryName(exe)!, "sq.version"), Path.Combine(directory, "sq.version"));
            File.SetUnixFileMode(copy, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
            // AppImage extraction and its original CWD can disappear as the parent exits.
            inner.StartProcess(copy, args, Paths.UpdateTemp, showWindow);
        } else inner.StartProcess(exe, args, workDir, showWindow);
    }
}
