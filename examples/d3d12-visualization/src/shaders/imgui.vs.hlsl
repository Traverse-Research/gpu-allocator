struct VertexInput
{
    float2 pos : POSITION;
    float2 texCoord : TEXCOORD0;
    float4 color: COLOR;
};

struct VertexOutput
{
    float4 position : SV_POSITION;
    float2 texCoord: TEXCOORD0;
    float4 color: COLOR;
};

struct Constants
{
    float2 scale;
    float2 translation;
};

ConstantBuffer<Constants> g_constants : register(b0, space0);

VertexOutput main(VertexInput vertex)
{
    VertexOutput o;
    o.position = float4(vertex.pos * g_constants.scale + g_constants.translation, 0.0, 1.0);
    o.texCoord = vertex.texCoord;
    o.color = vertex.color;
    return o;
}