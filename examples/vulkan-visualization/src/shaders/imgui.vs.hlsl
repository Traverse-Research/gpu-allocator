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
    //o.position = float4((vertex.pos + g_constants.translation) * g_constants.scale, 0.0, 1.0);
    //o.position = float4(vertex.pos * float2(1.0 / 1920.0, 1.0 / 1080.0) + float2(0, 0), 0, 1);
    o.texCoord = vertex.texCoord;
    o.color = vertex.color;
    return o;
}