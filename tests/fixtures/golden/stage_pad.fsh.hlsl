struct FragmentOutput {
    float4 color : SV_Target0;
};

static float2 uv_1 = (float2)0;
static float4 color = (float4)0;

struct FragmentInput_main {
    float dxr_pad0_1 : LOC0;
    float2 uv_2 : LOC1;
};

void main_1()
{
    float2 _e2 = uv_1;
    color = float4(_e2.x, _e2.y, 0.0, 1.0);
    return;
}

FragmentOutput ConstructFragmentOutput(float4 arg0) {
    FragmentOutput ret = (FragmentOutput)0;
    ret.color = arg0;
    return ret;
}

FragmentOutput main(FragmentInput_main fragmentinput_main)
{
    float dxr_pad0_ = fragmentinput_main.dxr_pad0_1;
    float2 uv = fragmentinput_main.uv_2;
    if ((dxr_pad0_ == 1234.5677)) {
        discard;
    }
    uv_1 = uv;
    main_1();
    float4 _e7 = color;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e7);
    return fragmentoutput;
}
