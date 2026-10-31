struct CommonUniforms {
    row_major float4x4 projectionMatrix;
    row_major float4x4 modelViewMatrix;
};

struct VertexOutput {
    float4 gl_Position : SV_Position;
};

static float3 vaPosition_1 = (float3)0;
static float3 Position_1 = (float3)0;
cbuffer global : register(b0) { CommonUniforms global; }
static uint gl_VertexIndex_1 = (uint)0;
static float4 gl_Position = (float4)0;

struct VertexOutput_main {
    float4 gl_Position_1 : SV_Position;
};

void main_1()
{
    float3 world = (float3)0;
    float4 clip_ = (float4)0;

    float3 _e6 = Position_1;
    float3 _e7 = vaPosition_1;
    world = (_e6 + _e7);
    float4x4 _e10 = global.projectionMatrix;
    float4x4 _e11 = global.modelViewMatrix;
    float3 _e13 = world;
    clip_ = mul(float4(_e13.x, _e13.y, _e13.z, 1.0), mul(_e11, _e10));
    uint _e22 = gl_VertexIndex_1;
    if ((_e22 == 0u)) {
        float4 _e27 = clip_;
        clip_.y = (_e27.y + 0.001);
    }
    float4 _e32 = clip_;
    gl_Position = _e32;
    return;
}

VertexOutput ConstructVertexOutput(float4 arg0) {
    VertexOutput ret = (VertexOutput)0;
    ret.gl_Position = arg0;
    return ret;
}

VertexOutput_main main(float3 vaPosition : LOC0, float3 Position : LOC1, uint gl_VertexIndex : SV_VertexID)
{
    vaPosition_1 = vaPosition;
    Position_1 = Position;
    gl_VertexIndex_1 = gl_VertexIndex;
    main_1();
    float4 _e15 = gl_Position;
    const VertexOutput vertexoutput = ConstructVertexOutput(_e15);
    const VertexOutput_main vertexoutput_1 = { vertexoutput.gl_Position };
    return vertexoutput_1;
}
