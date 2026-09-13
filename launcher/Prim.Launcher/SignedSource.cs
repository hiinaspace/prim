using System.Net;
using System.IO.Compression;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using Velopack;
using Velopack.Logging;
using Velopack.Sources;

namespace Prim.Launcher;

internal sealed record FeedConfig(string Url, string PublicKey, string Channel, bool AllowLoopbackHttp = false)
{
    public static FeedConfig Load() => JsonSerializer.Deserialize<FeedConfig>(File.ReadAllText(Path.Combine(Paths.Content, "update-config.json")))
        ?? throw new InvalidDataException("Missing update configuration.");
}

internal sealed class SignedSource : IUpdateSource, IDisposable
{
    private readonly FeedConfig config;
    private readonly Uri root;
    private readonly HttpClient http = new(new HttpClientHandler { AllowAutoRedirect = false }) { Timeout = TimeSpan.FromMinutes(15) };
    private readonly Dictionary<string, VelopackAsset> authorized = new(StringComparer.Ordinal);
    private readonly Dictionary<string, Dictionary<string, FileDigest>> packageContents = new(StringComparer.Ordinal);
    public VelopackAssetFeed? LastFeed { get; private set; }
    public SignedSource(FeedConfig config)
    {
        this.config = config;
        root = new Uri(config.Url.TrimEnd('/') + "/");
        if (root.Scheme != "https" && !(config.AllowLoopbackHttp && root.Scheme == "http" && root.IsLoopback))
            throw new InvalidDataException("The update feed requires HTTPS.");
        if (!string.IsNullOrEmpty(root.UserInfo) || !string.IsNullOrEmpty(root.Query) || !string.IsNullOrEmpty(root.Fragment))
            throw new InvalidDataException("Unsupported feed URL.");
        if (!Regex.IsMatch(config.Channel, "^[a-z0-9-]+$")) throw new InvalidDataException("Invalid update channel.");
    }
    public static (VelopackAssetFeed Feed, long Sequence, Dictionary<string, Dictionary<string, FileDigest>> Contents) Verify(string envelope, string pem, string channel)
    {
        using var outer = JsonDocument.Parse(envelope);
        var payload = Convert.FromBase64String(outer.RootElement.GetProperty("Payload").GetString()!);
        var signature = Convert.FromBase64String(outer.RootElement.GetProperty("Signature").GetString()!);
        using var rsa = RSA.Create(); rsa.ImportFromPem(pem);
        if (!rsa.VerifyData(payload, signature, HashAlgorithmName.SHA256, RSASignaturePadding.Pss))
            throw new CryptographicException("Update metadata signature does not match the trusted publisher.");
        using var doc = JsonDocument.Parse(payload);
        var p = doc.RootElement;
        if (p.GetProperty("Schema").GetInt32() != 1 || p.GetProperty("Channel").GetString() != channel)
            throw new InvalidDataException("Unsupported update metadata or channel.");
        var feed = VelopackAssetFeed.FromJson(p.GetProperty("Feed").GetRawText());
        if (feed.Assets.Length > 10000) throw new InvalidDataException("Update feed is too large.");
        var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var a in feed.Assets) {
            if (!Regex.IsMatch(a.FileName ?? "", "^[a-zA-Z0-9][a-zA-Z0-9._-]*$") || !names.Add(a.FileName!) ||
                a.PackageId != "Prim" || !Regex.IsMatch(a.SHA256 ?? "", "^[a-fA-F0-9]{64}$") || a.Size <= 0 || a.Size > 16L * 1024 * 1024 * 1024)
                throw new InvalidDataException("Invalid release asset.");
        }
        var contents = p.GetProperty("PackageContents").Deserialize<Dictionary<string, Dictionary<string, FileDigest>>>() ?? throw new InvalidDataException("Missing package contents.");
        foreach (var a in feed.Assets.Where(a => a.Type == VelopackAssetType.Full)) {
            if (!contents.TryGetValue(a.FileName, out var files) || files.Count == 0 || files.Count > 20000 || files.Values.Any(v => v.Size < 0 || !Regex.IsMatch(v.Sha256, "^[a-fA-F0-9]{64}$")))
                throw new InvalidDataException("Missing authenticated package content hashes.");
        }
        return (feed, p.GetProperty("Sequence").GetInt64(), contents);
    }
    public async Task<VelopackAssetFeed> GetReleaseFeed(IVelopackLogger logger, string? appId, string channel,
        Guid? stagingId = null, VelopackAsset? latestLocalRelease = null)
    {
        if (channel != config.Channel) throw new InvalidDataException("Unexpected update channel.");
        using var request = new HttpRequestMessage(HttpMethod.Get, new Uri(root, $"releases.{channel}.signed.json"));
        request.Headers.CacheControl = new CacheControlHeaderValue { NoCache = true };
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(20));
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, timeout.Token);
        response.EnsureSuccessStatusCode();
        await using var stream = await response.Content.ReadAsStreamAsync(timeout.Token);
        using var body = new MemoryStream();
        var buffer = new byte[8192]; int n;
        while ((n = await stream.ReadAsync(buffer, timeout.Token)) != 0) {
            if (body.Length + n > 8 * 1024 * 1024) throw new InvalidDataException("Update metadata exceeds the size limit.");
            body.Write(buffer, 0, n);
        }
        var (feed, sequence, contents) = Verify(Encoding.UTF8.GetString(body.ToArray()), config.PublicKey, channel);
        var history = Path.Combine(Paths.Root, $"feed-{channel}.json");
        var seen = File.Exists(history) ? JsonSerializer.Deserialize<long>(File.ReadAllText(history)) : 0;
        if (sequence < seen) throw new InvalidDataException("The server returned older update metadata. Retry later.");
        Paths.AtomicJson(history, sequence);
        // Keep already-authorized assets for a user-pinned transaction while a later check occurs.
        foreach (var a in feed.Assets) {
            authorized[a.FileName + ":" + a.SHA256] = a;
            if (contents.TryGetValue(a.FileName, out var entries)) packageContents[a.FileName + ":" + a.SHA256] = entries;
        }
        return LastFeed = feed;
    }
    public async Task DownloadReleaseEntry(IVelopackLogger logger, VelopackAsset asset, string localFile,
        Action<int> progress, CancellationToken cancelToken = default)
    {
        if (!authorized.TryGetValue(asset.FileName + ":" + asset.SHA256, out var expected) || expected.Size != asset.Size)
            throw new InvalidDataException("Package was not authorized by the signed feed.");
        using var response = await http.GetAsync(new Uri(root, asset.FileName), HttpCompletionOption.ResponseHeadersRead, cancelToken);
        response.EnsureSuccessStatusCode();
        if (response.StatusCode != HttpStatusCode.OK || (response.Content.Headers.ContentLength is long length && length != asset.Size))
            throw new InvalidDataException("Unexpected package response/length.");
        await using var input = await response.Content.ReadAsStreamAsync(cancelToken);
        await using (var output = File.Create(localFile)) {
            var bytes = new byte[131072]; long total = 0; int n;
            while ((n = await input.ReadAsync(bytes, cancelToken)) != 0) {
                total += n;
                if (total > asset.Size) throw new InvalidDataException("Package exceeds signed size.");
                await output.WriteAsync(bytes.AsMemory(0, n), cancelToken);
                progress((int)(total * 100 / asset.Size));
            }
            if (total != asset.Size) throw new InvalidDataException("Incomplete package download.");
        }
        if (!await Matches(localFile, asset, cancelToken)) throw new CryptographicException("Package checksum failed.");
    }
    public static async Task<bool> Matches(string path, VelopackAsset asset, CancellationToken ct = default)
    {
        if (!File.Exists(path) || new FileInfo(path).Length != asset.Size) return false;
        await using var f = File.OpenRead(path);
        return Convert.ToHexString(await SHA256.HashDataAsync(f, ct)).Equals(asset.SHA256, StringComparison.OrdinalIgnoreCase);
    }
    public async Task<bool> VerifyPackage(string path, VelopackAsset asset, CancellationToken ct = default)
    {
        if (!File.Exists(path) || !packageContents.TryGetValue(asset.FileName + ":" + asset.SHA256, out var expected)) return false;
        if (await Matches(path, asset, ct)) return true;
        // Delta output is recompressed by Rust; the ZIP bytes differ but its signed entries must match exactly.
        return await VerifyEntries(path, expected, ct);
    }
    internal static async Task<bool> VerifyEntries(string path, Dictionary<string, FileDigest> expected, CancellationToken ct = default)
    {
        try {
            using var zip = ZipFile.OpenRead(path);
            var entries = zip.Entries.Where(e => !e.FullName.EndsWith('/')).ToArray();
            if (entries.Length != expected.Count || entries.Select(e => e.FullName).Distinct(StringComparer.OrdinalIgnoreCase).Count() != entries.Length) return false;
            foreach (var e in entries) {
                if (!expected.TryGetValue(e.FullName, out var digest) || e.Length != digest.Size) return false;
                await using var stream = e.Open();
                if (!Convert.ToHexString(await SHA256.HashDataAsync(stream, ct)).Equals(digest.Sha256, StringComparison.OrdinalIgnoreCase)) return false;
            }
            return true;
        } catch (InvalidDataException) { return false; }
    }
    public void Dispose() => http.Dispose();
}
