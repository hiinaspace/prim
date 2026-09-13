using System.Diagnostics;
using Velopack;
using Velopack.Sources;

namespace Prim.Launcher;

// Keep Velopack's patch engine and fallback logic; only control scratch location
// and cancellable process lifetime. Upstream 1.2.0 omits --packageDir for patch.
internal sealed class PrimUpdateManager(IUpdateSource source, UpdateOptions options) : UpdateManager(source, options)
{
    protected override async Task DownloadAndApplyDeltaUpdates(UpdateInfo updates, string targetFile,
        Action<int> progress, CancellationToken cancelToken)
    {
        var deltas = updates.DeltasToTarget.OrderBy(a => a.Version).ToArray();
        for (var i = 0; i < deltas.Length; i++) {
            var index = i;
            await Source.DownloadReleaseEntry(Log, deltas[i], Path.Combine(Locator.PackagesDir!, deltas[i].FileName),
                p => progress((index * 70 + p * 70 / 100) / deltas.Length), cancelToken);
        }
        var start = new ProcessStartInfo(Locator.UpdateExePath!) { UseShellExecute = false, CreateNoWindow = true, WorkingDirectory = Paths.UpdateTemp };
        foreach (var argument in new[] { "patch", "--old", Path.Combine(Locator.PackagesDir!, updates.BaseRelease!.FileName),
            "--output", targetFile, "--packageDir", Locator.PackagesDir!, "--root", Locator.RootAppDir! }) start.ArgumentList.Add(argument);
        foreach (var delta in deltas) { start.ArgumentList.Add("--delta"); start.ArgumentList.Add(Path.Combine(Locator.PackagesDir!, delta.FileName)); }
        using var process = Process.Start(start) ?? throw new IOException("Could not start the delta patch helper.");
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancelToken);
        timeout.CancelAfter(TimeSpan.FromMinutes(5));
        try { await process.WaitForExitAsync(timeout.Token); }
        catch (OperationCanceledException) {
            if (!process.HasExited) process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync();
            cancelToken.ThrowIfCancellationRequested();
            throw new TimeoutException("Delta patching timed out.");
        }
        if (process.ExitCode != 0) throw new IOException($"Delta patch helper exited with code {process.ExitCode}.");
        progress(100);
    }
}
