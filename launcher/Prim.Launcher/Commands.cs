namespace Prim.Launcher;
internal static class Commands
{
    public static async Task<int?> Run(string[] args)
    {
        if (args[0] is "--probe" or "--probe-save") {
            var json = System.Text.Json.JsonSerializer.Serialize(new {
                framework = System.Runtime.InteropServices.RuntimeInformation.FrameworkDescription,
                os = System.Runtime.InteropServices.RuntimeInformation.OSDescription,
                version = Velopack.Locators.VelopackLocator.Current.CurrentlyInstalledVersion?.ToString(),
                content = Paths.Content,
                appImage = Environment.GetEnvironmentVariable("APPIMAGE")
            });
            Console.WriteLine(json);
            if (args[0] == "--probe-save") await File.WriteAllTextAsync(args[1], json);
            return 0;
        }
        if (args[0] == "--self-test") { await SelfTests.Run(); return 0; }
        if (args[0] == "--report") { Console.WriteLine(await Diagnostics.Create()); return 0; }
        if (args[0] == "--install-to") { await LinuxInstall.Install(args[1]); return 0; }
        if (args[0] == "--verify") { var bad = await UpdateService.VerifyFiles(); Console.WriteLine(string.Join('\n', bad)); return bad.Count == 0 ? 0 : 2; }
        if (args[0] == "--launch-smoke") return await GameSession.Run(false, "smoke", ["--quit-after", "90"]);
        if (args[0] is "--check" or "--prepare" or "--update-and-restart" or "--repair-and-restart" or "--previous-and-restart") {
            using var updates = new UpdateService(); await updates.Check();
            Console.WriteLine($"Installed: {updates.Version}; available: {updates.Available?.TargetFullRelease.Version}; delta count: {updates.Available?.DeltasToTarget.Length}");
            if (args[0] == "--check") return 0;
            var target = args[0] == "--repair-and-restart" ? await updates.RepairOrPrevious(false)
                : args[0] == "--previous-and-restart" ? await updates.RepairOrPrevious(true) : updates.Available;
            if (target is null) return 0;
            await updates.Prepare(target, p => Console.WriteLine($"Download: {p}%"), default);
            if (args[0] != "--prepare") { await updates.Apply(args.Length > 1 ? ["--probe-save", args[1]] : null); }
            return 0;
        }
        return null;
    }
}
