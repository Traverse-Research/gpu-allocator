// Loads texel (0,0) of a Texture2D bound through a descriptor table and stores it through a
// root UAV, so a stale SRV to a quarantined (never-resident) texture is actually dereferenced.
//
// Compiled with:
//   "C:/Program Files (x86)/Windows Kits/10/bin/10.0.26100.0/x64/dxc.exe" -T cs_6_0 -E main -Fo table_srv_texture_load.dxil table_srv_texture_load.hlsl
Texture2D<float4> tex : register(t0);
RWByteAddressBuffer dst : register(u0);

[RootSignature("DescriptorTable(SRV(t0)), UAV(u0)")]
[numthreads(1, 1, 1)]
void main() {
    dst.Store(0, asuint(tex.Load(int3(0, 0, 0)).x));
}
