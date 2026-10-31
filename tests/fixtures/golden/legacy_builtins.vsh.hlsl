struct CommonUniforms {
    row_major float4x4 iris_ModelViewMat;
    row_major float4x4 iris_ProjMat;
    row_major float4x4 iris_TextureMat;
    float3 iris_ModelOffset;
    int _pad4_0;
    float4 iris_ColorModulator;
};

struct VertexOutput {
    float4 vertexColor : LOC0;
    float2 texCoord : LOC1;
    float4 gl_Position : SV_Position;
};

static float2 iris_UV0_1 = (float2)0;
static float3 iris_Normal_1 = (float3)0;
static float3 iris_Position_1 = (float3)0;
static float4 iris_Color_1 = (float4)0;
cbuffer global : register(b0) { CommonUniforms global; }
static float4 vertexColor = (float4)0;
static float2 texCoord = (float2)0;
static float4 gl_Position = (float4)0;

struct VertexOutput_main {
    float4 vertexColor_1 : LOC0;
    float2 texCoord_1 : LOC1;
    float4 gl_Position_1 : SV_Position;
};

float4x4 iris_ModelOffsetMatrix()
{
    float3 _e26 = global.iris_ModelOffset;
    float3 _e28 = global.iris_ModelOffset;
    float3 _e30 = global.iris_ModelOffset;
    return float4x4(float4(1.0, 0.0, 0.0, 0.0), float4(0.0, 1.0, 0.0, 0.0), float4(0.0, 0.0, 1.0, 0.0), float4(_e26.x, _e28.y, _e30.z, 1.0));
}

void main_1()
{
    float4x4 _e17 = global.iris_ProjMat;
    float4x4 _e18 = global.iris_ModelViewMat;
    const float4x4 _e20 = iris_ModelOffsetMatrix();
    float3 _e22 = iris_Position_1;
    gl_Position = mul(float4(_e22.x, _e22.y, _e22.z, 1.0), mul(_e20, mul(_e18, _e17)));
    float4x4 _e29 = global.iris_TextureMat;
    float2 _e30 = iris_UV0_1;
    texCoord = mul(float4(_e30.x, _e30.y, 0.0, 1.0), _e29).xy;
    float4 _e38 = iris_Color_1;
    float4 _e39 = global.iris_ColorModulator;
    float3 _e41 = iris_Normal_1;
    float3 _e48 = iris_Position_1;
    vertexColor = (((_e38 * _e39) * float4(_e41.x, _e41.y, _e41.z, 1.0)) + float4(_e48.x, _e48.y, _e48.z, 1.0));
    return;
}

VertexOutput ConstructVertexOutput(float4 arg0, float2 arg1, float4 arg2) {
    VertexOutput ret = (VertexOutput)0;
    ret.vertexColor = arg0;
    ret.texCoord = arg1;
    ret.gl_Position = arg2;
    return ret;
}

VertexOutput_main main(float2 iris_UV0_ : LOC0, float3 iris_Normal : LOC1, float3 iris_Position : LOC2, float4 iris_Color : LOC3)
{
    iris_UV0_1 = iris_UV0_;
    iris_Normal_1 = iris_Normal;
    iris_Position_1 = iris_Position;
    iris_Color_1 = iris_Color;
    main_1();
    float4 _e31 = vertexColor;
    float2 _e33 = texCoord;
    float4 _e35 = gl_Position;
    const VertexOutput vertexoutput = ConstructVertexOutput(_e31, _e33, _e35);
    const VertexOutput_main vertexoutput_1 = { vertexoutput.vertexColor, vertexoutput.texCoord, vertexoutput.gl_Position };
    return vertexoutput_1;
}
