struct VertexOutput {
    float4 gl_Position : SV_Position;
};

ByteAddressBuffer Faces : register(t0);
static uint gl_VertexIndex_1 = (uint)0;
static float4 gl_Position = (float4)0;

struct VertexOutput_main {
    float4 gl_Position_1 : SV_Position;
};

int dxr_texel_Faces(int index)
{
    int index_1 = (int)0;
    int word = (int)0;
    int value = (int)0;

    index_1 = index;
    int _e5 = index_1;
    int _e11 = asint(Faces.Load((_e5 >> 2u)*4+0));
    word = _e11;
    int _e13 = word;
    int _e14 = index_1;
    value = ((_e13 >> uint(asint(asuint((_e14 & int(3))) * asuint(int(8))))) & int(255));
    int _e24 = value;
    return ((_e24 << 24u) >> 24u);
}

void main_1()
{
    int face = (int)0;

    uint _e4 = gl_VertexIndex_1;
    const int _e6 = dxr_texel_Faces(int(_e4));
    face = _e6;
    int _e9 = face;
    gl_Position = (float(_e9)).xxxx;
    return;
}

VertexOutput ConstructVertexOutput(float4 arg0) {
    VertexOutput ret = (VertexOutput)0;
    ret.gl_Position = arg0;
    return ret;
}

VertexOutput_main main(uint gl_VertexIndex : SV_VertexID)
{
    gl_VertexIndex_1 = gl_VertexIndex;
    main_1();
    float4 _e6 = gl_Position;
    const VertexOutput vertexoutput = ConstructVertexOutput(_e6);
    const VertexOutput_main vertexoutput_1 = { vertexoutput.gl_Position };
    return vertexoutput_1;
}
