struct Globals {
    float4 Tint;
};

struct FragmentOutput {
    float4 iris_FragData0_ : SV_Target0;
    float4 iris_FragData3_ : SV_Target3;
};

static float4 iris_FragData0_ = (float4)0;
static float4 iris_FragData3_ = (float4)0;
cbuffer global : register(b0) { Globals global; }

void main_1()
{
    float4 _e4 = global.Tint;
    iris_FragData0_ = _e4;
    iris_FragData3_ = (1.0).xxxx;
    return;
}

FragmentOutput ConstructFragmentOutput(float4 arg0, float4 arg1) {
    FragmentOutput ret = (FragmentOutput)0;
    ret.iris_FragData0_ = arg0;
    ret.iris_FragData3_ = arg1;
    return ret;
}

FragmentOutput main()
{
    main_1();
    float4 _e7 = iris_FragData0_;
    float4 _e9 = iris_FragData3_;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e7, _e9);
    return fragmentoutput;
}
