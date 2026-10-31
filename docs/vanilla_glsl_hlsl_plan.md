# 原版 GLSL → HLSL 转换链方案（M1 实现版）

本方案是 `illusion_vanilla_glsl_hlsl_shared_architecture_pl.md` 的落地修订版：只覆盖“转换”这一段，不含宿主接入。契约细节见 `glsl_hlsl_conversion_contract.md`。

## 1. 结论先行

原版 26.3 的 72 个着色器全部可以转成自包含 HLSL（给定每个 pipeline 的宏），光影包语料里除 24 个外部宿主程序外的 183 个也全部可转换（含 3 个 compute，UAV 产物已可 verify，宿主侧还没有绑定槽位）。达到这一点不需要自研 IR 或前端：naga 的 GLSL 前端、校验器与 HLSL 后端都在用，只在 naga 里补了六处缺陷/能力（见 §3 最后一行），主体工作量在 GLSL 方言归一与资源清单。

因此本期的架构定为：

```
shaders/ + defines
   → preprocess（include / 宏 / 条件）
   → dialect（保语义的 token 级改写）
   → naga GLSL frontend
   → naga validator
   → naga HLSL backend（static samplers）
   → HLSL + manifest.json
```

发布形态是 `illusion_shader.dll` 的 C ABI（JSON 进、JSON 出，契约见 `shader-convert-abi.md`）；`shader-convert` 命令行只用于开发与语料审计，不进发布物。

## 2. 与上游原方案的差异

| 原方案 | 实现后 |
| --- | --- |
| “优先复用 Naga，但不视为不可改边界” | naga 复用为主，只补六处必要改动：后端 `static_samplers` 选项、`SampleLevel::Zero` 与向量 `select` 的输出、向量 `abs` 的 `select` 形态、GLSL 前端入口函数丢 `local_variables`、校验器对未被引用的表达式过严 |
| 方言处理分散在“前端交接契约”里 | 收敛成独立的 `dialect` 层，规则可枚举、可单测（19 条改写，见契约 §3） |
| 绑定模型作为核心抽象 | manifest v1 直接承载寄存器、`space`、cbuffer `size` 与成员 `offset`，宿主按它装配；不引入独立绑定模型 |
| 目标 HLSL 由 DXC 校验 | 已实现：`verify` 用 DXC 编译并比对资源、入口签名与 cbuffer 布局（见 `verification.md`） |
| 光影包前端同期设计 | 独立前端已落地（`--uniform-block` / `--legacy-builtins` 等），输出同一套 manifest；双前端共享核心以 manifest 为界 |

## 3. 组成

| 组件 | 位置 | 职责 |
| --- | --- | --- |
| `preprocess` | `src/preprocess.rs` | 递归 include、条件编译、对象式与函数式宏（含续行）、`#version` 归一 |
| `dialect` | `src/dialect.rs` | 19 条 token 级改写；记录 combined sampler / uniform block / 降级 buffer texture / MRT 的分配；sampler 覆盖 `1D/1DArray/2D/2DArray/2DMS/2DMSArray/3D/Cube/CubeArray` × `float/int/uint` + 比较采样器 |
| `lexer` | `src/lexer.rs` | 保空白与注释的 tokenizer，改写后能逐 token 回写 |
| `compile` | `src/compile.rs` | 串联预处理→改写→naga→HLSL，构造 binding map；非 fragment 着色器把隐式 LOD 采样钳到 `SampleLevel::Zero` |
| `manifest` | `src/manifest.rs` | 资源、stage IO、MRT 与 cbuffer 布局清单（`illusion.shader-manifest/1`） |
| `abi` | `src/abi.rs` | C ABI：`illusion_shader_abi_version` / `illusion_shader_convert` / `illusion_shader_free`；release 产物 `illusion_shader.dll` |
| `verify` | `src/verify.rs` | 把生成的 HLSL 交给 DXC 编译，解析 `-Fc` 汇编里的 `!dx.resources`、DXIL 容器的 `ISG1`/`OSG1` 签名与 `-Zi` 的 `!dx.typeAnnotations`，与 manifest 逐条比对 |
| `shader-convert` | `cli/src/main.rs` | 仅开发用命令行（`convert` / `audit` / `verify` / `normalize`），独立 workspace 成员 `shader-convert-cli` |
| naga 补丁 | `naga-for-hlsl/naga/src/` | ① `back/hlsl/mod.rs` + `writer.rs`：`Options::static_samplers` 把 sampler 输出成独立 `SamplerState : register(sN)`（不生成 heap）；`SampleLevel::Zero` 输出 `SampleLevel(s, uv, 0.0)`；向量条件的 `Select` 输出 `select(a, b, cond)`。② `front/glsl/functions.rs`：入口函数继承全局上下文的 `local_variables`。③ `valid/function.rs`：未被引用的死表达式不再报 `UnvisitedExpression` |

naga 补丁是本方案唯一的上游改动；`static_samplers` 默认关闭时 naga 行为不变，其余四处是缺陷修正，可按需回推上游。

## 4. 里程碑

| 里程碑 | 内容 | 状态 |
| --- | --- | --- |
| M1 转换器 | 预处理、方言归一、naga 直连、HLSL + manifest、C ABI（DLL）、语料审计 | 完成：72 个语料 72 通过；109 条测试通过（95 个单测、4 条命令行选项用例、17 份 HLSL/manifest 快照见 `tests\fixtures\golden`、`tests\deterministic.rs` 钉住同样的输入产出逐字节相同的 HLSL/manifest、3 条 `tests\diagnostics.rs` 断言错误消息带 `文件:行号`、4 条 `tests\robustness.rs` 残缺输入用例要求报错而不是 panic）；`cargo build --release` 只产 `illusion_shader.dll`，CLI 仅 debug 构建；`cargo clippy` 只剩 1 条既有 warning（`rt_light_contract::next`） |
| M2 校验闭环 | 用 DXC 编译产物（SM 6.0+），拿 reflection 与 manifest 比对 | 完成：`verify` 已实现，72 个语料在 SM 6.0 与 6.9 下 DXC 编译通过，register/space/类型族、stage IO（含打包输出）与 cbuffer 成员偏移（`-Zi` 的 `!dx.typeAnnotations`）都与 manifest 一致 |
| M3 宿主接回 | 替换 `D3D12ShaderCompiler.createIntermediary` 的中间层；根签名改为 static sampler + 显式 cbuffer | **shadow + primary 均已落地**：`gui` / `position` / `text` 家族在标题界面视觉正确；世界内把 `shaderConverterPipelines` 留空时 **221 条管线**切到转换器（含 `solid_block`、`cutout_block`、`opaque_particle`、7 个资源的 `item_cutout`(flipY)、天空、云（`CloudFaces` 走 RAW SRV）、`blit_depth`、天气、OIT 系列、12 条地形），片段输入按顶点输出补齐后图集烘制也转正，地形由转换器产物建出 meshlet 变体（两种 VS 形态 + AS 剔除），`CreateGraphicsPipelineState` 失败 0 次、`terrain batch skipped` 0 次。旧链（shaderc + SPIRV-Cross）保留为回退。未切：无按类别跳过 |
| M4 光影包前端 | 独立前端，输出同一套 manifest | **已落地（验收范围）**：Complementary r5.8.1 的 219 个入口文件（216 个 `.vsh/.fsh` + 3 个 `.csh`）带 `--uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json` 为 **195 compiled / 24 failed**——24 个全是外部宿主程序（Colorwheel 12 / Distant Horizons 12）；排除它们后 **183/183**，183 个全部通过 `verify`（SM 6.0 与 6.9，含 3 个 compute 的 UAV 组）。宿主侧带 storage image 的管线按 `storage image` 跳过（绑定层还没有 UAV 槽位）。前端契约与验收口径见 [shader_pack_frontend.md](shader_pack_frontend.md)，语料现状见契约 §6.1 |







