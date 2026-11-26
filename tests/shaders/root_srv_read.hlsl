// Reads 4 bytes through a root SRV at whatever GPU VA the caller binds (no bounds check),
// then writes the value through a root UAV so the read cannot be optimised away.
//
// Compiled with:
//   "C:/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/dxc.exe" -T cs_6_0 -E main -Fo root_srv_read.dxil root_srv_read.hlsl
ByteAddressBuffer src : register(t0);
RWByteAddressBuffer dst : register(u0);

[RootSignature("SRV(t0), UAV(u0)")]
[numthreads(1, 1, 1)]
void main() {
    dst.Store(0, src.Load(0));
}
