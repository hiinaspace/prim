using System.Security.Cryptography;
using System.Text.Json;
using Velopack;
using Velopack.Locators;

namespace Prim.Launcher;

internal sealed class UpdateService : IDisposable
{
    public SignedSource Source { get; }
    public UpdateManager Manager { get; }
    public UpdateInfo? Available { get; private set; }
    public UpdateInfo? Prepared { get; private set; }
    public string Version => Manager.CurrentVersion?.ToString() ?? "development";
    public UpdateService()
    {
        var config = FeedConfig.Load();
        Source = new SignedSource(config);
        Manager = new PrimUpdateManager(Source, new UpdateOptions { ExplicitChannel = config.Channel });
    }
    public async Task Check() => Available = await Manager.CheckForUpdatesAsync();
    public async Task<UpdateInfo> RepairOrPrevious(bool previous)
    {
        await Check();
        var versions = Source.LastFeed!.Assets.Where(a => a.Type == VelopackAssetType.Full);
        var target = previous
            ? versions.Where(a => a.Version < Manager.CurrentVersion).OrderByDescending(a => a.Version).FirstOrDefault()
            : versions.FirstOrDefault(a => a.Version == Manager.CurrentVersion);
        return target is null ? throw new InvalidOperationException("That release is no longer in the feed.") : new UpdateInfo(target, true);
    }
    public async Task Prepare(UpdateInfo update, Action<int> progress, CancellationToken ct)
    {
        Prepared = null;
        var path = PackagePath(update.TargetFullRelease);
        // Velopack 1.2.0 trusts an existing cache filename; validate it ourselves.
        if (File.Exists(path) && !await Source.VerifyPackage(path, update.TargetFullRelease, ct)) File.Delete(path);
        if (update.BaseRelease is not null) {
            var trustedBase = Source.LastFeed!.Assets.FirstOrDefault(a => a.Type == VelopackAssetType.Full && a.Version == update.BaseRelease.Version);
            if (trustedBase is null || !await Source.VerifyPackage(PackagePath(update.BaseRelease), trustedBase, ct))
                update = new UpdateInfo(update.TargetFullRelease, update.IsDowngrade);
        }
        await Manager.DownloadUpdatesAsync(update, progress, ct);
        ct.ThrowIfCancellationRequested();
        if (!await Source.VerifyPackage(path, update.TargetFullRelease, ct)) throw new CryptographicException("Prepared update is corrupt.");
        Prepared = update;
        Program.Log.Write($"Prepared release {update.TargetFullRelease.Version}");
    }
    public async Task Apply(string[]? restartArgs = null)
    {
        if (Prepared is null) throw new InvalidOperationException("Prepare an update first.");
        if (GameSession.IsRunning) throw new InvalidOperationException("Close prim before installing the update.");
        var path = PackagePath(Prepared.TargetFullRelease);
        if (!await Source.VerifyPackage(path, Prepared.TargetFullRelease)) throw new CryptographicException("Prepared package changed; download again.");
        Manager.WaitExitThenApplyUpdates(Prepared, silent: true, restart: true, restartArgs: restartArgs);
    }
    public static async Task<List<string>> VerifyFiles(CancellationToken ct = default)
    {
        var manifest = JsonSerializer.Deserialize<Dictionary<string, FileDigest>>(await File.ReadAllTextAsync(Path.Combine(Paths.Content, "content-manifest.json"), ct))
            ?? throw new InvalidDataException("Missing content manifest.");
        var bad = new List<string>();
        foreach (var (relative, info) in manifest) {
            ct.ThrowIfCancellationRequested();
            var path = SafePath(Paths.Content, relative);
            if (!File.Exists(path) || new FileInfo(path).Length != info.Size) { bad.Add(relative); continue; }
            await using var f = File.OpenRead(path);
            if (!Convert.ToHexString(await SHA256.HashDataAsync(f, ct)).Equals(info.Sha256, StringComparison.OrdinalIgnoreCase)) bad.Add(relative);
            else if (!OperatingSystem.IsWindows() && info.Executable && (File.GetUnixFileMode(path) & UnixFileMode.UserExecute) == 0) bad.Add(relative + " (not executable)");
        }
        if (Directory.Exists(Paths.Game)) {
            foreach (var path in Directory.EnumerateFiles(Paths.Game, "*", SearchOption.AllDirectories)) {
                var relative = Path.GetRelativePath(Paths.Content, path).Replace('\\', '/');
                if (!manifest.ContainsKey(relative)) bad.Add(relative + " (unexpected)");
            }
        }
        Paths.AtomicJson(Path.Combine(Paths.Root, "verification.json"), new { At = DateTimeOffset.UtcNow, BadFiles = bad, Checked = manifest.Count });
        return bad;
    }
    internal static string SafePath(string root, string relative)
    {
        if (Path.IsPathRooted(relative) || relative.Contains('\\') || relative.Split('/').Any(x => x is "" or "." or ".."))
            throw new InvalidDataException("Unsafe manifest path.");
        var path = root;
        foreach (var part in relative.Split('/')) {
            path = Path.Combine(path, part);
            if ((File.Exists(path) || Directory.Exists(path)) && (File.GetAttributes(path) & FileAttributes.ReparsePoint) != 0)
                throw new InvalidDataException("Linked manifest paths are unsupported.");
        }
        return path;
    }
    public void Dispose() => Source.Dispose();
    private static string PackagePath(VelopackAsset asset) => Path.Combine(VelopackLocator.Current.PackagesDir!, asset.FileName);
}
internal sealed record FileDigest(long Size, string Sha256, bool Executable);
