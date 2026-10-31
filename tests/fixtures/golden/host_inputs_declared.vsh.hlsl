struct VertexOutput {
    float4 vertexColor : LOC0;
    float4 gl_Position : SV_Position;
};

static float3 Position_1 = (float3)0;
static float4 Color_1 = (float4)0;
static float2 UV0_1 = (float2)0;
static float4 vertexColor = (float4)0;
static float4 gl_Position = (float4)0;

struct VertexOutput_main {
    float4 vertexColor_1 : LOC0;
    float4 gl_Position_1 : SV_Position;
};

void main_1()
{
    float4 _e4 = Color_1;
    vertexColor = _e4;
    float3 _e6 = Position_1;
    float2 _e7 = UV0_1;
    float3 _e12 = (_e6 + float3(_e7.x, _e7.y, 0.0));
    gl_Position = float4(_e12.x, _e12.y, _e12.z, 1.0);
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
    float4 _e15 = vertexColor;
    float4 _e17 = gl_Position;
    const VertexOutput vertexoutput = ConstructVertexOutput(_e15, _e17);
    const VertexOutput_main vertexoutput_1 = { vertexoutput.vertexColor, vertexoutput.gl_Position };
    return vertexoutput_1;
}
