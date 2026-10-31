struct VertexOutput {
    float4 vertexColor : LOC0;
    float4 gl_Position : SV_Position;
};

static float3 Position_1 = (float3)0;
static float4 Color_1 = (float4)0;
static float2 UV0_1 = (float2)0;
Texture2D<float4> Sampler : register(t0);
SamplerState Sampler_sampler : register(s0);
static float4 vertexColor = (float4)0;
static float4 gl_Position = (float4)0;

struct VertexOutput_main {
    float4 vertexColor_1 : LOC0;
    float4 gl_Position_1 : SV_Position;
};

void main_1()
{
    float4 _e6 = Color_1;
    vertexColor = _e6;
    float3 _e8 = Position_1;
    gl_Position = float4(_e8.x, _e8.y, _e8.z, 1.0);
    float4 _e15 = gl_Position;
    gl_Position.y = -(_e15.y);
    return;
}

VertexOutput ConstructVertexOutput(float4 arg0, float4 arg1) {
    VertexOutput ret = (VertexOutput)0;
    ret.vertexColor = arg0;
    ret.gl_Position = arg1;
    return ret;
}

VertexOutput_main main(float3 Position : LOC0, float4 Color : LOC2, float2 UV0_ : LOC1)
{
    Position_1 = Position;
    Color_1 = Color;
    UV0_1 = UV0_;
    main_1();
    float4 _e19 = vertexColor;
    float4 _e21 = gl_Position;
    const VertexOutput vertexoutput = ConstructVertexOutput(_e19, _e21);
    const VertexOutput_main vertexoutput_1 = { vertexoutput.vertexColor, vertexoutput.gl_Position };
    return vertexoutput_1;
}
