# 原版链路内迁 + 双前端架构：能力与设施探测报告

- 版本：1.0（探测轮，零代码改动）
- 日期：2026-10-27
- 范围：本文件只回答「现在的链条长什么样、要接哪几处、哪里已有、哪里要新建」。**本轮不提交任何代码改动**：`Illusion-shaders-extension\src\lib.rs` 仍是 cargo 模板，`Cargo.toml` 仍只有 `naga = "30.0.1"`，`Illusion\` 与 `MCDR\` 一行未动。
- 证据来源：Minecraft 26.3 反编译源 `_research\mc263-src\`（7,301 个 .java + 完整 assets）、`ref_shader_loader\Iris`（branch `26.1`，HEAD `bff1e69c`）、`input\ComplementaryUnbound_r5.8.1`（408 个着色器文件 / 18,978 行）、本机 naga 30.0.1 源码、DXRMC 现网 Kotlin/native 代码。

---

## 0 结论速览

1. **原版链路今天已经在跑「与目标同构」的 5 段流水线**，只是中间用了 `shaderc → SPIR-V → spvc` 这条中转。要抛弃 SPIR-V，替换面**落在 JVM 侧的一个类**：`D3D12ShaderCompiler.createIntermediary(String, String, ShaderType, resolver)`（`Illusion\src\main\kotlin\dev\nts\expdxr\d3d12\render\shader\D3D12ShaderCompiler.kt:58-59`）。native 侧**零改动**。
2. **native 对流式接口的全部要求就是「HLSL 文本 + 绑定表 + 顶点元素」**：`mcdr_dx12CreateGraphicsPipeline` 内部自己用 DXC 编 `vs_6_9`/`ps_6_9`（`MCDR\src\core\d3d12_device.cpp:5637`、`:5642`，profile 见 `:5100`），根签名按绑定表下标建（`:5659-5679`）。这正是我们本来就打算产出的形状，**解耦面比上一版方案假设的更小**。
3. **两个前端的输入差异极大、核心差异极小**。原版 72 个编译单元是 `#version 330` + 显式 `layout(location)`（287 处）+ 匿名 std140 块（32 个）+ 散装 `sampler2D`（52 处），而 `texture2D`/`gl_FragData`/`ftransform`/`gl_VertexID` 全为 0；光影包是 130/400/430 compat + 约 362 处必须改写的 legacy 构造 + 43 处 `DRAWBUFFERS` + 3 个 compute。**两者撞在同一件小事上：naga 的 glsl 前端不认 `#version 330`，也完全不认 combined sampler。**
4. **需要自己造的只有「前端」和「规范化」，不必重造 IR / 校验 / HLSL 发射**。naga 30.0.1 的 `ir` 2,750 行 + `valid` 9,634 行 + `back\hlsl` 10,402 行是净收益；`front\glsl` 10,633 行是 Vulkan 口味（`parser\builtins.rs` 2,182 行全是 Vulkan 内建），改它比自己写前端更贵。
5. **迁移会顺手消掉 3 处现存的 HLSL 文本补丁**（`frag_main` 纹理翻转、`gl_Position.y` 负号、meshlet 的 `SPIRV_Cross_Output` 改名）。这些补丁存在的原因恰恰是「产物是 spvc 的通用 HLSL」；一旦由我们自己发射，翻转与语义名应该是编译选项，不是正则。

**一句话**：把「GLSL→SPIR-V→HLSL(SM5.1)→文本补丁」换成「双前端→共享 IR→HLSL(SM6.9)」；改动集中在 JVM 的一个适配器 + 模块从零到一；native 不动，最危险的部分（D3D12 PSO、根签名、DXIL 缓存）已经存在且被 149 条管线跑过。

---

## 1 今天的真实链条

### 1.1 原版图形管线（149 条 `RenderPipeline`）

| # | 段 | 实现 | 关键事实 |
|---|---|---|---|
| 1 | 收集源与 include | `_research\mc263-src\net\minecraft\client\renderer\ShaderManager.java`（383 行）`loadConfigs` | `SHADER_PATH="shaders"`、`SHADER_INCLUDE_PATH="shaders/include/"`；`Configs(shaderSources, includeSources, postChains)`；required 失败抛 `RuntimeException("Failed to load required shader programs...")`，optional 只 warn |
| 2 | 交给后端 | `FrontendGpuDeviceCompilePipelineMixin.java:28-36` → `D3D12PipelineBridge.register(pipeline, source)`（`d3d12\backend\D3D12PipelineBridge.kt:31-33`） | 26.3 起 `createDevice` 不再收 `defaultShaderSource`，源只在 `compilePipeline(CreateInfo)` 里到场 |
| 3 | 取源 | `D3D12Device.resolveShaderSource`（`render\adapter\D3D12Device.kt:284`） | `D3D12PipelineBridge.lookupSource(location) ?: defaultShaderSource` |
| 4 | **编译中途站** | `D3D12ShaderCompiler.createIntermediary`（`render\shader\D3D12ShaderCompiler.kt:58-59`） | ①`injectShaderDefines`（`:355`）**文本**注入 `#define`；②`expandShaderIncludes`（`:386`，标记 `// >>> include <id>`，`:409-411`）**文本**展开 include；③`Shaderc.shaderc_compile_into_spv`（`:69`，target env Vulkan 1.2 = `0x402000`，`set_auto_bind_uniforms(true)`、`set_auto_map_locations(true)`，`:30-33`）；失败把真送编译器的源码落盘 `dxrmc\glsl_<file>.<stage>.<tid>.failed.glsl` |
| 5 | 绑定重编号 | `D3D12IntermediaryShaderModule.rebind`（`D3D12IntermediaryShaderModule.kt:114-140`） | 直接改写 SPIR-V 二进制的 `Binding` decoration = `entries` 下标；输入按顶点属性名重编 location；残留未匹配抛 `"Shader expects uniform buffers which are not being provided: ..."` |
| 6 | 发 HLSL | 同文件 `toHlsl`（`:161` 起） | spvc HLSL 后端，**`SPVC_COMPILER_OPTION_HLSL_SHADER_MODEL = 51`**；额外开 `HLSL_POINT_SIZE_COMPAT`、`HLSL_POINT_COORD_COMPAT`（P6：`debug_point` 必需）；顶点输入注 D3D12 语义（0→`POSITION`，其余→`TEXCOORD<n>`） |
| 7 | **文本补丁 ×3** | `D3D12ShaderCompiler.kt:188-196` / `:212-219` / `:252-272` | ①needle `"void frag_main()\n{"` 后插 `texCoord.y = 1.0f - texCoord.y;`（仅 `lightmap` 且非 flipY）；②同 needle 用于 `core/screenquad`；③正则 `(?m)^([ \t]*gl_Position\s*=\s*[^;]+;)$` 追加 `gl_Position.y = -gl_Position.y;`。找不到只打 `"[FIX] WARN: ... needle not found"` |
| 8 | 建 PSO | `D3D12Device.kt:432` → `d3d12_device.cpp` | native 用 **DXC** 编 `vs_6_9`/`ps_6_9`（`compileHlslToDxil`，`:5637`/`:5642`）；DXIL 磁盘缓存键 = FNV-1a 64(dxc 版本 + profile + entry + HLSL 源)（`:1634`、`:1959-1971`）；根签名 = CBV root descriptors + 1 个 SRV table + static samplers，`reg = 条目序号`（`:5659-5679`） |
| 9 | 缓存 | `D3D12Device.shaderCache`（`:98`） | `ShaderCompilationKey`（`:836`）**没有 equals/hashCode ⇒ 身份语义 ⇒ 永远 miss**；真正生效的去重是 native 的 DXIL 磁盘缓存 |

> 第 4 段是唯一与官方实现分叉的地方：官方 `com\mojang\renderpearl\frontend\shaders\GlslCompiler` 用 `add_macro_definition`（宏不是文本）与 `set_include_callbacks`（include 由 `ShaderSource::getInclude` 交付 `CachedIncludeSource`，见 `renderpearl\api\pipeline\ShaderSource.java`）——DXRMC 改成了文本注入/展开。

### 1.2 原版后处理链

- 5 个 `post_effect\*.json`（`blur` 4,842 B / `creeper` 1,798 / `entity_outline` 2,672 / `invert` 1,266 / `spider` 11,364）+ `post\` 9 个 shader。
- **块布局由 JSON 声明，GLSL 只是同名重声明**：`post_effect\blur.json` 里 `"BlurConfig": [{name: BlurDir, type: vec2, value: [1,0]}, {name: Radius, type: float}]`，而 `assets\minecraft\shaders\post\box_blur.fsh:13-16` 写的是 `layout(std140) uniform BlurConfig { vec2 BlurDir; float Radius; };`；输入采样器在 JSON 里叫 `In`，GLSL 里是 `uniform sampler2D InSampler;`（`:6`），`PostChain.java:102` 用 `input.samplerName() + "Sampler"` 拼出这个名字。
- `PostChain.java:95-111` 在运行时逐 pass 建 `RenderPipeline`，`BindGroupLayout` 由 JSON 拼（`<sampler>Sampler`、`SamplerInfo`、`<uniformGroup>`）⇒ **后处理不需要新机制，它走同一条 `compilePipeline` 接缝**。DXRMC 侧没有任何 post-chain 专用代码（Kotlin 里 grep `PostChain|post_effect` 0 命中），说明今天是靠通用路径自然吃下的。

### 1.3 mesh / meshlet 变体

- `D3D12Device.createMeshVariant`（`:500-589`）把地形 VS 的 **HLSL 文本**交给 `D3D12MeshletShader.build`（345 行，**文本解析 + 重写**）生成 `ms_main`/`as_main`，再 `D3D12CompileShaderDxc(..., "ms_6_9"/"as_6_9")`；PS 复用原管线片元（`:507`，`ps_6_0`）。
- 这是仓库里「文本处理着色器」最重的一处，也是迁移后**最该被模块接管**的一处（模块直接发 mesh 兼容 HLSL，就不需要再正则拆 VS）。
- 另一处同类：`D3D12MeshletShader.kt:350-366` 靠 uniform needle（`CameraBlockPos`/`CameraOffset`/`ModelViewMat`/`ProjMat`）定位 cbuffer 成员。

### 1.4 自研 compute（4 处内联 HLSL 字符串）

| 位置 | profile | 内容 |
|---|---|---|
| `d3d12\backend\D3D12Backend.kt:577-585` | `cs_6_8` | `RWTexture2D<float4> gOutput` + `cbuffer Params` |
| `d3d12\backend\D3D12Backend.kt:661-677` | `cs_6_8` | `gDepth`/`gMotion`/`RWTexture2D<uint> gOut` |
| `d3d12\render\advanced\vrs\D3D12ShadingRateImage.kt:420` | `cs_6_8` | VRS 生成 |
| `dxrmc\sr\D3D12SrMask.kt:201` | `cs_6_0` | SR mask |

⇒ 仓库里**已经存在「手写 HLSL 直通」的先例**，这正是光追 shader 的形态；模块把它当第三类输入（HLSL → DXIL + 可选反射）即可，不必为它设计 GLSL 前端。

---

## 2 目标形态：双前端 + 共享核心

```
   vanilla GLSL 330 ──▶ 前端 V（vanilla 约定）
   + PipelineDecl        · #version 330 / #extension 剥除
   (bindGroupLayouts,    · <minecraft:xxx.glsl> include 解析
    ShaderDefines,       · 匿名 std140 块（裸成员名）
    vertexFormat)        · 散装 sampler2D → 分离 sampler
                         · 显式 location 保留 / 缺失即报错
                                   │
   pack 目录树 ────────▶ 前端 P（pack 约定）
   (Iris 语义)            · Iris 六步预处理（include 先于宏）
                          · 130/400/430 compat → core
                          · legacy 内建 → 注入变量/函数
                          · DRAWBUFFERS/RENDERTARGETS → MRT 槽
                          · 阶段宏包装（同一 .glsl 编两次）
                                   ▼
              ┌──────────────────────────────────────────┐
              │ 共享核心（与方言无关）                   │
              │  · IR（选型见 §6）                       │
              │  · std140 布局计算 / 绑定规划            │
              │  · 接口匹配（VS out ↔ FS in，按 location）│
              │  · HLSL 发射（SM 6.9，register/space 可控）│
              │  · 反射 JSON / 诊断 / 缓存 / C ABI       │
              └───────────────┬──────────────────────────┘
                              │ HLSL + plan.json
                              ▼
                 JVM 适配器（D3D12ShaderCompiler 的替换体）
                              │ HLSL 文本 + 绑定表 + 顶点元素
                              ▼
                 native：DXC → DXIL → PSO（零改动）
```

### 2.1 输入契约必须扩宽（本轮最重要的架构结论）

今天模块的输入是一个 `String`。但原版前端的正确性依赖三样**不在 GLSL 文本里**的东西：

1. `pipeline.bindGroupLayouts`（`BindGroupLayouts.java` 里的块名/采样器名/类型，101 行）——GLSL 声明必须与之同名，否则官方会抛 `"Unable to find shader defined uniform (%s)"`（`PipelineBuilder` 里消费反射的那段，`:277`）。
2. `ShaderDefines`（record：`Map<String,String> values` + `Set<String> flags`）——注意**有值宏**（`OIT_WAVELET_RANK=2`、`OIT_COEFF_COUNT=...`）与**标志宏**两类，`withShaderDefines` 在 149 条管线里出现 92 次。
3. `pipeline.vertexFormatBindings`（`VertexFormat` 元素名 → location/offset/format）——VS 输入必须按元素名对齐，`PipelineBuilder:110-114` 用 `VertexFormatElement.name()` 配对。

⇒ `createIntermediary(source, type, resolver)` 这个签名不够了；需要 `create(source, stage, dialect, defines, vertexFormat, bindGroupLayouts, options)`。**这是 additive 改动**，宿主只改一个调用点。

### 2.2 输出契约

沿用上一版方案的 `plan.json`（HLSL + io + bindings + cbuffers + diagnostics），补两个原版特有的字段：

- `comparison`（`sampler2DShadow` → `SamplerComparisonState`，M1 已验证 `.SampleCmp` 可用）；
- `registerBase`/`space`：**今天由宿主决定**（`reg = entries` 下标，`d3d12_device.cpp:5664`），模块发的 register 只能是提示；权威映射是宿主 reflection + 名字对齐。上一版方案 §6.1 已改为「binding 必写但数值不权威」，本轮结论不变。

---

## 3 SPIR-V 今天替我们承担的职责，谁来接

| # | 职责 | 今天的实现者 | 替换者 | 风险 |
|---|---|---|---|---|
| 1 | GLSL 语法/类型/重载/隐式转换 | shaderc（glslang） | 自有前端（或 naga fork） | **高**——最贵的一块 |
| 2 | 预处理（宏、条件、include） | shaderc 宏定义 + DXRMC 文本展开 | 自有预处理器（pack 侧本来就要） | 中（已有设计） |
| 3 | 反射：stage IO / descriptor / push constant | spvc `SpvModule.Reflection`（`SPIRVModule.java` 306 行） | 自有反射（AST 上直接拿，比改 SPIR-V 二进制干净） | 低 |
| 4 | VS→FS 接口匹配（按 location、类型、插值） | `PipelineBuilder.generateBackendCreateInfo` 读反射 | 模块内做同一条规则，宿主只校验 | 低 |
| 5 | 顶点属性 → VS 输入配对（元素名 + 格式兼容） | 同上（`:110-126`） | 模块内做 | 低 |
| 6 | 绑定编号（首个出现顺序 + 跨 stage 一致 + set=0） | 同上（`:280-296`） | 模块内做，输出到 plan.json | 低 |
| 7 | HLSL 发射（`SV_Target`/`SV_Position`/`nointerpolation`/`SampleCmp`/`Load`/`row_major`/`mul` 顺序） | spvc `back\hlsl` | naga `back\hlsl`（10,402 行）或自研 | **中**——`mul` 操作数翻转、采样器排列是实测过的坑 |

**额外一条（迁移的红利）**：今天 3 处文本补丁（§1.1 第 7 段）之所以存在，是因为 spvc 发的是「通用 HLSL，名字与坐标约定与我们无关」。自己发射后，翻转 = 编译选项、`gl_Position` 的 Y 处理 = 发射时决定、入口名 = 我们定（native 的 `entry` 参数继续传 `"main"` 即可）。**这三处补丁可整体删除**，不必保留 needle 兼容。

---

## 4 两个前端各要吃什么（语料实测）

### 4.1 原版前端（`assets\minecraft\shaders\`，89 个文件 = 63 core + 9 post + 17 include）

| 构造 | 出现次数 | 说明 |
|---|---|---|
| `#version 330` | **72 / 72** | 全部编译单元统一，无其它版本 |
| `#extension GL_ARB_separate_shader_objects : require` | 72 | 需要剥除或忽略 |
| `#include <minecraft:xxx.glsl>` | **150** | 尖括号 + `minecraft:` 命名空间；17 个 include 片段本身无 `#version` |
| `layout(location = N)` | **287** | 全部显式，无隐式分配 ⇒ 前端 V 不需要分配逻辑 |
| `layout(std140) uniform X { ... };` | **32** | 全是**匿名块 + 裸成员名**（如 `include\terrainglobals.glsl:4-7` 的 `TerrainUniform`） |
| 散装 `uniform sampler2D X;` | **52** | **无 binding**，且是 naga 的死穴（§6.4） |
| `samplerBuffer` | 1 | `CloudFaces`（`GpuFormat.R8_SINT`），texel buffer |
| `texelFetch` | 14 | |
| `gl_FragCoord` / `discard` / `flat` / `gl_FrontFacing` / `gl_PointSize` | 24 / 21 / 2 / 1 / 1 | |
| `texture2D` `gl_FragData` `ftransform` `gl_VertexID` `gl_InstanceID` `gl_Layer` `noperspective` `centroid` | **0** | 现代写法，无 legacy 负担 |

- 多 draw 变体靠宏：`core\terrain.vsh` 的 `#ifndef MULTIDRAW_TERRAIN`，多出 `layout(location=4) in ivec3 ChunkPosition;`、`layout(location=5) in float ChunkVisibility;`。
- 结论：**前端 V 是一个窄而规整的 GLSL 330 core 子集**，真正的难点只有 ①`#include` 的命名空间解析、②散装采样器、③匿名块的裸成员名。

### 4.2 光影包前端（`input\ComplementaryUnbound_r5.8.1`，408 文件 / 18,978 行）

| 构造 | 次数 | 备注 |
|---|---|---|
| `#version 130` / `400 compatibility` / `430 compatibility` | 213 / 3 / 3 | compat profile 与 compute 都已出现 |
| `#extension`（image_load_store / SSBO / gpu_shader5 / packing） | 6 | 不再是 0 |
| `#include` / `#define` / `#ifdef` / `#if` / `#elif` | 790 / 1,209 / 895 / 644 / 136 | 阶段宏包装：一个 `.glsl` 被 `#define FRAGMENT_SHADER`、`#define VERTEX_SHADER` 编两次 |
| legacy 内建：`gl_ModelView*` 18 / `gl_ProjectionMatrix` 5 / `gl_NormalMatrix` 22 / `gl_Vertex` 17 / `gl_Color` 20 / `gl_MultiTexCoord*` 27 / `gl_TextureMatrix` 30 / `ftransform` 23 | 162 | 加上 `texture2D*` 118 + `gl_FragData` 66 + `attribute` 16 = **约 362 处必须改写** |
| `DRAWBUFFERS` / `RENDERTARGETS` | 43 / 5 | 注释指令；`RENDERTARGETS: 0,5,4,19` 出现 ⇒ colortex 上限确实大于 8（Iris 侧 `MAX_COLOR_BUFFERS = 32`） |
| compute（`shadowcomp.csh` ×3） | 3 | 每维度一个；配套 `shared` 2、`barrier(` 1、`imageStore` 9、`std430` 2、`gl_WorkGroupID` 1 |
| `uniform sampler2D` / `sampler2DShadow` / `usampler*` | 30 / 2 / 4 | |
| 现代写法（`layout(` / `gl_FragColor` / `varying` / `std140`） | 6 / 0 / 0 / 0 | 包本身不用现代语法 |

- 另有 26.3 专有、参考仓（Iris 26.1）**不认识**的东西：`clrwl_*`（每维度 8 文件）、`voxy_*.glsl` + `voxy.json`、`colorwheel.properties`、`shaders\pack.json`（全仓 grep 0 命中）⇒ 必须设计为「未知程序 → 跳过 + 诊断」，不崩、不臆造。
- 许可证：`Complementary License Agreement 1.6`，明确禁止再分发与改代码 ⇒ `input/` 永不入库（已 gitignore）。

### 4.3 两个前端的交集

| 能力 | 前端 V | 前端 P | 能否共用 |
|---|---|---|---|
| 词法 / 预处理器 | include（无宏展开需求） | include + 宏 + 条件 + 阶段选择 | **共用一份**（V 只用其中一部分） |
| 类型系统 / 表达式 / 语句 | GLSL 330 core | GLSL 130–430 compat 子集 | **共用一份**（同一门语言，只是版本门与内建表不同） |
| legacy 内建 | 不需要 | 需要（注入预置声明，不改语法） | 仅 P 启用（可开关的 prelude） |
| 匿名 std140 块 | 需要（32 个） | 无 | 共用 |
| 散装 sampler → 分离 | 需要（52 处） | 需要（30 处） | **共用**（同一段重写逻辑） |
| location 分配 | 不需要（全显式） | 需要 | 仅 P 启用 |
| MRT / DRAWBUFFERS | 不需要 | 需要 | 仅 P 启用 |
| compute / image / SSBO | 不需要（无 `.csh`） | 需要（3 个） | 仅 P 启用 |
| 反射 / 绑定规划 / HLSL 发射 | 需要 | 需要 | **完全共用** |

⇒ 「两个前端」实际是**一套前端 + 两个 profile**（版本门、内建 prelude、是否分配 location、是否吃注释指令）。落地成本比「写两套前端」低一个量级，**建议按「一套前端 + 两个 profile」实现**。

---

## 5 设施盘点：已有 / 需新增 / 需改

### 5.1 模块内（`Illusion-shaders-extension\`）

| 能力 | 现状 | 判定 |
|---|---|---|
| Rust crate 骨架 | `Cargo.toml` 只有 `naga = "30.0.1"`（**0 个 feature**），`src\lib.rs` 210 B 模板 | 需改：加 feature、拆 crate |
| 方案文档 | `docs\minecraft_glsl_to_hlsl_rust_technical_plan.md` 1,133 行（M1 验证已固化） | 已有，但**本轮转向后需修订**（见 §9） |
| 验证样本 | `docs\spike\composite5.frag` 2,456 B、`docs\spike\gbuffers_terrain.vert` 1,665 B；glslang / spvc / dxc 三段实测通过 | **已有**（可作回归基线） |
| 方言规范化设计 | 上一版 §4 的 R1–R11 | 设计已有，代码无 |
| vanilla profile | — | **需新增** |
| 预处理器 | — | **需新增**（pack 侧本来就要；V 侧只需 include） |
| IR | — | 需选型（§6） |
| HLSL 发射 | — | 需选型（§6） |
| 反射 / 绑定规划 | 设计已有（plan.json） | 代码无 |
| C ABI | 设计已有（6 函数 + SxBytes/SxHandle） | 代码无 |
| 缓存 | 设计已有（分层键） | 代码无 |
| CLI（`sx-cli`） | 设计已有 | 代码无；**离线批量编译是语料回归的前提** |
| 语料治理 | `docs/corpus.lock` 设计已有 | 代码无；`input/` 已 gitignore |

### 5.2 宿主 JVM（`Illusion\`）

| 文件 | 行数 | 判定 |
|---|---|---|
| `render\shader\D3D12ShaderCompiler.kt` | 402 | **需改**：`createIntermediary` 里的 shaderc/spvc 段换成模块调用；`injectShaderDefines`/`expandShaderIncludes` 可退休（模块自己做）；3 处文本补丁删除 |
| `render\shader\D3D12IntermediaryShaderModule.kt` | 383 | **需改 / 大幅缩减**：`rebind` 与 `toHlsl` 的职责下沉到模块；保留「模块输出 → `D3D12CompiledShader`」的转换 |
| `render\shader\D3D12CompiledShader.kt` | 16 | 保留（`hlsl` + `entries` 载体） |
| `render\shader\D3D12FallbackShaderSource.kt` | 76 | 保留（P30 回退源，与翻译无关） |
| `backend\D3D12PipelineBridge.kt` | 31 | 保留（唯一注册接缝） |
| `mixin\...\FrontendGpuDeviceCompilePipelineMixin.java` | 34 | 保留 |
| `mixin\...\GameRendererPreloadUiShaderMixin.java` / `ShaderManagerRequiredFailureMixin.java` | 30 / 51 | 保留 |
| `render\adapter\D3D12Device.kt` | 大文件 | **需改一处**：`compilePipeline` 段落（约 `:380-480`）把 `bindGroupLayouts`/`shaderDefines`/`vertexFormatBindings` 一起交给模块 |
| `render\advanced\mesh\D3D12MeshletShader.kt` | 345 | 可选：迁移后可退役（模块直接发 mesh HLSL）；保留则继续吃 `vs_6_9` 的 HLSL 文本 |
| **新增** 模块加载器（FFM） | — | **需新增**：必须独立于 `MCDR_ABI`（不共享 ARENA/LINKER 状态），DLL 位置自定（建议 `dxrmc\sx\`），照抄 `MCDR_ABI.kt:32-33`/`:277-278`/`:419-424` 的模式 |
| **新增** 配置项 | — | **需新增**（AGENTS.md 硬要求）：`IllusionConfig` 字段 + `D3D12Settings.Snapshot` 参数/属性/`build()` 三处；禁止环境变量入口 |
| **新增** 语言键 | — | `assets\dxr_mc\lang\en_us.json` / `zh_cn.json` 各加键 |

### 5.3 native MCDR

| 能力 | 现状 | 判定 |
|---|---|---|
| HLSL → DXIL | `mcdr_dx12CompileShaderDxc(source, len, entry, profile, out, cap)`（`MCDR\include\mcdr_abi.h:1242-1244`；JVM 封装 `MCDR_ABI.kt:2218`） | **已有，零改动** |
| 图形 PSO | `mcdr_dx12CreateGraphicsPipeline` + `vs_/ps_` + `smProfile = "6_9"` | **已有，零改动** |
| 根签名 / 绑定 | 按绑定表下标（`reg = 条目序号`，`d3d12_device.cpp:5664`） | **已有，零改动** |
| DXIL 磁盘缓存 | FNV-1a(dxc 版本 + profile + entry + 源码) | **已有，零改动** |
| mesh / AS | `mcdr_dx12CreateMeshPipelineWithRootSignature` + `ms_6_9`/`as_6_9` | **已有，零改动** |

⇒ **MCDR 侧结论：一行不用改。** 若模块想自己编 DXIL（不经宿主），`dxcompiler.dll` 已随 JAR 分发（`MCDR_ABI.kt:29` 注释说明 Agility 619 + DXC 是硬依赖），也不构成对 MCDR 的依赖。

### 5.4 三个「必须新建但有现成范本」的东西

1. **FFM 加载器**：范本 = `MCDR_ABI.kt`（`Arena.ofShared` `:32`、`Linker.nativeLinker()` `:33`、`extractLibrary()` `:277`、`downcallHandle` `:419-424`、调用点 `Arena.ofConfined().use { ... }` `:2178-2197`）。**必须独立 object**，不改 `MCDR_ABI`。
2. **配置开关**：范本 = `D3D12SetReflexFrameLimit` / `D3D12SetPIXDirectory` 这类「设备创建前推给 native」的专用 ABI 函数模式（AGENTS.md 明确规定）。模块开关若只在 JVM 侧生效（不需要 native 感知），成本就是一个 `if`。
3. **DLL 分发**：模块 DLL 不进 `vendor/`（那是三方 SDK 的位置），走 `gradlew copyNative` 同类任务即可；运行期位置建议 `<user.dir>/dxrmc/sx/`（与 `MCDR.dll` 同目录树，但不共享提取器）。

---

## 6 naga 改造面：三个选项与推荐

### 6.1 规模实测（naga 30.0.1）

| 部件 | 行数 | 与我们的关系 |
|---|---|---|
| `front\glsl`（含 `parser\builtins.rs` 2,182） | **10,633** | Vulkan 440/450/460 口味；**不认 `#version 330`、不认 combined sampler** |
| `front\wgsl` | 13,948 | 不用 |
| `front\spv` | 7,842 | 抛弃 SPIR-V 后不用（可留作逃生舱） |
| `back\hlsl` | **10,402** | **净收益**：`SV_Target*`、`SampleCmp`、`Load`、`row_major`、`mul` 顺序都已被上游验证 |
| `ir` | 2,750 | 净收益（sampler 与 texture 分离，恰好适配 D3D12） |
| `valid` | 9,634 | 净收益（类型/控制流/UAV 校验） |
| 全仓 `src` | 110,125 | |

已复现的上游缺陷（会直接影响我们）：`naga = { features = ["glsl-in","hlsl-out"] }` **单独无法编译**——`src\front\mod.rs` 把 `mod interpolator` 门控在 `spv-in|wgsl-in`，而 `front\glsl\variables.rs:467` 调用 `apply_default_interpolation` ⇒ `E0599`。加 `wgsl-in` 可编译。**fork 的第一件事就是修这个 cfg。**

### 6.2 三个选项

| 选项 | 做法 | 成本 | 风险 | 适配度 |
|---|---|---|---|---|
| **A. fork naga 全套** | 改 `front\glsl` 吃 330/compat + combined sampler | 改 10.6k 行里的类型系统与内建表 | 高（与上游持续分叉） | 差：等于把 Vulkan 前端改成 GL 前端 |
| **B. 自有前端 → naga IR**（推荐） | 自己写 lexer/preprocessor/parser/语义 → `naga::Module`；`valid` + `back\hlsl` 打小补丁 | 前端 4–6k 行（两 profile 共用） | 中（前端全责，但 IR/发射有保障） | **好**：正合「核心通用、前端分方言」 |
| **C. 全自研** | 自有 IR + 自有 HLSL 发射 | 20k+ 行 | 高（要重造校验器） | 差：放弃 22k 行已验证资产 |

### 6.3 推荐：B，且 fork 面收敛到 3 处

1. **版本门**：`front\glsl` 的 `#version` 只接受 440/450/460（否则 `ErrorKind::InvalidVersion`）；放开到 `330`/`400`/`430` 并接受 `compatibility` profile。改动点很小但必须改（否则原版 72 个文件全灭）。
2. **combined sampler**：`front\glsl\parser\declarations.rs:444-450` 的 `NotImplemented("variable qualifier")`。两条路：
   - 前端在**规范化阶段**把 `uniform sampler2D X;` 拆成 `layout(binding=b) uniform texture2D X_tex; layout(binding=b+1) uniform sampler X_smp;`，并把 `texture(X, uv)` 改成 `texture(sampler2D(X_tex, X_smp), uv)`——**纯文本可做**，前提是采样器不作为函数参数传递；
   - 若出现 `void f(sampler2D s)` 这类参数传递，则必须在 AST 上做「采样器参数拆成两个参数」的变换（成本增加，但仍是前端职责，不动 naga）。
3. **texel buffer**：原版有 1 处 `samplerBuffer`（`CloudFaces`）——需确认 naga glsl 前端是否支持 `samplerBuffer`（`front\glsl` 里 image 走 `map_image_format`，texel buffer 是另一条路）。若不支持，**这是前端 V 的硬阻塞**，必须 M0 阶段就探明。

### 6.4 待验证清单（M0 探针，8 条，全部可离线跑）

| # | 探针 | 通过判据 |
|---|---|---|
| P1 | naga `glsl-in` 接受 `#version 330`（打补丁后） | 原版 `terrain.vsh` 能解析 |
| P2 | 匿名 std140 块 + 裸成员名 | `TerrainUniform` 的成员可直接引用 |
| P3 | `layout(std140) uniform X {...}` **无** `binding` 时的行为 | 是否仍强制要求 `layout(binding=)`（上一版实测：所有 uniform 都强制要 binding） |
| P4 | `samplerBuffer` / texel buffer | 原版 `CloudFaces` 能表达 |
| P5 | 分离 texture+sampler → HLSL 发的是 `register(sN)` 静态采样器，还是 `nagaSamplerHeap[...]` 堆查表 | 若是堆（`back\hlsl\help.rs:2027-2053`、`back\hlsl\writer.rs:1224-1263`），必须打补丁——**这是 D3D12 静态采样器根签名的硬冲突** |
| P6 | `sampler2DShadow` → `SamplerComparisonState` + `SampleCmp` | M1 在 spvc 侧已验证；naga 侧待验 |
| P7 | compute（`shadowcomp.csh`）：`shared`/`barrier`/`gl_WorkGroupID`/`imageStore`/`std430` SSBO | IR 里有对应物；前端能否解析 compat 430 |
| P8 | `binding_map` / `BindTarget{space, register}` 能按宿主契约指定寄存器 | `back\hlsl\mod.rs` 的 `Options.binding_map` 文档已明确支持，需实测 |

> P5 是最重要的一条：它决定「naga 是否只需薄补丁」。上一版方案曾把采样器堆当成既定结论，本轮降级为**待验证**——M1 的 spvc 产物是静态 `register(sN)`，naga 是否同样要看 P5。

---

## 7 待拍板的决策点

| # | 决策 | 选项 | 影响 |
|---|---|---|---|
| D1 | 模块定位 | (a) 只做 GLSL→HLSL 翻译库；(b) 兼做 HLSL 直通（光追/自研 compute） | 选 b 则 ABI 加 `dialect = hlsl` + 反射可选，成本很小、收益明确（现有 4 处内联 HLSL 可收编） |
| D2 | 前端实现 | (a) 自研前端 → naga IR（推荐）；(b) fork naga 前端；(c) 全自研 | 决定 M0–M2 的工作量与 fork 维护成本 |
| D3 | 双前端 vs 一套前端两 profile | 建议「一套前端 + 两个 profile」（§4.3） | 影响 crate 划分与测试矩阵 |
| D4 | 原版迁移时机 | (a) 先只接光影包，原版保持 shaderc 链（双轨）；(b) 一次切换 | 选 a 则两套 HLSL 约定共存（补丁要两边都活）；选 b 回归压力大但收敛快 |
| D5 | DXIL 谁来编 | (a) 宿主调 `MCDR_ABI.D3D12CompileShaderDxc`（零改动）；(b) 模块自己调 `dxcompiler.dll` | 选 a 保持解耦、复用 native 磁盘缓存；选 b 模块可独立跑 CLI 全链路 |
| D6 | flipY / 语义名 | 模块选项（推荐）还是保持宿主补丁 | 选前者可删 3 处正则；选后者要维持 `frag_main` 等 needle 兼容 |

---

## 8 风险与不做什么

| # | 风险 | 触发条件 | 缓解 |
|---|---|---|---|
| R1 | 自研前端在 compat 语义上反复 | `#version 130` 的隐式规则（`texture2D` 重载、`gl_FragData` 与 `out` 混用） | 语料驱动：408 文件全部进 golden；`sx-cli` 可离线跑 |
| R2 | naga HLSL 发射与 D3D12 根签名冲突（采样器堆） | P5 若为堆 | 打补丁，或改根签名（后者动 native，优先级低） |
| R3 | 原版 149 条管线回归 | 双轨期两套约定并存 | 先做「模块产物 vs 现网产物」的逐管线 A/B（HLSL 文本 diff + DXIL 反汇编 diff） |
| R4 | 编译并发 | `ShaderManager` 用 `taskExecutor` 并发编译（官方 `GlslCompiler` 用编译器对象池） | 模块必须无全局可变状态；FFM 每次调用 `Arena.ofConfined` |
| R5 | 语料泄漏 | `input/` 已 gitignore，但 `docs/spike/` 的样本来自 Complementary | 现有两个样本是**手写**的（非拷贝），保持这条纪律 |

**不做什么**

- 不为了翻译去改 native（MCDR 零改动是硬约束，§5.3 已论证可行）。
- 不为 compute/光追去扩 naga 的特性面（光追 shader 手写 HLSL）。
- 不在本轮改任何代码（含 `lib.rs`、`Cargo.toml`、JVM/native 任何文件）。

---

## 9 本轮未做的事

1. **没有改方案文档正文**。上一版 `docs\minecraft_glsl_to_hlsl_rust_technical_plan.md`（1,133 行）仍以「M1 = 规范化 → glslang → spvc」为基线；本轮结论（弃 SPIR-V、双前端、原版内迁）需要下一轮改写它的 §3 / §6 / §10。建议把本文件作为附件，正文只改「后端选型」「里程碑」「与宿主契约」三章。
2. **没有做 P1–P8 探针**（§6.4）。这是 M0 的第一件事，全部可离线跑，不需要进游戏。
3. **没有量化「自研前端」的真实工作量**。§6.2 的 4–6k 行是估计值；建议 M0 结束时用「原版 72 + 光影包 219 个 stage 入口的解析成功率」替换这个估计。
4. **没有定 D1–D6**。这些需要拍板，尤其 D2（前端路线）与 D4（原版迁移时机）会改变后续所有里程碑。
