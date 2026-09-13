using Avalonia;
using Avalonia.Controls;
using Avalonia.Layout;
using Avalonia.Threading;
using Avalonia.Platform.Storage;
using System.Diagnostics;
using Velopack;
namespace Prim.Launcher;
internal sealed class MainWindow : Window
{
    private readonly TextBlock status = new() { TextWrapping = Avalonia.Media.TextWrapping.Wrap };
    private readonly TextBox notes = new() { IsReadOnly = true, AcceptsReturn = true, TextWrapping = Avalonia.Media.TextWrapping.Wrap, MinHeight = 180 };
    private readonly ProgressBar progress = new() { Minimum = 0, Maximum = 100, Height = 8 };
    private readonly List<Button> buttons = [];
    private readonly UpdateService? updates;
    private CancellationTokenSource? operation;
    private string? report;
    public MainWindow()
    {
        Title = "prim"; Width = 780; Height = 660; MinWidth = 620; MinHeight = 500;
        try { updates = new UpdateService(); } catch (Exception ex) { status.Text = ex.Message; }
        var panel = new StackPanel { Spacing = 12, Margin = new Thickness(24) };
        panel.Children.Add(new TextBlock { Text = "prim", FontSize = 32 });
        panel.Children.Add(new TextBlock { Text = $"Installed version: {updates?.Version ?? "development"}" });
        panel.Children.Add(status); panel.Children.Add(progress);
        var localNotes = Path.Combine(Paths.Content, "CHANGELOG.md");
        if (File.Exists(localNotes)) notes.Text = File.ReadAllText(localNotes);
        panel.Children.Add(new ScrollViewer { Content = notes, MaxHeight = 230 });
        var play = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
        Add(play, "Play VR", () => StartGame(true)); Add(play, "Play Desktop", () => StartGame(false)); panel.Children.Add(play);
        var row = new WrapPanel { Orientation = Orientation.Horizontal };
        Add(row, "Check for updates", Check);
        Add(row, "Download update", async () => await Prepare(updates?.Available ?? throw new InvalidOperationException("Check for an available update first.")));
        Add(row, "Install and restart", async () => { NeedUpdates(); await updates!.Apply(); Environment.Exit(0); });
        Add(row, "Verify files", async () => { var bad = await UpdateService.VerifyFiles(operation!.Token); status.Text = bad.Count == 0 ? "All managed files match." : $"{bad.Count} files need repair. Choose Repair installation."; notes.Text = string.Join('\n', bad); });
        Add(row, "Repair installation", async () => { NeedUpdates(); await Prepare(await updates!.RepairOrPrevious(false)); });
        Add(row, "Previous version", async () => { NeedUpdates(); await Prepare(await updates!.RepairOrPrevious(true)); });
        Add(row, "Create support report", async () => { report = await Diagnostics.Create(); status.Text = "Support ZIP saved. Review it before sharing."; notes.Text = report; });
        Add(row, "Open report folder", () => { OpenFolder(report is null ? Paths.Make("reports") : Path.GetDirectoryName(report)!); return Task.CompletedTask; });
        if (OperatingSystem.IsLinux()) {
            Add(row, "Install in user folder", async () => { await LinuxInstall.Install(Path.Combine(Path.GetDirectoryName(Paths.Root)!, "app")); status.Text = "Installed. Open the user-folder copy for future updates."; });
            Add(row, "Install in another folder…", async () => {
                var folders = await StorageProvider.OpenFolderPickerAsync(new FolderPickerOpenOptions { Title = "Choose an empty prim installation folder", AllowMultiple = false });
                if (folders.Count > 0 && folders[0].TryGetLocalPath() is string path) { await LinuxInstall.Install(path); status.Text = "Installed. Open the new copy for future updates."; }
            });
        }
        panel.Children.Add(row);
        var cancel = new Button { Content = "Cancel download / verification" }; cancel.Click += (_, _) => operation?.Cancel(); panel.Children.Add(cancel);
        panel.Children.Add(new TextBlock { Text = "Updates install only when clicked. Keep this launcher open while prim runs.", TextWrapping = Avalonia.Media.TextWrapping.Wrap });
        Content = new ScrollViewer { Content = panel };
        Closing += (_, e) => {
            if (GameSession.IsRunning || operation is not null) { e.Cancel = true; status.Text = "Close prim and finish or cancel the current operation before closing the launcher."; }
            else updates?.Dispose();
        };
        Opened += async (_, _) => {
            if (Environment.GetEnvironmentVariable("PRIM_LAUNCHER_SMOKE") == "1") { await Task.Delay(1500); Close(); return; }
            if (updates?.Manager.IsInstalled == true) await Run(Check);
            else status.Text = "Development build. Update controls require a packaged installation.";
        };
    }
    private void Add(Panel panel, string label, Func<Task> action)
    {
        var button = new Button { Content = label, Margin = new Thickness(0, 0, 8, 8) };
        button.Click += async (_, _) => await Run(action); panel.Children.Add(button); buttons.Add(button);
    }
    private async Task Run(Func<Task> action)
    {
        if (operation is not null) return;
        operation = new CancellationTokenSource(); foreach (var button in buttons) button.IsEnabled = false;
        try { await action(); }
        catch (OperationCanceledException) { status.Text = "Cancelled. The installed version is unchanged."; }
        catch (Exception ex) { status.Text = ex.Message; Program.Log.Write(ex.ToString()); }
        finally { operation.Dispose(); operation = null; foreach (var button in buttons) button.IsEnabled = true; }
    }
    private void NeedUpdates() { if (updates?.Manager.IsInstalled != true) throw new InvalidOperationException("Use an installed Velopack package for updates."); }
    private async Task Check()
    {
        NeedUpdates(); status.Text = "Checking for updates…"; await updates!.Check();
        status.Text = updates.Available is null ? "Up to date." : $"Version {updates.Available.TargetFullRelease.Version} is available.";
        var releases = updates.Source.LastFeed!.Assets.Where(a => a.Type == VelopackAssetType.Full && a.Version >= updates.Manager.CurrentVersion);
        notes.Text = string.Join("\n\n", releases.OrderByDescending(a => a.Version).Select(a => $"{a.Version}\n{a.NotesMarkdown}"));
    }
    private async Task Prepare(UpdateInfo update)
    {
        NeedUpdates(); status.Text = $"Preparing {update.TargetFullRelease.Version}…";
        await updates!.Prepare(update, p => Dispatcher.UIThread.Post(() => progress.Value = p), operation!.Token);
        status.Text = $"{update.TargetFullRelease.Version} is verified and ready. Close prim, then click Install and restart.";
    }
    private Task StartGame(bool vr)
    {
        if (GameSession.IsRunning) throw new InvalidOperationException("prim is already running.");
        _ = WatchGame(vr); return Task.CompletedTask;
    }
    private async Task WatchGame(bool vr)
    {
        try { status.Text = "prim is running."; await GameSession.Run(vr, updates?.Version ?? "development"); status.Text = "prim exited. Session logs saved."; }
        catch (Exception ex) { status.Text = ex.Message; Program.Log.Write(ex.ToString()); }
    }
    private static void OpenFolder(string path) => Process.Start(new ProcessStartInfo {
        FileName = OperatingSystem.IsWindows() ? "explorer.exe" : "xdg-open", UseShellExecute = false, ArgumentList = { path }
    });
}
