struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

Texture2D<float4> colortex : register(t0);
SamplerState colortex_sampler : register(s0);
Texture2D<float> shadowtex0_ : register(t1);
SamplerComparisonState shadowtex0_sampler : register(s1);
static float4 fragColor = (float4)0;

void main_1()
{
    float4 lod = (float4)0;
    float shadow = (float)0;

    float4 _e8 = colortex.SampleLevel(colortex_sampler, (0.5).xx, 0.0);
    lod = _e8;
    float _e13 = shadowtex0_.SampleCmp(shadowtex0_sampler, (0.5).xx, 0.5);
    shadow = (_e13).xxxx.x;
    float4 _e17 = lod;
    float _e18 = shadow;
    fragColor = (_e17 * _e18);
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
    float4 _e11 = fragColor;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e11);
    return fragmentoutput;
}
