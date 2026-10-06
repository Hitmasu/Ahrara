using System.Reflection;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace AhraraSdk;

internal static partial class Native
{
    private const string Library = "ahrara_native";
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower
    };

    static Native()
    {
        NativeLibrary.SetDllImportResolver(typeof(Native).Assembly, Resolve);
        if (AbiVersion() != 1)
            throw new NotSupportedException("This SDK requires Ahrara native ABI version 1.");
    }

    private static nint Resolve(string name, Assembly assembly, DllImportSearchPath? path)
    {
        if (name != Library) return 0;

        var developmentLibrary = Environment.GetEnvironmentVariable("AHRARA_NATIVE_LIBRARY");
        if (string.IsNullOrWhiteSpace(developmentLibrary)) return 0;
        if (!Path.IsPathFullyQualified(developmentLibrary))
            throw new ArgumentException("AHRARA_NATIVE_LIBRARY must be an absolute path.");

        return NativeLibrary.Load(developmentLibrary);
    }

    [LibraryImport(Library, EntryPoint = "ahrara_abi_version")]
    private static partial uint AbiVersion();

    [LibraryImport(Library, EntryPoint = "ahrara_call", StringMarshalling = StringMarshalling.Utf8)]
    private static partial NativeStringHandle CallRaw(string request);

    [LibraryImport(Library, EntryPoint = "ahrara_string_free")]
    internal static partial void FreeString(nint response);

    internal static JsonElement Call(object request)
    {
        using var response = CallRaw(JsonSerializer.Serialize(request, JsonOptions));
        if (response.IsInvalid) throw new InvalidOperationException("Ahrara returned an empty native response.");

        var text = Marshal.PtrToStringUTF8(response.DangerousGetHandle())!;
        using var document = JsonDocument.Parse(text);
        var root = document.RootElement;
        if (!root.GetProperty("ok").GetBoolean())
        {
            var error = root.GetProperty("error");
            throw new AhraraException(error.GetProperty("code").GetString()!,
                error.GetProperty("message").GetString()!);
        }

        return root.GetProperty("result").Clone();
    }

    internal static T Deserialize<T>(JsonElement value) =>
        value.Deserialize<T>(JsonOptions) ?? throw new InvalidOperationException("Missing native result.");
}
