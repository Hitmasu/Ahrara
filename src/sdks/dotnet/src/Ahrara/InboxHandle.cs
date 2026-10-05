using System.Runtime.InteropServices;

namespace AhraraSdk;

// The ABI ID is not a pointer. Retain all 64 bits independently of SafeHandle's sentinel.
internal sealed class InboxHandle : SafeHandle
{
    internal ulong Id { get; }

    internal InboxHandle(ulong id) : base(0, true)
    {
        Id = id;
        SetHandle(1);
    }

    public override bool IsInvalid => handle == 0;

    // Only finalizes an inbox that was never disposed; Inbox.Dispose closes explicitly.
    protected override bool ReleaseHandle()
    {
        try
        {
            Native.Call(new { op = "close", handle = Id });
            return true;
        }
        catch
        {
            // SafeHandle finalization must never throw.
            return false;
        }
    }
}
