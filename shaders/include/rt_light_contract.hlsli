#ifndef RT_LIGHT_CONTRACT_HLSLI
#define RT_LIGHT_CONTRACT_HLSLI

static const float RT_PI = 3.14159265358979;
static const float RT_INV_PI = 0.31830988618379;
static const float RT_EPS = 1e-6;

uint RtPcgHash(uint v) {
    uint state = v * 747796405u + 2891336453u;
    uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

uint RtSeed(uint2 pixel, uint frameIndex, uint bounce, uint sampleIndex, uint seed) {
    uint h = RtPcgHash(pixel.x ^ RtPcgHash(pixel.y));
    h = RtPcgHash(h ^ (frameIndex * 0x9E3779B9u));
    h = RtPcgHash(h ^ (bounce * 0x85EBCA6Bu));
    h = RtPcgHash(h ^ (sampleIndex * 0xC2B2AE35u));
    return RtPcgHash(h ^ seed);
}

float RtRngNext(inout uint state) {
    state = state * 747796405u + 2891336453u;
    uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return float((word >> 22u) ^ word) * (1.0 / 4294967296.0);
}

float2 RtRngNext2(inout uint state) { return float2(RtRngNext(state), RtRngNext(state)); }

float3 RtCosineHemisphere(float2 u, float3 n) {
    float r = sqrt(u.x);
    float phi = 2.0 * RT_PI * u.y;
    float3 t = abs(n.z) < 0.999 ? normalize(cross(float3(0, 0, 1), n)) : float3(1, 0, 0);
    float3 b = cross(n, t);
    return r * cos(phi) * t + r * sin(phi) * b + sqrt(saturate(1.0 - u.x)) * n;
}

float RtCosinePdf(float cosTheta) { return max(cosTheta, 0.0) * RT_INV_PI; }

float3 RtTangentFrame(float3 n, out float3 t, out float3 b) {
    float3 up = abs(n.z) < 0.999 ? float3(0, 0, 1) : float3(1, 0, 0);
    t = normalize(cross(up, n));
    b = cross(n, t);
    return n;
}

float3 RtSampleGgxVndf(float2 u, float3 n, float3 v, float alpha, out float3 h) {
    float a2 = alpha * alpha;
    float3 t, b;
    RtTangentFrame(n, t, b);
    float3 vLocal = float3(dot(v, t), dot(v, b), dot(v, n));
    float3 vh = normalize(float3(alpha * vLocal.x, alpha * vLocal.y, vLocal.z));
    float lensq = vh.x * vh.x + vh.y * vh.y;
    float3 t1 = lensq > 0 ? float3(-vh.y, vh.x, 0) * rsqrt(lensq) : float3(1, 0, 0);
    float3 t2 = cross(vh, t1);
    float r = sqrt(u.x);
    float phi = 2.0 * RT_PI * u.y;
    float p1 = r * cos(phi);
    float p2 = r * sin(phi);
    float s = 0.5 * (1.0 + vh.z);
    p2 = (1.0 - s) * sqrt(saturate(1.0 - p1 * p1)) + s * p2;
    float3 nh = p1 * t1 + p2 * t2 + sqrt(saturate(1.0 - p1 * p1 - p2 * p2)) * vh;
    h = normalize(float3(alpha * nh.x, alpha * nh.y, max(nh.z, RT_EPS)) + float3(0, 0, RT_EPS));
    h = normalize(t * h.x + b * h.y + n * h.z);
    return reflect(-v, h);
}

float RtGgxD(float3 n, float3 h, float alpha) {
    float a2 = alpha * alpha;
    float nh = max(dot(n, h), 0.0);
    float d = nh * nh * (a2 - 1.0) + 1.0;
    return a2 / max(RT_PI * d * d, RT_EPS);
}

float RtGgxG1(float cosTheta, float alpha) {
    float k = alpha * 0.5;
    return max(cosTheta, 0.0) / max(cosTheta * (1.0 - k) + k, RT_EPS);
}

float RtGgxPdf(float3 n, float3 v, float3 h, float alpha) {
    float nv = max(dot(n, v), RT_EPS);
    return RtGgxD(n, h, alpha) * RtGgxG1(nv, alpha) / (4.0 * nv);
}

float3 RtFresnelSchlick(float cosTheta, float3 f0) {
    float m = pow(saturate(1.0 - cosTheta), 5.0);
    return f0 + (float3(1, 1, 1) - f0) * m;
}

float3 RtGgxBrdf(float3 n, float3 v, float3 l, float3 f0, float alpha) {
    float3 h = normalize(v + l);
    float nh = max(dot(n, h), 0.0);
    float nv = max(dot(n, v), RT_EPS);
    float nl = max(dot(n, l), 0.0);
    float vh = max(dot(v, h), 0.0);
    float3 f = RtFresnelSchlick(vh, f0);
    float g = 0.5 / max(nv * sqrt(alpha * alpha + (1.0 - alpha * alpha) * nv * nv), RT_EPS)
            + 0.5 / max(nl * sqrt(alpha * alpha + (1.0 - alpha * alpha) * nl * nl), RT_EPS);
    return f * RtGgxD(n, h, alpha) * g / max(4.0 * nv * nl, RT_EPS);
}

float RtRayBias(float hitDistance) { return 1e-3 + 0.01 * hitDistance; }

bool RtRouletteSurvive(float3 throughput, inout uint state, out float weight) {
    float q = saturate(max(throughput.r, max(throughput.g, throughput.b)));
    weight = 1.0;
    if (q >= 1.0) return true;
    if (RtRngNext(state) >= q) return false;
    weight = 1.0 / max(q, RT_EPS);
    return true;
}

bool RtAccumulatePdf(float pdf, inout float pdfProduct) {
    pdfProduct *= max(pdf, RT_EPS);
    return pdfProduct > 0.0;
}

#endif