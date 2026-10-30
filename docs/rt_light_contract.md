# `rt_light_contract` 用法

两个文件成对使用：`shaders/include/rt_light_contract.hlsli`（HLSL）与
`src/rt_light_contract.rs`（CPU 镜像，`cargo test` 跑数值）。**改一处必须改另一处**，
否则测试会通过而 HLSL 已经漂移。

## 随机数

```hlsl
uint rng = RtSeed(pixel, frameIndex, bounce, sampleIndex, seed);
float u  = RtRngNext(rng);
float2 uv = RtRngNext2(rng);
```

`RtSeed` 是纯函数：同一 `(pixel, frame, bounce, sample, seed)` 必得同一序列，跨 pass 可复现。

## 漫反射：余弦加权半球

```hlsl
float3 wi   = RtCosineHemisphere(RtRngNext2(rng), n);
float  pdf  = RtCosinePdf(dot(n, wi));   // = max(cos,0)/π
float3 f    = albedo * RT_INV_PI;        // 朗伯 BRDF
// 贡献 = f * Li * dot(n, wi) / pdf
```

## 镜面：GGX VNDF

```hlsl
float  alpha = max(roughness * roughness, 0.002);
float3 h;
float3 wi = RtSampleGgxVndf(RtRngNext2(rng), n, v, alpha, h);
float  pdf = RtGgxPdf(n, v, h, alpha);          // 采样方向密度（含 Smith G1）
float3 f   = RtGgxBrdf(n, v, wi, f0, alpha);
```

`RtSampleGgxVndf` 可能给出**低于表面**的 `wi`（VNDF 的已知行为）：`dot(n, wi) <= 0` 时直接
终止这条路径。所有方向的密度在整个球上归一，只在上半球积分会少约 14%。

## 遮挡与偏移

```hlsl
float bias = RtRayBias(hitDistance);   // 1e-3 + 0.01 * 距离，单位是米
```

沿法线偏移 `bias`，不要用固定常量：远距离的固定偏移会让薄面漏光。

## 终止

```hlsl
float weight;
if (!RtRouletteSurvive(throughput, rng, weight)) return;
throughput *= weight;                   // 存活权重 1/q，期望无偏
float pdfProduct = 1.0;
RtAccumulatePdf(pdf, pdfProduct);       // 每跳乘一次；<=0 视为契约破坏
```

`RtRouletteSurvive` 的 `q = max(throughput)`，`q >= 1` 时恒存活且权重 1。

## 数值测试

```powershell
cd Illusion-shaders-extension
cargo test
```

五个用例：余弦密度归一、GGX 密度归一（整球）、轮盘赌无偏、抽样不跑到表面背面、
`RtPcgHash`/`RtSeed` 的确定性。