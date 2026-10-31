struct CommonUniforms {
    float gl_Fog_start;
    float gl_Fog_scale;
    int _pad2_0;
    int _pad2_1;
    float4 gl_Fog_color;
    int fogEnabled_value;
    int _end_pad_0;
    int _end_pad_1;
    int _end_pad_2;
};

struct FragmentOutput {
    float4 iris_FragData0_ : SV_Target0;
    float4 iris_FragData1_ : SV_Target1;
};

static bool fogEnabled = (bool)0;
static float4 iris_FragData0_ = (float4)0;
static float4 iris_FragData1_ = (float4)0;
cbuffer global : register(b0) { CommonUniforms global; }
Texture2D<float4> gcolor : register(t1);
SamplerState gcolor_sampler : register(s1);
static float2 texcoord_1 = (float2)0;

struct FragmentInput_main {
    float2 texcoord_2 : LOC0;
};

void main_1()
{
    float4 albedo = (float4)0;
    float fog = (float)0;
    float local = (float)0;

    int _e14 = global.fogEnabled_value;
    fogEnabled = (_e14 != int(0));
    float2 _e17 = texcoord_1;
    float4 _e18 = gcolor.Sample(gcolor_sampler, _e17);
    albedo = _e18;
    float _e20 = global.gl_Fog_start;
    float2 _e21 = texcoord_1;
    float2 _e23 = texcoord_1;
    float _e27 = global.gl_Fog_scale;
    fog = ((_e20 * (_e21.x * _e23.x)) / max(_e27, 0.001));
    float4 _e32 = albedo;
    bool _e33 = fogEnabled;
    if (_e33) {
        local = 1.0;
    } else {
        local = 0.5;
    }
    float _e37 = local;
    iris_FragData0_ = (_e32 * _e37);
    float _e39 = fog;
    float4 _e40 = global.gl_Fog_color;
    float _e42 = fog;
    float3 _e43 = (_e40.xyz * _e42);
    iris_FragData1_ = float4(_e39, _e43.x, _e43.y, _e43.z);
    return;
}

FragmentOutput ConstructFragmentOutput(float4 arg0, float4 arg1) {
    FragmentOutput ret = (FragmentOutput)0;
    ret.iris_FragData0_ = arg0;
    ret.iris_FragData1_ = arg1;
    return ret;
}

FragmentOutput main(FragmentInput_main fragmentinput_main)
{
    float2 texcoord = fragmentinput_main.texcoord_2;
    texcoord_1 = texcoord;
    main_1();
    float4 _e23 = iris_FragData0_;
    float4 _e25 = iris_FragData1_;
    const FragmentOutput fragmentoutput = ConstructFragmentOutput(_e23, _e25);
    return fragmentoutput;
}
