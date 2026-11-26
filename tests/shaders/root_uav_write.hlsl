// Stores 4 bytes through a root UAV at whatever GPU VA the caller binds (no bounds check).
//
// Compiled with:
//   "C:/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/dxc.exe" -T cs_6_0 -E main -Fo root_uav_write.dxil root_uav_write.hlsl
RWByteAddressBuffer dst : register(u0);

[RootSignature("UAV(u0)")]
[numthreads(1, 1, 1)]
void main() {
    dst.Store(0, 0xDEADBEEF);
}
