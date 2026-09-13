using System.Diagnostics;

namespace Prim.Launcher;
internal static class LinuxInstall
{
    public static async Task Install(string directory)
    {
        if (!OperatingSystem.IsLinux()) throw new PlatformNotSupportedException();
        if (GameSession.IsRunning) throw new InvalidOperationException("Close prim before moving the launcher.");
        var source = Environment.GetEnvironmentVariable("APPIMAGE");
        if (string.IsNullOrEmpty(source) || !File.Exists(source)) throw new InvalidOperationException("Start the AppImage to install it.");
        directory = Path.GetFullPath(directory);
        if (directory.Any(char.IsControl)) throw new InvalidDataException("Installation paths cannot contain control characters.");
        Directory.CreateDirectory(directory);
        var target = Path.Combine(directory, "Prim.AppImage");
        if (Path.GetFullPath(source) != target) {
            if (File.Exists(target)) throw new IOException("Prim.AppImage already exists in that folder. Choose an empty install folder.");
            var temporary = target + ".part";
            try {
                await using (var input = File.OpenRead(source)) await using (var output = new FileStream(temporary, FileMode.CreateNew)) await input.CopyToAsync(output);
                File.SetUnixFileMode(temporary, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
                File.Move(temporary, target);
            } finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
        var data = Environment.GetEnvironmentVariable("XDG_DATA_HOME") ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".local/share");
        var applications = Path.Combine(data, "applications"); Directory.CreateDirectory(applications);
        // A small script preserves the Nix FHS runtime for desktop launches and updates.
        var wrapper = Path.Combine(directory, "run-prim");
        static string Quote(string s) => "'" + s.Replace("'", "'\"'\"'") + "'";
        var fhs = Environment.GetEnvironmentVariable("PRIM_LAUNCHER_FHS");
        var command = string.IsNullOrEmpty(fhs) ? "exec " + Quote(target) + " \"$@\""
            : "exec " + Quote(fhs) + " -c 'exec \"$@\"' prim " + Quote(target) + " \"$@\"";
        await File.WriteAllTextAsync(wrapper, "#!/bin/sh\n" + command + "\n");
        File.SetUnixFileMode(wrapper, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
        var escaped = wrapper.Replace("\\", "\\\\\\\\").Replace("\"", "\\\\\"").Replace("`", "\\\\`").Replace("$", "\\\\$").Replace("%", "%%");
        await File.WriteAllTextAsync(Path.Combine(applications, "prim.desktop"), $"[Desktop Entry]\nType=Application\nName=prim\nExec=\"{escaped}\"\nTerminal=false\nCategories=Game;\n");
        Program.Log.Write("Installed AppImage and user shortcut.");
    }
}
