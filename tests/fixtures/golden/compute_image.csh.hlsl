RWTexture3D<float4> floodfill : register(u0);
static uint3 gl_GlobalInvocationID_1 = (uint3)0;

void main_1()
{
    uint3 _e2 = gl_GlobalInvocationID_1;
    floodfill[int3(_e2)] = (1.0).xxxx;
    return;
}

[numthreads(8, 8, 8)]
void main(uint3 gl_GlobalInvocationID : SV_DispatchThreadID)
{
    gl_GlobalInvocationID_1 = gl_GlobalInvocationID;
    main_1();
    return;
}
