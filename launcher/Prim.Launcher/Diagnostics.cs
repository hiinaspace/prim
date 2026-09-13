using System.IO.Compression;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using Microsoft.Win32;

namespace Prim.Launcher;

internal static class Diagnostics
{
    internal static string Scrub(string text)
    {
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        foreach (var path in new[] { Paths.Root, Paths.Content.TrimEnd(Path.DirectorySeparatorChar), home }.OrderByDescending(x => x.Length))
            if (!string.IsNullOrEmpty(path)) text = text.Replace(path, "<local>", StringComparison.OrdinalIgnoreCase).Replace(path.Replace("\\", "\\\\"), "<local>", StringComparison.OrdinalIgnoreCase);
        text = Regex.Replace(text, @"-----BEGIN [^-]*PRIVATE KEY-----[\s\S]*?-----END [^-]*PRIVATE KEY-----", "<private-key>");
        // Source events include movie names and private paths even if they are not URLs.
        text = Regex.Replace(text, @"(?m)^.*\[prim media\].*$", "[prim media] <source details omitted>");
        text = Regex.Replace(text, @"https?://[^\s\""<>]+", m => Uri.TryCreate(m.Value.TrimEnd(',', ')'), UriKind.Absolute, out var uri) ? $"{uri.Scheme}://{uri.Host}/<redacted>" : "<url>");
        text = Regex.Replace(text, @"(?i)\b(?:bearer\s+\S+|(?:secret|token|password|cookie|authorization)\s*[\""']?\s*[:=]\s*[\""']?[^\s,\""']+)", "<credential>");
        text = Regex.Replace(text, @"\b(?:[0-9a-fA-F]{64}|gh[pousr]_[A-Za-z0-9_]+|github_pat_[A-Za-z0-9_]+)\b", "<identifier>");
        text = Regex.Replace(text, @"[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}", "<email>", RegexOptions.IgnoreCase);
        text = Regex.Replace(text, @"\b(?:\d{1,3}\.){3}\d{1,3}\b", "<ip>");
        text = Regex.Replace(text, @"(?i)(?<!\w)(?=[0-9a-f:]*::|(?:[0-9a-f]{1,4}:){7})(?:[0-9a-f]{0,4}:){2,}[0-9a-f:%]*(?!\w)", "<network-id>");
        text = Regex.Replace(text, @"(?i)\b[A-Z]:[\\/]+[^\r\n\""<>]*", "<path>");
        text = Regex.Replace(text, @"(?<![\w:/])/(?:home|Users|mnt|media|run/user|tmp|var/tmp)/[^\s\""<>]*", "<path>");
        return text;
    }
    public static async Task<string> Create()
    {
        var output = Path.Combine(Paths.Make("reports"), $"prim-support-{DateTime.UtcNow:yyyyMMdd-HHmmss}-{Guid.NewGuid():N}.zip");
        var summary = new StringBuilder();
        summary.AppendLine("prim support report — local export; no automatic upload");
        summary.AppendLine($"Created: {DateTimeOffset.UtcNow:O}");
        summary.AppendLine($"OS: {RuntimeInformation.OSDescription}; architecture: {RuntimeInformation.OSArchitecture}");
        summary.AppendLine($"Launcher runtime: {RuntimeInformation.FrameworkDescription}");
        summary.AppendLine($"Session type: {Environment.GetEnvironmentVariable("XDG_SESSION_TYPE") ?? "Windows/unknown"}");
        summary.AppendLine($"CPU: {Environment.GetEnvironmentVariable("PROCESSOR_IDENTIFIER") ?? ReadSelected("/proc/cpuinfo", "model name")}");
        summary.AppendLine($"Memory: {ReadSelected("/proc/meminfo", "MemTotal")}");
        var runtime = FindRuntime();
        summary.AppendLine($"Configured OpenXR manifest: {runtime ?? "not found"}; exists: {runtime is not null && File.Exists(runtime)}");
        if (runtime is not null && File.Exists(runtime) && new FileInfo(runtime).Length < 256 * 1024) {
            try {
                using var manifest = System.Text.Json.JsonDocument.Parse(await File.ReadAllTextAsync(runtime));
                if (manifest.RootElement.TryGetProperty("runtime", out var r)) {
                    foreach (var field in new[] { "name", "library_path" })
                        if (r.TryGetProperty(field, out var value)) summary.AppendLine($"OpenXR {field}: {value}");
                }
            } catch (Exception ex) when (ex is IOException or System.Text.Json.JsonException) { summary.AppendLine("OpenXR manifest could not be read/parsed."); }
        }
        summary.AppendLine("Runtime selection is configuration evidence; see application logs for actual XR/GPU initialization.");
        summary.AppendLine("Text logs are scrubbed. No media, settings files, screenshots, recordings or memory dumps are included.");
        summary.AppendLine("Crash backtraces, when present, are part of session stderr/Godot logs. Review before sharing.");
        var files = new List<(string Source, string Name)>();
        foreach (var name in new[] { "launcher.log", "launcher.log.previous" }) files.Add((Path.Combine(Paths.Make("logs"), name), "launcher/" + name));
        var sessions = Directory.GetDirectories(Paths.Make("sessions")).OrderDescending().Take(3);
        int index = 0;
        foreach (var session in sessions) {
            ++index;
            foreach (var name in new[] { "session.json", "stdout.log", "stderr.log", "godot.log" }) files.Add((Path.Combine(session, name), $"sessions/{index}/{name}.txt"));
        }
        var userDir = OperatingSystem.IsWindows()
            ? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "prim")
            : Path.GetDirectoryName(Paths.Root)!;
        files.Add((Path.Combine(userDir, "logs", "godot.log"), "previous-godot.txt"));
        files.Add((Path.Combine(Paths.Game, "build-revisions.json"), "build-revisions.txt"));
        files.Add((Path.Combine(Paths.Root, "verification.json"), "verification.txt"));
        files.Add((Path.Combine(Paths.UpdateTemp, "velopack.log"), "updater.txt"));
        long remaining = 20 * 1024 * 1024;
        using var archive = ZipFile.Open(output, ZipArchiveMode.Create);
        var included = new List<string>();
        foreach (var (file, name) in files) {
            if (!File.Exists(file) || (File.GetAttributes(file) & FileAttributes.ReparsePoint) != 0 || remaining <= 0) continue;
            try {
                await using var input = new FileStream(file, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
                var count = (int)Math.Min(Math.Min(input.Length, 2 * 1024 * 1024), remaining);
                var buffer = new byte[count]; var actual = await input.ReadAtLeastAsync(buffer, count, false);
                var text = Scrub(Encoding.UTF8.GetString(buffer, 0, actual));
                if (input.Length > count) text += "\n<truncated>\n";
                await Write(archive, name, text); remaining -= actual; included.Add(name);
            } catch (IOException) { summary.AppendLine($"Unavailable: {name}"); }
        }
        summary.AppendLine("Included:\n" + string.Join('\n', included));
        await Write(archive, "summary.txt", Scrub(summary.ToString()));
        await Write(archive, "privacy.txt", "Allowlisted text export. Redaction rules: v1. Automatic upload: false. Binary dump inclusion: false.\nThis is basic scrubbing, not a guarantee of anonymity. Preview the ZIP before sharing.\n");
        return output;
    }
    private static async Task Write(ZipArchive archive, string path, string text)
    {
        await using var w = new StreamWriter(archive.CreateEntry(path).Open()); await w.WriteAsync(text);
    }
    private static string ReadSelected(string file, string key)
    {
        try { return File.ReadLines(file).FirstOrDefault(x => x.StartsWith(key, StringComparison.Ordinal)) ?? "unavailable"; }
        catch (IOException) { return "unavailable"; }
    }
    internal static string? FindRuntime()
    {
        var forced = Environment.GetEnvironmentVariable("XR_RUNTIME_JSON");
        if (!string.IsNullOrEmpty(forced)) return forced;
        if (OperatingSystem.IsWindows()) {
            using var root = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, RegistryView.Registry64);
            using var key = root.OpenSubKey(@"SOFTWARE\Khronos\OpenXR\1");
            return key?.GetValue("ActiveRuntime") as string;
        }
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var dirs = new[] { Environment.GetEnvironmentVariable("XDG_CONFIG_HOME") ?? Path.Combine(home, ".config") }
            .Concat((Environment.GetEnvironmentVariable("XDG_CONFIG_DIRS") ?? "/etc/xdg").Split(':')).Append("/etc");
        foreach (var d in dirs) foreach (var name in new[] { "active_runtime.x86_64.json", "active_runtime.json" }) {
            var p = Path.Combine(d, "openxr", "1", name); if (File.Exists(p)) return p;
        }
        return null;
    }
}
