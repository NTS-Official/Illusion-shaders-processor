struct Globals {
    float4 Color;
    row_major float4x4 View;
    float Time;
    int _end_pad_0;
    int _end_pad_1;
    int _end_pad_2;
};

struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

cbuffer global : register(b0) { Globals global; }
TextureCube<float4> Sky : register(t1);
SamplerState Sky_sampler : register(s1);
static float4 fragColor = (float4)0;

void main_1()
{
    float4 _e9 = global.Color;
    float4 _e12 = global.View[0];
    float4 _e16 = Sky.Sample(Sky_sampler, (0.0).xxx);
    float _e17 = global.Time;
    fragColor = ((_e9 + _e12) + (_e16 * _e17));
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
    float4 _e13 = fragColor;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e13);
    return fragmentoutput;
}
