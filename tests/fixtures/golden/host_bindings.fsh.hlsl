struct Globals {
    float4 Tint;
};

struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

cbuffer global : register(b3) { Globals global; }
Texture2D<float4> Sampler0_ : register(t7);
SamplerState Sampler0_sampler : register(s7);
static float4 fragColor = (float4)0;

void main_1()
{
    float4 _e7 = Sampler0_.Sample(Sampler0_sampler, (0.5).xx);
    float4 _e8 = global.Tint;
    fragColor = (_e7 * _e8);
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
