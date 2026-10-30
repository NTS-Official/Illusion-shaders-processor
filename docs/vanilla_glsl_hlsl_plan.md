# 原版 GLSL → HLSL 转换链方案（M1 实现版）

本方案是 `illusion_vanilla_glsl_hlsl_shared_architecture_pl.md` 的落地修订版：只覆盖“转换”这一段，不含宿主接入。契约细节见 `glsl_hlsl_conversion_contract.md`。

## 1. 结论先行

原版 26.3 的 72 个着色器里，70 个可以直接转成自包含 HLSL（给定每个 pipeline 的宏），2 个是 naga 能力缺口。达到这一点**没有修改 naga 的 GLSL 前端**，只加了 HLSL 后端的一个选项；主体工作量在 GLSL 方言归一与资源清单，而不是 IR 设计。

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

## 2. 与上游原方案的差异

| 原方案 | 实现后 |
| --- | --- |
| “优先复用 Naga，但不视为不可改边界” | naga 只加一个后端开关 `static_samplers`，前端不动 |
| 方言处理分散在“前端交接契约”里 | 收敛成独立的 `dialect` 层，规则可枚举、可单测（7 条改写，见契约 §3） |
| 绑定模型作为核心抽象 | 先用 manifest v1 承载；偏移与反射留到 M2，避免臆造 cbuffer 布局 |
| 目标 HLSL 由 DXC 校验 | M1 先保证“能生成、能通过 naga 校验”，DXC 编译与反射比对放 M2 |
| 光影包前端同期设计 | 本期不做；双前端仍成立，但共享核心以 manifest 为界 |

## 3. 组成

| 组件 | 位置 | 职责 |
| --- | --- | --- |
| `preprocess` | `src/preprocess.rs` | 递归 include、条件编译、对象式宏、`#version` 归一 |
| `dialect` | `src/dialect.rs` | 7 条 token 级改写；记录 combined sampler / uniform block 的 binding 分配 |
| `lexer` | `src/lexer.rs` | 保空白与注释的 tokenizer，改写后能逐 token 回写 |
| `compile` | `src/compile.rs` | 串联预处理→改写→naga→HLSL，构造 binding map；非 fragment 着色器把隐式 LOD 采样钳到 `SampleLevel::Zero` |
| `manifest` | `src/manifest.rs` | 资源与 stage IO 清单（`illusion.shader-manifest/1`） |
| `shader-convert` | `src/bin/shader-convert.rs` | `convert` / `audit` / `normalize` 三个子命令 |
| naga 补丁 | `naga-for-hlsl/naga/src/back/hlsl/{mod,writer}.rs` | `Options::static_samplers`：每个 sampler 输出为独立 `SamplerState : register(sN)`，不生成 heap |

naga 补丁是本方案唯一的上游改动；不打开该选项时 naga 行为不变（默认 `false`），因此可以按需回推上游。

## 4. 里程碑

| 里程碑 | 内容 | 状态 |
| --- | --- | --- |
| M1 转换器 | 预处理、方言归一、naga 直连、HLSL + manifest、语料审计 | 完成：72 个语料 70 通过；16 个单测通过；`cargo clippy` 只剩 2 条既有 warning（crate 名非 snake_case、`rt_light_contract::next`） |
| M2 校验闭环 | 用 DXC 编译产物（SM 6.0+），拿 reflection 与 manifest 比对；补 buffer texture / PointSize 两个缺口 | 待做 |
| M3 宿主接回 | 替换 `D3D12ShaderCompiler.createIntermediary` 的中间层；根签名改为 static sampler + 显式 cbuffer | 待做（本期禁止改宿主源码） |
| M4 光影包前端 | 独立前端，输出同一套 manifest | 未开始 |

M2 的退出条件：72/72 语料经 DXC 编译成功，reflection 的 `register` 与 manifest 完全一致；`terrain`/`clouds`/`oit_*` 三个代表性 pipeline 的 VS/PS 语义配对正确。

## 5. 验证方式

- 单测：lexer、preprocess、dialect（含 binding 分配、sampler 拆分、const 数组长度）。
- 语料审计：`shader-convert audit <shaders> --defines-file tests/fixtures/corpus-defines.json`；退出码 = 失败数。
- 人工抽查：`convert ... --dump-glsl` 同时落盘归一化 GLSL，用于判断失败是“方言没覆盖”还是“naga 能力缺口”。

语料与 defines 都不进 git 之外的位置：语料取自工作区的反编译源码，defines 是仓库内 JSON，任何一条都能追溯到 `RenderPipelines.java` 的 `withShaderDefine`。

## 6. 风险

| 风险 | 现状 | 处理 |
| --- | --- | --- |
| 能力缺口：texel buffer（`isamplerBuffer`）与 `gl_PointSize` | 2 个语料失败；前者是 naga IR 没有 Buffer 维度，不是前端 bug | M2 做降级改写（2D 视图 / 丢弃 PointSize 输出），或在 fork 里补 IR 维度 |
| 依赖 naga 内部 IR 语义（`AddressSpace`、`SampleLevel`） | 已发生一次（顶点采样钳位） | 把这类适配集中在 `compile.rs`，并在契约里写明；升级 naga 时只需重跑语料审计 |
| HLSL 标识符被 naga 重命名 | `Sampler2` → `Sampler2_` | manifest 记录 IR 名，宿主以 HLSL 声明为准；M2 由 reflection 兜底 |
| cbuffer 布局 | naga 按 std140 插 pad 字段 | 契约要求宿主按 std140 写；M2 用 reflection 验证，不自行推算 |
| 宏集合漂移 | defines 是输入契约 | 每个 pipeline 的宏从 `RenderPipelines.java` 抄；语料审计作为回归 |

## 7. 下一步（按顺序）

1. 在 M2 接 DXC：`dxcompiler.dll` 已随宿主发行，转换器侧只需要一个可选的 `--dxc` 校验开关（外部进程或 FFI，二选一，不引入新依赖）。
2. 补两个缺口：buffer texture 降级、PointSize 丢弃（写进 manifest 的 `warnings`）。
3. 与宿主对齐根签名：static sampler + 显式 cbuffer 寄存器；这需要一份“manifest → root signature”的映射表，放宿主侧实现。
4. 之后再谈光影包前端；它复用本期的 preprocess / manifest，但不复用 dialect（Iris 方言不同）。