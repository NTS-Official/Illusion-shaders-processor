struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

Texture2D<float> ShadowMap : register(t0);
SamplerComparisonState ShadowMap_sampler : register(s0);
static float4 fragColor = (float4)0;

void main_1()
{
    float _e8 = ShadowMap.SampleCmp(ShadowMap_sampler, float2(0.5, 0.5), 0.25);
    fragColor = (_e8).xxxx;
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
    float4 _e7 = fragColor;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e7);
    return fragmentoutput;
}
