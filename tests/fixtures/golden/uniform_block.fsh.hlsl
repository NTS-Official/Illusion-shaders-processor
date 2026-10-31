struct CommonUniforms {
    float far;
    int _pad1_0;
    int _pad1_1;
    int _pad1_2;
    row_major float4x4 gbufferProjection;
    float4 fogColor;
};

struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

cbuffer global : register(b0) { CommonUniforms global; }
static float4 fragColor = (float4)0;

void main_1()
{
    float4 _e7 = global.fogColor;
    float _e8 = global.far;
    float _e13 = global.gbufferProjection[0].x;
    fragColor = (_e7 * (_e8 + _e13));
    return;
}

FragmentOutput ConstructFragmentOutput(float4 arg0) {
    FragmentOutput ret = (FragmentOutput)0;
    ret.fragColor = arg0;
    return ret;
}

FragmentOutput main()
{
    main_1();
    float4 _e9 = fragColor;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e9);
    return fragmentoutput;
}
