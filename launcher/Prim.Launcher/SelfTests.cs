using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.IO.Compression;

namespace Prim.Launcher;
internal static class SelfTests
{
    public static async Task Run()
    {
        int checks = 0;
        void Assert(bool condition, string name) { if (!condition) throw new Exception("Test failed: " + name); checks++; }
        using var key = RSA.Create(2048);
        var payload = JsonSerializer.SerializeToUtf8Bytes(new { Schema = 1, Channel = "test", Sequence = 1, Feed = new { Assets = Array.Empty<object>() }, PackageContents = new Dictionary<string, object>() });
        var signature = key.SignData(payload, HashAlgorithmName.SHA256, RSASignaturePadding.Pss);
        string Wrap(byte[] p, byte[] s) => JsonSerializer.Serialize(new { Payload = Convert.ToBase64String(p), Signature = Convert.ToBase64String(s) });
        var result = SignedSource.Verify(Wrap(payload, signature), key.ExportSubjectPublicKeyInfoPem(), "test");
        Assert(result.Sequence == 1, "valid signed feed");
        payload[5] ^= 1;
        try { SignedSource.Verify(Wrap(payload, signature), key.ExportSubjectPublicKeyInfoPem(), "test"); throw new Exception("tampered metadata accepted"); }
        catch (CryptographicException) { checks++; }
        payload[5] ^= 1;
        try { SignedSource.Verify(Wrap(payload, signature), key.ExportSubjectPublicKeyInfoPem(), "other"); throw new Exception("wrong channel accepted"); }
        catch (InvalidDataException) { checks++; }
        foreach (var bad in new[] { "../outside", "/outside", "a/../../b", "a\\b" }) {
            try { UpdateService.SafePath(Paths.Root, bad); throw new Exception("unsafe path accepted"); }
            catch (InvalidDataException) { checks++; }
        }
        var fixtures = new[] {
            ("token=abcSECRETabc", "abcSECRETabc"),
            ("[prim media] {\"source\":\"private-movie.mkv\"}", "private-movie"),
            ("https://alice:password@video.example/watch?token=PRIVATE", "PRIVATE"),
            ("C:\\Users\\Alice\\Private Movie.mkv", "Alice"),
            ("/home/alice/private.mkv", "alice"),
            ("contact alice@example.com peer 192.168.24.81", "alice@example.com"),
            ("key " + new string('b', 64), new string('b', 64)),
            ("Bearer ABC-SECRET", "ABC-SECRET")
        };
        foreach (var (input, secret) in fixtures) Assert(!Diagnostics.Scrub(input).Contains(secret), "redaction " + checks);
        Assert(Diagnostics.Scrub("2026-09-12T20:10:55.000Z").Contains("20:10:55"), "timestamps preserved");
        Assert(!Diagnostics.Scrub("peer 2001:db8::1234").Contains("2001:db8"), "IPv6 scrubbed");
        var path = Path.Combine(Paths.Make("tmp"), "hash-test-" + Guid.NewGuid());
        try {
            await File.WriteAllTextAsync(path, "valid");
            var asset = new Velopack.VelopackAsset { Size = 5, SHA256 = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes("valid"))) };
            Assert(await SignedSource.Matches(path, asset), "valid cache");
            await File.WriteAllTextAsync(path, "wrong");
            Assert(!await SignedSource.Matches(path, asset), "corrupt same-sized cache");
        } finally { File.Delete(path); }
        var zipPath = path + ".zip";
        try {
            var bytes = Encoding.UTF8.GetBytes(new string('x', 4096));
            var entries = new Dictionary<string, FileDigest> { ["lib/app/test"] = new(bytes.Length, Convert.ToHexString(SHA256.HashData(bytes)), false) };
            void MakeZip(CompressionLevel level, bool corrupt = false, bool extra = false) {
                File.Delete(zipPath);
                using var zip = ZipFile.Open(zipPath, ZipArchiveMode.Create);
                using (var s = zip.CreateEntry("lib/app/test", level).Open()) s.Write(corrupt ? Encoding.UTF8.GetBytes(new string('y', 4096)) : bytes);
                if (extra) zip.CreateEntry("extra");
            }
            MakeZip(CompressionLevel.NoCompression);
            Assert(await SignedSource.VerifyEntries(zipPath, entries), "uncompressed authenticated ZIP");
            MakeZip(CompressionLevel.SmallestSize);
            Assert(await SignedSource.VerifyEntries(zipPath, entries), "recompressed authenticated ZIP");
            MakeZip(CompressionLevel.SmallestSize, corrupt: true);
            Assert(!await SignedSource.VerifyEntries(zipPath, entries), "changed ZIP entry rejected");
            MakeZip(CompressionLevel.SmallestSize, extra: true);
            Assert(!await SignedSource.VerifyEntries(zipPath, entries), "extra ZIP entry rejected");
        } finally { File.Delete(zipPath); }
        Console.WriteLine($"Passed {checks} launcher checks.");
    }
}
