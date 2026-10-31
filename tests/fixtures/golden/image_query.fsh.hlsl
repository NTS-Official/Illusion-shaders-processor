struct Globals {
    float4 Color;
};

struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

Texture2D<float4> Sampler0_ : register(t1);
SamplerState Sampler0_sampler : register(s1);
cbuffer global : register(b0) { Globals global; }
static float4 fragColor = (float4)0;

uint2 NagaMipDimensions2D(Texture2D<float4> tex, uint mip_level)
{
    uint4 ret;
    tex.GetDimensions(mip_level, ret.x, ret.y, ret.z);
    return ret.xy;
}

void main_1()
{
    int2 size = (int2)0;

    size = int2(NagaMipDimensions2D(Sampler0_, int(0)));
    float4 _e9 = global.Color;
    int2 _e11 = size;
    int2 _e14 = size;
    float3 _e18 = (_e9.xyz * (float(_e11.x) + float(_e14.y)));
    fragColor = float4(_e18.x, _e18.y, _e18.z, 1.0);
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
