using System.Runtime.InteropServices;

namespace AhraraSdk;

internal sealed class NativeStringHandle : SafeHandle
{
    public NativeStringHandle() : base(0, true) { }

    public override bool IsInvalid => handle == 0;

    protected override bool ReleaseHandle()
    {
        Native.FreeString(handle);
        return true;
    }
}
