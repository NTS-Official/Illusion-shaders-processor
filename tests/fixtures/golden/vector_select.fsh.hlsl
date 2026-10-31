struct FragmentOutput {
    float4 fragColor : SV_Target0;
};

static float4 fragColor = (float4)0;

void main_1()
{
    bool3 mask = bool3(true, false, true);
    float3 a = float3(1.0, 2.0, 3.0);
    float3 b = float3(4.0, 5.0, 6.0);

    float3 _e16 = a;
    float3 _e17 = b;
    bool3 _e18 = mask;
    float3 _e19 = select(_e16, _e17, _e18);
    fragColor = float4(_e19.x, _e19.y, _e19.z, 1.0);
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
    float4 _e3 = fragColor;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e3);
    return fragmentoutput;
}
