SamplerState g_sampler : register(s1, space0);
Texture2D g_texture : register(t2, space0);

struct VertexInput
{
    float4 position : SV_POSITION;
    float2 texCoord: TEXCOORD0;
    float4 color: COLOR;
};

float4 main(VertexInput input) : SV_Target0
{
    //return input.color;
    return input.color * g_texture.Sample(g_sampler, input.texCoord);
}
