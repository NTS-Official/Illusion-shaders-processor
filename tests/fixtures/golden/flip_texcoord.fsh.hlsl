struct Globals {
    float4 Tint;
};

struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

cbuffer global : register(b0) { Globals global; }
static float2 texCoord_1 = (float2)0;
static float4 fragColor = (float4)0;

struct FragmentInput_main {
    float2 texCoord_2 : LOC0;
};

void main_1()
{
    float2 texCoordFlipped = (float2)0;
    float level = (float)0;

    float2 _e4 = texCoord_1;
    float2 _e7 = texCoord_1;
    texCoordFlipped = float2(_e4.x, (1.0 - _e7.y));
    float2 _e12 = texCoordFlipped;
    level = (_e12.y * 16.0);
    float4 _e17 = global.Tint;
    float _e18 = level;
    fragColor = (_e17 * _e18);
    return;
}

FragmentOutput ConstructFragmentOutput(float4 arg0) {
    FragmentOutput ret = (FragmentOutput)0;
    ret.fragColor = arg0;
    return ret;
}

FragmentOutput main(FragmentInput_main fragmentinput_main)
{
    float2 texCoord = fragmentinput_main.texCoord_2;
    texCoord_1 = texCoord;
    main_1();
    float4 _e9 = fragColor;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e9);
    return fragmentoutput;
}
