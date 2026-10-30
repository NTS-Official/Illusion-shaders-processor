# Minecraft 光影包前端（`Illusion-shaders-extension`）技术方案

**版本：** 3.0（事实基线重写版）\
**日期：** 2026-10-27\
**状态：** 设计定稿，待实现\
**模块位置：** `D:\Projects\MinecraftMod\DXRMC\Illusion-shaders-extension\`
**主体仓：** `Illusion\`（NeoForge / Kotlin / JVM 侧）、`MCDR\`（C++ / MSVC / D3D12 native 侧）\
**参考实现：** `ref_shader_loader\Iris`（Iris 26.1 分支，HEAD `bff1e69cb6c5519d8745784aa9c8b92984de67e7`）\
**语料：** `input\ComplementaryUnbound_r5.8.1`（Complementary Unbound r5.8.1，面向 MC 26.3）

> **证据等级标注约定**：本文所有"事实"都标出处与等级。
> **【A】** = 本机实测（可复现命令写在 §13.2）；**【B】** = 参考仓 / 上游源码逐行核对（给出 `path:line`）；
> **【C】** = 推断，未经实测，文中会明确写"待验证"。凡是没有标注的段落都是设计约束，不是事实陈述。

---

## 0. 本版修订摘要

### 0.1 为什么必须有这一版

v2.0 的三条技术前提在本机全部被证伪。它们不是"实现细节偏差"，而是"地基选错了"，继续按 v2.0 施工会在第三个月撞墙：

| # | v2.0 的主张 | 实测事实 | 等级 | 后果 |
| --- | --- | --- | --- | --- |
| 1 | "改造 `wgpu-mc/cyntax` 使其处理 Minecraft GLSL 子集"，路线 A = `cyntax AST → Naga IR`（首选） | cyntax 是 **C 语言预处理器 + C 声明解析器**（约 4,450 行、8 个 commit、MPL-2.0）；`cyntax_parser\src\decl.rs` 解析 `parse_external_declaration` / `parse_declaration_specifiers` / `StorageClassSpecifier::Typedef`，没有 GLSL 类型系统、没有内建函数、没有 IR lowering | 【A】【B】 | 路线 A 作废；cyntax 只剩"预处理器"这一块可参考 |
| 2 | 集成 Naga 当 GLSL 前端（"Naga 已经能解析 GLSL"） | naga 30.0.1 的 GLSL 前端只接受 **Vulkan 440(partial)/450/460**；`#version 130` → `InvalidVersion(130)`；legacy 内建（`texture2D`/`gl_FragColor`/`gl_ModelViewMatrix`/`ftransform`/`gl_Layer`）在 `front/glsl` 里**命中数为 0**；**不支持 combined sampler**（`uniform sampler2D x;` → `NotImplemented("variable qualifier")`）；**不支持 `#include`**（依赖的 `pp-rs` 没有 include 实现） | 【A】【B】 | Naga 不能当"现成前端"，只能当"我方规范化之后的校验器/后端" |
| 3 | 用 Naga 的 HLSL 后端产出 HLSL，喂给宿主 DXC | naga 29/30 的 HLSL 后端**强制输出 sampler heap**：`SamplerState nagaSamplerHeap[2048] : register(s0, space0);` + `StructuredBuffer<uint> nagaGroup0SamplerIndexArray : register(tN, space255);` + `static const SamplerState texS = nagaSamplerHeap[nagaGroup0SamplerIndexArray[k]];`。这是 wgpu-hal 的 D3D12 模型，**与 DXRMC 的 static-sampler 根签名不兼容**（宿主 `MCDR\src\core\d3d12_device.cpp:5659-5679` 只用 root CBV + 一个 SRV 表 + static sampler） | 【A】【B】 | "纯 Rust 产出 HLSL"降级为 M3 可选后端，且需要 30 行上游补丁或换发射器 |
| 4 | 假设宿主是泛化的 "C/C++ 宿主"，C ABI DLL 是第一交付物 | 宿主是 **JVM**：着色器编译全部在 Kotlin（`Illusion\src\main\kotlin\dev\nts\expdxr\d3d12\render\shader\D3D12ShaderCompiler.kt`），native 只接 HLSL 文本做 DXC 编译；`MCDR\src` 下没有任何 shader 相关文件；且宿主**已经有一条完整的 `GLSL → shaderc → SPIR-V → SPIRV-Cross → HLSL(SM5.1) → DXC(SM6.9)` 链**在跑原版 26.2 的着色器 | 【A】【B】 | C ABI 仍然正确（它是解耦机制），但它不是唯一接缝；GLSL 文本接缝的侵入度为 0 |
| 5 | 把 `DRAWBUFFERS` 当作"包元数据"在早期解析 | 它是**片元源里的注释指令**，且 Iris 在**预处理之后**才采集（`ShaderPack.java:286-320` 的 `sourceProvider` 先 include 拼接过 JCPP，再由 `ProgramDirectives.java:115` 采集）；采集规则是"同种指令取最后一次出现，两种指令之间取位置更后的那条"（`CommentDirectiveParser.java:26-60`、`ProgramDirectives.java:149-161`）；其语义是 **`gl_FragData[i]` 的 location == 槽位 i**，`colortex` 编号只出现在**渲染目标列表**里 | 【B】 | 采集点错了就会读错光影包的 MRT 布局；而且**绝不能在着色器里把 location 改写成 colortex 编号** |

### 0.2 新的定位（一句话）

> 本模块不是"GLSL → HLSL 翻译器"，而是**光影包前端**：把任意 Iris/OptiFine 光影包**解析、规范化成 `#version 450 core` 的 GLSL，并产出一份描述"有哪些 pass、每个 pass 读写哪些 buffer、要什么管线状态、需要宿主具备哪些能力"的 `plan.json`**；HLSL / SPIR-V 是它的可选下游后端（M2 / M3），不是第一交付物。

这不是降级，而是把工程量放在真正稀缺的地方。`vanilla_shader_plan.md` §0.1 的结论是"**翻译不是瓶颈，能力才是**"——宿主已经有 GLSL→HLSL 的翻译链；缺的是"包发现 + 方言规范化 + pass 图"。这三件事恰好是 Minecraft 专属知识，通用工具（Naga / glslang / SPIRV-Cross）一件都做不了：

- 没有任何通用工具认识 `/* DRAWBUFFERS:03648 */`；
- 没有任何通用工具认识"`program\composite.glsl` 是一个被 `#define FRAGMENT_SHADER` 参数化、要编译两次的元文件"；
- 没有任何通用工具认识 `ftransform()` 在合成 pass 里 == `gl_Position = Projection * Vertex`（ModelView 被当单位矩阵）；
- 没有任何通用工具认识 `attribute vec4 mc_Entity` 要由宿主顶点格式的第三个 `ivec2` 属性填。

**而这个"产物形状"是可离线验证的**：§2.7 按本文件的规则手写了等价样本，用本机 Vulkan SDK 的三件工具（`glslangValidator` / `spirv-cross` / `dxc`）一路跑到 `ps_6_9`/`vs_6_9` 的 DXIL，零告警。也就是说 M1 的契约在写代码之前就已经闭环，剩下的未知只有宿主侧的渲染图能力，不在本模块里。

### 0.3 保留 v2.0 的哪些内容

v2.0 的分层流水线、每个阶段独立错误类型、VFS 抽象、宏优先级、诊断分档、ABI 结构体规则、缓存分层、安全清单都是对的，本版保留其形状并替换事实基础（§3、§7-§9 直接沿用并加约束）。被替换的是：§2 目标、§3 路线选择、§7 cyntax 集成、§8 语义映射、整个"产物只有 HLSL"的假设。

---

## 1. 目标、边界与验收

### 1.1 交付物

一个独立的 Rust workspace，产出三类**文本**产物（二进制产物是可选的 M2）：

| 产物 | 形态 | 消费者 | 优先级 |
| --- | --- | --- | --- |
| **A. 规范化 GLSL** | 每个 `(维度, 程序名, 阶段)` 一个 `#version 450 core` 源文件 | 宿主现有 `D3D12ShaderCompiler.createIntermediary`（零改动） | P0 |
| **B. `plan.json`** | 每个包一份：程序清单、pass 图、管线状态、buffer 需求、uniform 块布局、能力表（§5） | 宿主的"包驱动渲染图"（独立路线，不在本模块内） | P0 |
| **C. 诊断** | 结构化 JSON（规则 id / 严重度 / 包内文件与行 / 产物内行） | CLI + 宿主日志 | P0 |
| **D. SPIR-V**（M2） | 每阶段一个 `.spv` | 想要二进制接口的宿主；离线 `spirv-val` | P2 |
| **E. HLSL**（M3） | 每阶段一份 HLSL（SM 6.x） | 跳过 shaderc+spvc 的宿主 | P3 / 决策门 VG8 |

另外提供 `sx-cli`（同名 workspace 的可执行 crate，子命令 `survey` / `normalize` / `compile` / `verify` / `diff`，见 §13.2）：`sx compile --pack <dir> --program composite5 --stage fragment --emit glsl450 --plan`。**CLI 是模块的一等公民**，不是附属品——它让整个模块在没有 Minecraft、没有 GPU、没有 Windows 的机器上可测（见 §9）。

### 1.2 解耦契约（硬规则）

1. **零反向依赖**：模块的 `Cargo.toml` 里不出现任何指向 `Illusion\` / `MCDR\` 的 path 依赖；不含 `DXRMC`、`MCDR`、`dxr_mc`、`D3D12` 字样（除注释里说明"这是给谁用的"）。把 `Illusion-shaders-extension\` 整个目录复制到别处，`cargo test` 必须通过。
2. **零宿主类型**：输入是 `PackSource`（trait：`read(path) -> Option<Vec<u8>>`、`list_dir(path)`）、`EnvironmentDesc`（纯数据）、`Options`（纯数据）。宿主无论是 JVM、C++、还是命令行，都只需实现这两个 trait。
3. **文件访问一律走回调**：模块自己**不读 zip、不读 JAR、不碰 `java.nio`**。宿主（JVM 侧）负责解包，模块只收 `path → bytes`。这样"包在 JAR 里 / 在 zip 里 / 在文件夹里"对模块不可见。
4. **不许改主体**：v1 全部验收都在 CLI 上完成。宿主侧接入的三个级别见 §1.3，全部是**新增文件**，不改既有行为。
5. **可脱库**：模块自带 `Cargo.toml` workspace、`LICENSE`、`README.md`、`.gitignore`（必须忽略 `input/`，见 §2.6）、CI 脚本；将来可以整目录 `git subtree split` 出去。
6. **版本化接口**：`plan.json` 带 `schema`；C ABI 带 `abi_version` + 每个结构体首位 `struct_size`（沿用 v2.0 §9.1）。

### 1.3 宿主侧接触面（按侵入度排序，全部可选、全部 additive）

| 级别 | 宿主改动 | 用途 | 是否属于本模块交付 |
| --- | --- | --- | --- |
| **L-0 零改动** | 无。用 `sx-cli` 离线产出，人工比对宿主 `HLSL 落盘` 的输出 | 开发期验证规范化是否正确 | 是 |
| **L-1 只换源文本** | `Illusion` 新增一个 `ShaderSource` provider（1 个 Kotlin 文件 + 1 处注册），按 `(program, stage)` 返回我们产出的 GLSL | 跑通 L0/L0.5（后处理 + 世界管线换源） | 否（宿主侧工作，见 §6.1） |
| **L-2 消费 `plan.json`** | `Illusion` 新增包驱动渲染图 + colortex 池 + shadow pass | L1（G-buffer 档）/ L2c（compute 档） | 否（这是 `vanilla_shader_plan.md` 的独立里程碑） |

L-1 的可行性已经在本机被证明：宿主的编译入口是纯文本函数
`createIntermediary(filename: String, source: String, type: ShaderType, includeResolver: ...)`（`D3D12ShaderCompiler.kt:58-59`），
且已存在"不经过 GLSL 编译器、直接把内置 HLSL 文本交给 DXC"的通道（`D3D12Backend.kt:428-447` 的 `EMBEDDED_SHADER_SOURCE`）。
所以"换 GLSL 源文本"这件事**不需要动编译器、不需要动 native**。

### 1.4 非目标（本版明确不做）

- 不做渲染图、不做 shadow pass、不做 compute 管线、不做 colortex 池、不做 pass 排序执行 —— 这些是宿主的活（`vanilla_shader_plan.md`）。
- 不改 `MCDR\`（一行都不改；HLSL 文本进 DXC 的既有通道足够）。
- 不做曲面细分（语料 0 个 `.tcs/.tes`；参考实现的 `ProgramSet.java:107-125` 也只在 `tessellation` 特性开启时才读）。
- 不实现 `#moj_import`（Iris 自己显式拒绝，见 §2.2）。
- 不做 shader 编辑/热重载 UI、不做 `.properties` 里的屏幕布局 UI（只解析成数据）。
- 不做"语义等价证明"（v2.0 的 §8.1 提法保留为诊断等级，不作为正确性保证）。

### 1.5 验收标准

| 编号 | 标准 | 判定方式 |
| --- | --- | --- |
| V1 | `ComplementaryUnbound_r5.8.1` 的**全部 219 个 stage 入口**（108 `.fsh` + 108 `.vsh` + 3 `.csh`）都出现在 `plan.json` 里：要么 `status = "ok"/"partial"`，要么 `status = "skipped"` 且带诊断，**不得静默丢失** | `plan.json` 的 `programs[].status` 计数 |
| V2 | 规范化后 GLSL 的**语法可编译率 ≥ 95%**（用宿主同款 shaderc 编译到 SPIR-V，`set_auto_bind_uniforms(true)`、`set_auto_map_locations(true)`、Vulkan 1.2 target） | 离线脚本 + 统计 |
| V3 | 每个产出的 `plan.json` 满足 §5.1 的 JSON Schema，且 pass 图的每条边都能在语料里找到证据 | schema 校验 + 人工抽查 3 个程序 |
| V4 | module 目录整体复制到 `C:\tmp\sx` 后 `cargo test --offline` 全绿 | CI |
| V5 | 同一输入的两次运行产物**逐字节相同**（确定性） | 哈希对比 |
| V6 | 单包全量（408 个着色器文件 / 3 个维度）编译耗时 < 2 s（release，8 核） | bench |

> **VG5 决策门**（原定义见 `vanilla_shader_plan.md`，本版据实测重述，详见 §10）：V2 的成功率若 < 90%，先按 `sx-cli survey` 的频次表补齐出现最多的失败构造；仍不达标则把 L1 降级为非目标、只交付 L0。**本版不再把"引入 Naga glsl-in"当作升级路径**——它连 `sampler2D` 声明都不接受（§2.4），不是更宽的网。

---

## 2. 事实基线

### 2.1 宿主侧（DXRMC 现状，全部实测）

**编译链（JVM）**

```
pack GLSL ─┐
           ├─ injectShaderDefines()        D3D12ShaderCompiler.kt:355
           ├─ expandShaderIncludes()       D3D12ShaderCompiler.kt:386（文本级展开，标记 "// >>> include <id>" :409-411）
           ├─ shaderc → SPIR-V             D3D12ShaderCompiler.kt:69
           │    target env = Vulkan 1.2 (0x402000)，set_auto_bind_uniforms(true)、set_auto_map_locations(true)（:30-33）
           ├─ SPIRV-Cross → HLSL           D3D12IntermediaryShaderModule.kt:185-186（HLSL shader model 51）
           │    + spvc_compiler_hlsl_add_vertex_attribute_remap（:216-221）
           └─ DXC → DXIL → PSO             MCDR\src\core\d3d12_device.cpp:1837/1919-1943/5637-5642（SM 6_9，:5630、:5100）
```

**关键契约（这几条决定了本模块的输出必须长成什么样）**

| 契约 | 实现位置 | 对本模块的约束 |
| --- | --- | --- |
| 绑定按**出现顺序**自动分配：`binding` decoration 被改写为 entries 下标，native 侧 `reg = 条目序号` | `D3D12IntermediaryShaderModule.kt:114-140`、`D3D12Device.kt:811`、`d3d12_device.cpp:5664` | **M1 不依赖** `layout(binding=)` 决定寄存器；但 uniform 块仍要写 `binding`（naga 的硬要求，且 glm/`plan.json` 要与源码一致），采样器在 M1 不写。`plan.json` 的资源顺序必须与源码出现顺序一致 |
| 根签名 = root CBV + 一个 SRV 表 + **static samplers** | `d3d12_device.cpp:5659-5679` | 采样器必须是静态的 → naga 的 sampler heap 不可用（§2.4） |
| 顶点输入 location 被顺序重编号，语义名 `POSITION` / `TEXCOORDn` | `D3D12IntermediaryShaderModule.kt:102`、`:216-221` | 我们输出的 `layout(location=N)` 顺序 = 宿主顶点格式的属性顺序（Position, Color, UV0, UV2 等，见 `vanilla_shader_plan.md` §0.2） |
| HLSL 文本级补丁存在（`void frag_main()\n{`、`gl_Position = ...` 正则、meshlet 入口改名） | `D3D12ShaderCompiler.kt:188-196`、`:212-219`、`:252-272`；`D3D12MeshletShader.kt:48-52` | 这些 needle 依赖 spvc 的输出形态。**走 M3（我们直接出 HLSL）时这些补丁全部失效**，必须由我们的发射器自己承担 |
| `shaderCache` 的 key 没有 `equals`/`hashCode`（`D3D12Device.kt:98`、`:836`） | 每次编译必然 miss | 本模块必须在**自己这边**做缓存（§8），不能指望宿主去重 |
| cbuffer 偏移不在编译层算：vanilla 的 `GpuBufferSlice` 原样透传到 native | `D3D12RenderPassBackend.kt:405-406`、`:429` | 包 uniform 的 std140 布局必须由**本模块**给出（§4.8），宿主只按偏移写字节 |

**FFM 侧**（只影响宿主怎么写调用代码，不影响本模块）：`MCDR_ABI.kt:32-33` 共享 `Arena` + `Linker.nativeLinker()`，`extractLibrary()` 解到 `<user.dir>/dxrmc/MCDR.dll`（`:277-278`、`:322`），全仓无 JNI。本模块的 C ABI 会被同一套 FFM 调用，所以 **ABI 里只能用 C 基本类型 + 指针 + 长度**，不能有 Rust `bool`/`&str`/泛型/回调 trait object。

### 2.2 Iris 26.1 的语义（参考实现，全部 `path:line` 级核对）

**预处理顺序（必须 1:1 复刻，顺序错了 MRT 与宏全错）**

```
① 发现程序文件（按 ProgramGroup 前缀 + 维度目录）
② include 图：FileNode/IncludeGraph/IncludeProcessor    ShaderPack.java:297
   - #include 相对"包含者所在目录"解析（FileNode.java:61）
   - 环检测 (IncludeGraph.java:163-217，"Cycle detected in #include graph")
   - 注释明确指出 include 先于一切其它预处理指令，include guard 无效（IncludeGraph.java:206）
③ JCPP（org.anarres.cpp）：宏展开，KEEPCOMMENTS        ShaderPack.java:317 / JcppProcessor.java:51
   - #version / #extension 被换成 #warning 标记，稍后还原并提升到文件头（JcppProcessor.java:28-29、GlslCollectingListener.java:7-24）
   - 环境 define 通过 pp.addMacro 注入，不写进源文本（保住行号，JcppProcessor.java:42-44）
   - 剥 NUL 字节（:32）
④ #version 归一：< 330 → 330，非 core 一律 CORE       TransformPatcher.java:148-150、:172-175
⑤ 采集注释/const 指令（DRAWBUFFERS / RENDERTARGETS / colortexNFormat...） ProgramDirectives.java:115
⑥ 逐个 transformer 改写（CommonTransformer / VanillaTransformer / CompatibilityTransformer / EntityPatcher ...）
```

**指令采集的精确规则（这是最容易做错的一段，直接照抄语义）**

```java
// CommentDirectiveParser.java:26-60
int indexOfPrefix = haystack.lastIndexOf(prefix);        // 同种指令取最后一次
String before = haystack.substring(0, indexOfPrefix).trim();
if (!before.endsWith("/*")) return Optional.empty();      // 前面必须是 /*（允许 /*DRAWBUFFERS:321*/ 这种无空格写法）
... haystack.indexOf("*/") 之前的全部内容即为 value
```
```java
// ProgramDirectives.java:149-161
if (drawbuffers.isPresent() && rendertargets.isPresent())
    return drawbuffers.getLocation() > rendertargets.getLocation() ? drawbuffers : rendertargets;
// 即：两种指令都出现时，源码位置更靠后的那条生效
```

**其它必须知道的事实**

| 事实 | 出处 |
| --- | --- |
| `colortex` 上限 **32**（`MAX_COLOR_BUFFERS`），legacy 别名 `gcolor,gdepth,gnormal,composite,gaux1..4` = 0..7 | `gl/IrisLimits.java:11`、`PackRenderTargetDirectives.java:21-30` |
| `#moj_import` 被显式拒绝（"isn't currently supported"） | `TransformPatcher.java:86`、`:124-131` |
| gbuffers 回退链是**语义等价类**，不是一个文件 fallback 到另一个文件 | `ProgramId.java:21-57`、`ProgramFallbackResolver.java:28-45`（Terrain→TexturedLit→Textured→Basic 等） |
| 只有 `.fsh` 的包会被合成一个 `#version 120` 顶点着色器 | `ProgramSet.java:127-144` |
| `#version` 声明值参与环境宏 `MC_GLSL_VERSION`，包代码按它分支 ⇒ **必须与 Iris 一致地固定在它实际编译用的版本**（Iris 把 `< 330` 一律提升到 `330 core`，所以报 330）。**绝不能报 450**，否则包会打开我们没规范化过的 4.x 分支 | `common\src\main\java\net\irisshaders\iris\shaderpack\parsing\StandardMacros.java:46-61`、`TransformPatcher.java:148-150`、`:172-175` |
| 注入的 legacy 兼容层：`gl_ModelViewMatrix→mat4(1.0)`（仅 CompositeTransformer）、`ftransform()→gl_ModelViewProjectionMatrix*gl_Vertex`、`gl_FragData[i]→iris_FragDataN + layout(location=i) out`、`gl_MultiTexCoord3→mc_midTexCoord`、`mc_Entity→iris_Entity` | `CompositeTransformer.java:67-78`、`CommonTransformer.java:266-303`、`:116-134`、`EntityPatcher.java:154-166` |
| `shaders.properties` 的键族：`program.*`、`profile.*`、`screen*`、`sliders`、`blend.*`、`alphaTest.*`、`scale.*`、`size.buffer.*`、`texture.*`、`image.*`、`uniform.*`、`variable.*` + 一堆开关 | `ShaderProperties.java:131-625` |
| `RENDERTARGETS` **不是** properties 键，是注释指令；`colortexNFormat/Clear/ClearColor`、`colortexNMipmapEnabled` 是 **const 指令** | `ProgramDirectives.java:54-74`、`:108-111`、`PackRenderTargetDirectives.java:95-124` |
| 本地参考仓是 **26.1**，语料面向 **26.3**：`clrwl` / `colorwheel` / `pack.json` / `voxy` 在参考仓里 **0 命中** | `grep -r` 实测（复现命令见 §13.2） |

### 2.3 语料：Complementary Unbound r5.8.1（实测）

```
input\ComplementaryUnbound_r5.8.1\
  shaders\
    world0\  76 文件 = 36 组 .vsh/.fsh（72）+ shadowcomp.csh + voxy_opaque.glsl
             + voxy_translucent.glsl + voxy.json
    world1\  76 文件（同名同构）
    world-1\ 76 文件（同名同构）
    program\ 34 个 .glsl（+ voxy.json）
    lib\     若干 .glsl（common.glsl 等；**所有用户选项 #define 都在这里**）
    lang\    en_US.lang
  ── 与 world* 同级（shaders\ 根）──
    block.properties(34,652 B)  colorwheel.properties  dimension.properties
    entity.properties  item.properties  shaders.properties(23,489 B)  pack.json
```

| 指标 | 值 |
| --- | --- |
| 着色器文件总数 / 总行数 | 408 文件 / **18,978 行**（`.fsh` 108 + `.vsh` 108 + `.csh` 3 + `.glsl` 189） |
| `#version` 分布 | `130` × 213；`400 compatibility` × 3（三个维度的 `shadow.vsh`）；`430 compatibility` × 3（`shadowcomp.csh`）；`.glsl` 无 `#version` |
| `#extension` | **有**：`GL_ARB_shader_image_load_store` ×2、`GL_ARB_shader_storage_buffer_object` ×2、`GL_ARB_gpu_shader5`、`GL_ARB_shading_language_packing` |
| 预处理密度 | `#include` 790、`#define` 1209、`#ifdef` 895、`#if` 644、`#elif` 136 |
| legacy 内建 | `texture2D(` 118、`texture2DLod` 75、`shadow2D` 6、`gl_FragData` 66、`gl_ModelView*` 18、`gl_ProjectionMatrix` 5、`gl_NormalMatrix` 22、`gl_Vertex` 17、`gl_Color` 20、`gl_MultiTexCoord*` 27、`gl_TextureMatrix` 30、`ftransform(` 23、`attribute` 16 |
| 现代构造 | `varying` **0**、`texelFetch` 101、`gl_FragCoord` 73、`discard` 33、`switch` 2、`gl_VertexID` 13、`usampler*` 4、`sampler2DShadow` 2 |
| 资源 | `uniform sampler2D` 30、`colortex*` 124、`depthtex*` 48、`noisetex` 45、`shadowtex*` 12、`layout(` 6、`std140` **0**、`std430` 2 |
| **compute** | **存在**：`shadowcomp.csh` ×3（`#version 430 compatibility`），`imageStore` 9、`image2D` 2、`shared` 2、`barrier()` 1、`gl_WorkGroupID` 1、SSBO 2 |
| MRT | `DRAWBUFFERS` 43 处，最大 5 槽（`gbuffers_water: 03648`）；`RENDERTARGETS` 5 处，含**非连续且 > 8** 的编号（`deferred1: 0,5,4,19,18`） |
| 顶点属性 | `mc_Entity` 7、`mc_midTexCoord` 21、`at_tangent` 21 |

**结构事实（决定了规范化器的形状）**

1. **一个程序 = 宏参数化的元文件，编译两次**。`world0\composite.fsh` 全文 7 行：
   ```glsl
   #version 130
   #define FRAGMENT_SHADER
   #define OVERWORLD
   #define COMPOSITE
   #include "/program/composite.glsl"
   ```
   而 `program\composite.glsl` 内部用 `#ifdef FRAGMENT_SHADER … #endif` / `#ifdef VERTEX_SHADER … #endif` 写两个阶段。**所以"程序"不是文件，"程序"= (元文件, 阶段宏, 维度宏, 程序宏) 的组合。**
2. **程序编号不连续**：`program\composite2.glsl` 在三个维度目录里都**没有**入口文件（实测 `Get-ChildItem -Recurse -Filter composite2*` 只有那一个 `.glsl`）。⇒ 程序发现必须**以入口文件为准**，不能按 `composite0..7` 循环。
3. **维度三份全量拷贝**：`world0/world1/world-1` 各 76 个文件，内容靠 `#define OVERWORLD/NETHER/END` 区分。⇒ 规范化必须按维度分别展开，不能只编一份。
4. **参考实现不认识的新程序族**：`clrwl_*`（8 个文件/维度）+ `voxy_*` + `pack.json` + `colorwheel.properties` 在 Iris 26.1 里 0 命中。⇒ v1 必须把它们当"未知程序"跳过并记诊断，**不能崩**，也不能假装理解了。

### 2.4 Naga 30.0.1 能做什么、不能做什么（实测）

**依赖与开关**

- `naga = "30.0.1"` 默认 `default = []`（什么都不开）。`glsl-in = ["dep:pp-rs"]`、`hlsl-out`、`spv-in`、`spv-out`、`wgsl-in/out` 都要显式打开。当前模块的 `Cargo.toml` **一个 feature 都没开**（事实：`Illusion-shaders-extension\Cargo.toml` 只有 `naga = "30.0.1"`）。
- naga 的 GLSL 前端依赖 `pp-rs 0.2.1`，**没有 `#include`**（`pp-rs\src` 全文 include 命中只在注释里）。
- 许可证 `MIT OR Apache-2.0`，`rust-version = "1.87"`，edition 2021。本机 `rustc 1.98.1` 可用。

**上游编译 bug（必须先绕开）**\
`naga = { features = ["glsl-in", "hlsl-out"] }` 单独无法编译：

```
error[E0599]: no method named `apply_default_interpolation` found for enum `ir::Binding`
   src\front\glsl\variables.rs:467 → src\front\mod.rs 把 `mod interpolator` 门控在
   #[cfg(any(feature = "spv-in", feature = "wgsl-in"))]
```
⇒ 必须**同时**打开 `wgsl-in`（或 `spv-in`）。这是上游 30.0.1 的 feature 声明缺陷（29.0.3 同样存在）。

**能力边界（每条都是实测错误/输出）**

| 输入 | 结果 |
| --- | --- |
| `#version 130` | `kind=InvalidVersion(130)` |
| 两个 `layout(location=0/1) out vec4` | ✅ 通过；HLSL：`struct FragmentOutput { float4 outColor0_ : SV_Target0; float4 outColor1_ : SV_Target1; };` → **MRT 通路成立** |
| 三个**没有** location 的 `in` | ❌ `BindingCollision { location: 0 }`（naga 不自动分配 stage IO 位置） |
| 任何 `uniform`（块或散装） | ❌ `SemanticError("uniform/buffer blocks require layout(binding=X)")` |
| `uniform sampler2D x;` | ❌ `NotImplemented("variable qualifier")`，span 正好盖住 `sampler2D` |
| 匿名 uniform 块 + `layout(std140, binding=0)`，成员**裸名**访问 | ✅ 解析通过，成员偏移 `mvp@0 / tint@64`，struct span 80（自动补 4 字节尾填充）；HLSL：`cbuffer global : register(b0) { Scene global; }`，`mul(...)` 且 `row_major float4x4` |
| 分离式 `uniform texture2D tex; uniform sampler texS;` + `texture(sampler2D(tex,texS), uv)` | ✅ 解析通过，但 **hlsl-out panic**：`internal error: entered unreachable code: Sampler buffer of group SamplerIndexBufferKey { group: 0 } not bound to a register`（`back\hlsl\help.rs:2088`） |
| 同一模块给出 `sampler_buffer_binding_map` | 能写出来，但生成的是 `SamplerState nagaSamplerHeap[2048]` + `StructuredBuffer<uint>` 索引 + `space255`（`help.rs:2027-2103`、`writer.rs:1224-1263`） |

**结论（这三条直接决定 §6 的后端划分）**

1. Naga 的 GLSL 前端只覆盖 **Vulkan 口味**，必须由我们的规范化器保证输入合法（§4.0 的四条不变量 + §4.1–§4.11 的规则就是为它量身定做的）。
2. Naga 的 HLSL 后端产出的是 **wgpu-hal 的 D3D12 资源模型**（sampler heap + 索引缓冲），不是"经典根签名"。要用它必须在 §6.3 的三个方案里选一个（推荐 M3-a 的文本后处理），或者干脆换 SPIRV-Cross。
3. 因此 **M1（规范化 GLSL）不需要 Naga 参与编译**；Naga 只当"额外的语义体检"（可选 feature `naga-check`，用来提前发现我们自己规范化出来的非法写法）。

### 2.5 其它事实

- **cyntax**（`Illusion-shaders-extension\cyntax`，origin `wgpu-mc/cyntax`，main，`0.1.0`，8 个 commit，MPL-2.0，约 4,450 行）：唯一的可复用点是预处理器 `preprocess_str(input, macros)`（`src\lib.rs`），它已经识别 `#include` 行（`src\expand.rs:131` 的 `ControlLine::Include`）但实际取文件要外部钩子。**不建议直接依赖**：MPL-2.0 是文件级 copyleft，一旦我们改它的文件，那些文件就必须以 MPL 发布；而我们真正需要的是"JCPP 语义"，不是一个 C 预处理器。预处理器走 §3.1 阶段 5 的自研路线（约 1,200–1,600 行）。
- **Iris 许可证** = LGPL-3.0；`LICENSE-DEPENDENCIES` 自己标注 `glsl-transformer` 是 **AGPL-3.0**（"这可能影响你分发 Iris 的能力"）。⇒ 我们**不能**把 Iris 的 transformer 代码抄进本模块（LGPL 传染 + 上游自身也有 AGPL 污染），只能"行为对齐 + 自己写"。`glsl-transformer` 明确禁止使用（`vanilla_shader_plan.md` §2.6 已记录）。
- **语料许可证** = `Complementary License Agreement 1.6`：0.2 明确把"再分发（部分）包"与"修改包代码"排除在正常使用之外。⇒ `input\` **必须进 `.gitignore`**（已落实），仓库里只允许存"统计数字 + 结构化摘录的最短片段（引用性使用，且注明来源与不可分发）"。
- **本地参考克隆的处置**：`ref_shader_loader\Iris`（LGPL-3.0）与 `cyntax`（MPL-2.0）都只是"读它、对齐行为"用的，**不入库**（已 gitignore）。本模块永远不 import、不 vendored、不 fork 它们——这正是 §1.2 第 1 条契约的物理形态。

### 2.6 许可证与分发矩阵

| 组件 | 许可证 | 对本模块的处置 |
| --- | --- | --- |
| naga / naga-types | MIT OR Apache-2.0 | 可以依赖；`Cargo.lock` 固定版本 |
| pp-rs | MIT OR Apache-2.0 | 仅在启用 `naga-check` 时作为间接依赖 |
| cyntax | MPL-2.0 | **不依赖**（只作参考，见 §2.5）；本地克隆 `/cyntax/` 已 gitignore |
| Iris 26.1 | LGPL-3.0 | **只做行为参考**，不复制代码；文档里引用 `path:line` 属于描述性引用；本地克隆 `/ref_shader_loader/` 已 gitignore |
| glsl-transformer | AGPL-3.0 | **禁止**任何形式引入 |
| Complementary Unbound r5.8.1 | Complementary License Agreement 1.6 | **不入库、不随发行包分发**；已由 `Illusion-shaders-extension\.gitignore` 的 `/input/` 落实；仓库只留统计数字与指纹 |
| 本模块自身 | 待定（建议与主体一致；Rust 侧用 `MIT OR Apache-2.0` 便于被宿主引用） | 需与用户确认后写进 `Cargo.toml` |

### 2.7 M1 契约的端到端离线验证（本机实测，2026-10-27）

§6.1 说 M1 是"把 §4 的归一化 GLSL 直接喂给宿主机既有的四步链"。这句话在写下来的时候只是推理；本节把它变成实测。

**方法**：不实现本模块，先按 §4 的规则**手写"产物应该长什么样"的等价样本**，然后跑主体同一条链。两份样本已随本文件入库（`docs\spike\`，共 4 KB，纯自写、不含任何包代码）：`composite5.frag`、`gbuffers_terrain.vert`。产物（`.spv`/`.hlsl`/`.dxil`）不进仓库。

- `composite5.frag`：具名兼容块 + 匿名 std140 块 + 分离矩阵块（`mat4` 与 `mat3` 混排）+ 6 个采样器（含 `sampler2DShadow` 与 `usampler2D`）+ `layout(location=0..2) in`（含 `flat`）+ 两槽 MRT + `discard` + `texelFetch`/`textureLod`。
- `gbuffers_terrain.vert`：`iris_Position/Color/UV0/UV2/Normal` 属性（`ivec2` 与 `int2`）+ `iris_transforms.ModelViewMat * sxTranslate(ModelOffset)` + `iris_ProjMat` + `iris_NormalMat`。

**命令**（本机已装 Vulkan SDK，三个工具都在 `C:\Program Files\Khronos Group\Vulkan SDK\Bin\`；`cd Illusion-shaders-extension\docs\spike` 后执行）：

```powershell
glslangValidator -V --target-env vulkan1.2 -S frag -o composite5.frag.spv composite5.frag
spirv-cross composite5.frag.spv --hlsl --shader-model 51 --output composite5.hlsl
dxc -T ps_6_9 -E main -Fo composite5.dxil composite5.hlsl
# vertex 同构：-S vert / vs_6_9
```

| 步骤 | 结果 | 产物 |
| --- | --- | --- |
| glslangValidator → SPIR-V（frag / vert） | exit 0 / exit 0 | 6,168 B / 4,224 B |
| spirv-cross → HLSL SM 5.1 | exit 0 / exit 0 | 3,992 B / 2,790 B |
| dxc → DXIL `ps_6_9` / `vs_6_9` | exit 0，**零告警** | 8,264 B / 8,052 B |
| dxc → DXIL `ps_6_6`（对照组） | exit 0 | 8,024 B |

**九条实测结论**（每条都影响设计，不是"验证通过"一句话）：

| # | 结论 | 证据 / 影响 |
| --- | --- | --- |
| 1 | `#version 450 core` 是**下限**，不是偏好 | `#version 130` 直接报 `Desktop shaders for Vulkan SPIR-V require version 140 or higher` ⇒ R1 的取值是硬的 |
| 2 | `layout(binding=X)` 对 **uniform 块和采样器都是强制** | 缺块 binding → `uniform/buffer blocks require layout(binding=X)`；缺采样器 binding → `sampler/texture/image requires layout(binding=X)`。两条消息与 §13.3 里 naga 的措辞同源（同一 Vulkan 规则）⇒ R8 里"必须写 binding"对 M1 同样成立，不是只为 M3 |
| 3 | `texture2D(...)` 在 450 core 下**不是函数** | 报 `'sampler/image' : cannot construct this type`（被当成类型构造）⇒ R5 的改名是必需项，不是兼容糖 |
| 4 | 具名块的成员**必须带实例名前缀** | 裸写 `ColorModulator` → `undeclared identifier` ⇒ §4.8 把"`iris_transforms` 用具名块、散装 uniform 用匿名块"分开写是硬约束，两者不可互换 |
| 5 | MRT 由 `layout(location)` 决定，与 `DRAWBUFFERS` 无关 | 输出 `SPIRV_Cross_Output { float4 iris_FragData0 : SV_Target0; float4 iris_FragData1 : SV_Target1; }`；注释在 SPIR-V 里完全不存在 ⇒ R6"先把注释变成代码"是唯一可行路径 |
| 6 | 匿名块 → 具名 cbuffer，且 `packoffset` 与 §4.8 的 std140 计算**逐一吻合** | `cbuffer SxLooseUniforms : register(b1, space0)`，成员被改名为 `_70_*`：`shadowModelView @ c0`、`cameraPosition @ c4`、`frameTimeCounter @ c4.w`、`shadowLightPosition @ c5`、`viewWidth @ c5.w`、`viewHeight @ c6`、`invViewSize @ c6.z` —— 与文档给出的偏移表完全一致。这是对 R8 布局算法的独立校验 |
| 7 | `sampler2DShadow` 在 M1 链下**完整可用** | → `Texture2D<float4> shadowtex0` + `SamplerComparisonState _shadowtex0_sampler` + `.SampleCmp(uv, depth)`，DXIL 通过 ⇒ 阴影比较采样不需要 M3；**但宿主必须为它提供 comparison 采样器**，故 `plan.json` 的采样器描述需要 `comparison` 字段（§5.2、§5.3） |
| 8 | `binding` 号**直接变成 HLSL register**，但主体会把它覆盖掉 | spvc 输出 `register(b1)` / `register(t105)` / `register(s105)`，即 binding 原样搬进 register；而主体在 `D3D12IntermediaryShaderModule.kt:114-140` 按 resource 发现顺序把 binding 重写成 entries 下标 ⇒ **`plan.json` 里的 binding 只是提示**，权威映射来自宿主自己的 reflection + 名字/语义对齐（§5.3 已注明） |
| 9 | 主体的 Y 翻转补丁在 M1 产物上**仍能命中** | spvc 产出的 `void frag_main()\n{` 确实存在（样本 HLSL 第 71-72 行），`D3D12ShaderCompiler.kt:188-196` 的 needle 不会静默失配 ⇒ §11 的 R6 风险从"中"降到"低" |

**结论**：M1 的契约可以**完全离线闭环**，不需要先进游戏。因此 L0.5 与 L1 的 fragment/vertex 部分没有未解的技术风险，剩下的未知只有两处，且都在宿主侧而不在本模块：①宿主的渲染图能力（§6.1 的 M1-4：colortex 池、MRT 描述、pass 顺序）；②compute（L2c）。

**副产品**：这套三命令链同时就是 §9.1 的"第 1 层测试"实现——`tests/golden` 的每个用例都可以用同样三条命令比对产物，不需要任何 Minecraft 环境。

## 3. 流水线、crate 布局与错误模型

### 3.1 十四个阶段与它们的产物

每一阶段的输出都是**具体类型**，任何阶段失败只产生该阶段的错误变体，不存在 `Result<String, String>`。

| # | 阶段 | 输入 | 输出 | 失败变体 |
| --- | --- | --- | --- | --- |
| 1 | `open_pack` | 根路径 | `PackInfo{ name, roots[] }` | `NotFound` / `Malformed` |
| 2 | `discover_programs` | `PackInfo` | `Vec<ProgramEntry>` | `Discovery` |
| 3 | `resolve_fallback` | 程序清单 | `Vec<ResolvedProgram>`（含 `fallback_from`） | — （缺失是正常态） |
| 4 | `splice_includes` | 程序入口文件 | `SplicedSource{ text, line_map }` | `Include{cycle,missing,depth,files}` |
| 5 | `preprocess` | `SplicedSource` + `EnvMacros` | `Preprocessed{ text, line_map }` | `Preprocess{unterminated_if,bad_directive,out_of_memory}` |
| 6 | `collect_directives` | `Preprocessed`（片元） | `Directives{ mrt, consts, alpha_test }` | `Directive` |
| 7 | `build_env` | 程序上下文 | `EnvMacros` | — |
| 8 | `scan_declarations` | `Preprocessed` | `Decls{ uniforms, samplers, io, attrs, images, ssbos }` | `Dialect{unsupported}` |
| 9 | `dialect_rewrite` | `Preprocessed` + `Decls` + `Directives` | `Rewritten{ tokens, rewrites[] }` | `Dialect{unsupported,conflict}` |
| 10 | `layout_uniforms` | `Decls.uniforms` | `UniformLayout{ blocks[], size }` | `Layout{overflow}` |
| 11 | `emit` | `Rewritten` + `UniformLayout` | `Emitted{ source, line_map }` | `Emit{internal}` |
| 12 | `assemble_plan` | 所有产物 | `Plan`（§5） | `Plan{inconsistent}` |
| 13 | `diagnostics` | 全部 | `Vec<Diag>`（累计，不中断） | — |
| 14 | `cache_key` | 输入 + 版本 | `[u8;32]` | — |

**顺序性的三条硬约束**（都有实测依据）：

1. `splice_includes` 必须早于 `preprocess`。Iris 就是先拼文件再跑 JCPP（§2.2），因为拼接后的文本才消除 `#ifdef FRAGMENT_SHADER` 死分支；反过来做会让 `DRAWBUFFERS` 与 `#version` 都不可判定。
2. `collect_directives` 必须在 `preprocess` 之后、`dialect_rewrite` 之前。指令是**注释**，`dialect_rewrite` 之后注释可能已被规范化器丢弃或位置改写。
3. `layout_uniforms` 必须在 `emit` 之前，因为注入的块要带着算好的 `binding` 写进源码；同时 `plan.json` 的偏移来自同一个结果，**两处不得各算一次**。

### 3.2 crate 布局

```
Illusion-shaders-extension/
├─ Cargo.toml                 # workspace，resolver = "3"，无路径依赖到主体
├─ crates/
│  ├─ sx-core/                # 全部纯 Rust 逻辑，不依赖任何 FFI/OS 特性
│  │  ├─ src/lib.rs
│  │  ├─ src/pack/{mod,fs,discover,fallback}.rs
│  │  ├─ src/pre/{mod,include,preprocess,env,macros}.rs
│  │  ├─ src/directives.rs
│  │  ├─ src/lex/{mod,token,scanner}.rs
│  │  ├─ src/dialect/{mod,version,io,builtins,samplers,textures,mrt,attrs,ssbo}.rs
│  │  ├─ src/uniform_layout.rs
│  │  ├─ src/emit/{mod,glsl450,naga_subset}.rs
│  │  ├─ src/plan/{mod,schema,semantics}.rs
│  │  └─ src/diag.rs
│  ├─ sx-abi/                 # cdylib + staticlib，唯一 extern "C" 边界
│  ├─ sx-cli/                 # 离线批量编译/差分（开发与 CI 的主力）
│  └─ sx-golden/              # 合成 mini-pack + 快照测试
├─ docs/
├─ input/                     # 语料，**.gitignore**（许可证见 §2.6）
└─ ref_shader_loader/         # Iris 参考仓，**.gitignore** 或 submodule（不入发行包）
```

依赖方向是单向的：`sx-cmd → sx-abi → sx-core`，`sx-cli → sx-core`。`sx-core` **不认识 FFM、不认识 D3D12、不认识 Minecraft 版本**——它只认识 GLSL 文本与光影包格式。

**feature 开关（默认全关，由宿主或 CLI 显式打开）：**

| feature | 作用 | 默认 |
| --- | --- | --- |
| `naga-check` | 用 naga 校验 `naga_subset` 产物（会连带打开 `wgsl-in`，见 §2.4） | off |
| `naga-hlsl` | 直接产出 HLSL（M3，依赖 sampler heap，见 §6.3） | off |
| `spirv-verify` | 用 `naga spv-in` 校验 M1 产物 | off |
| `serde` | `plan.json` 序列化 | on |

### 3.3 错误模型

一个 `Diag` 就是一条可定位、可去重、可统计的记录：

```rust
pub struct Diag {
    pub code: DiagCode,        // 稳定字符串，如 "SX0312"
    pub severity: Severity,    // Error | Warning | Note
    pub stage: Stage,
    pub program: Option<String>,
    pub file: Option<PathBuf>, // pack 内相对路径
    pub line: Option<u32>,     // 相对入口文件（include 已展开）
    pub origin: Option<Origin>,// 追加的 (file,line)，用于把错误映射回被 include 的行
    pub message: String,
    pub hint: Option<String>,
}
```

| 区间 | 类别 | 例子 |
| --- | --- | --- |
| `SX00xx` | 包级 | `SX0001 include-cycle`、`SX0002 include-missing`、`SX0003 include-too-deep` |
| `SX01xx` | 预处理 | `SX0101 unterminated-if`、`SX0102 unknown-directive`、`SX0103 macro-recursion-limit` |
| `SX02xx` | 发现 | `SX0201 unknown-program-skipped`（`clrwl_*` / `voxy_*` 落到这里）、`SX0202 duplicate-program` |
| `SX03xx` | 方言 | `SX0301 unsupported-construct`（带构造名）、`SX0302 rewrite-conflict`、`SX0303 reserved-prefix-used`（包源码里出现 `iris_`/`moj_import`，照 Iris 的做法**报错**而不是静默） |
| `SX04xx` | 布局 | `SX0401 uniform-block-overflow`（std140 超过宿主单 UBO 上限） |
| `SX05xx` | 能力 | `SX0501 requires-compute`、`SX0502 requires-uav`、`SX0503 requires-shadow-map`、`SX0504 requires-vertex-attribute` |

`SX05xx` 是全案最重要的一类：它把"这个包跑不动"从**编译失败**变成**能力缺口**，宿主据此决定降级到 L0/L1 而不是黑屏。§2.1 的注释也印证了主体自身的分层文化（"后端架构派生自能力表"）。

## 4. 方言规范化规则（R1–R11）

### 4.0 为什么必须是 token 级，而不能是正则

`texture2D`、`gl_FragData` 这类名字会出现在注释、`#define` 名、变量名、用户函数名里。v2.0 的 §6.4 已经写对了这一点（"禁止用全局字符串替换"），本版把它变成**可执行的规则**：

规范化器分四个 pass，每个 pass 都是纯函数：

1. **tokenize**：把 `Preprocessed` 切成 token 流（标识符 / 数字 / 标点 / 注释 / 行号）。保留原始行列，用于诊断。
2. **结构扫描**：只用大括号与小括号深度做**轻结构**分析，识别出顶层声明（`uniform`/`in`/`out`/`attribute`/`varying`/`layout(...)` 前缀 + 类型 + 名字列表）、函数定义边界、以及每个标识符是否处于"声明位置"或"引用位置"。
3. **规则改写**：R1–R11 依次作用在 token 区间上，全部是**整 token 替换**或**区间插入**，不做子串匹配。
4. **emit**：从 token 流重建文本，同时产出 `line_map`（产物行 → 入口文件行）。

四条不变量（全部写成测试）：

- **幂等**：`normalize(normalize(x)) == normalize(x)`。
- **不误伤**：注释、字符串字面量、以及"作为声明名的标识符"不会被 R4–R6 改写（命中时报 `SX0303` 或 warning）。
- **可归因**：任何一条改写都记录在 `rewrites[]` 里（规则号 + 原 token + 新 token + 位置），`plan.json` 与 CLI 都能打印。
- **不猜**：规则不认识的东西（新语法、26.3 才有的内建）一律 `SX0301 unsupported-construct` 并保留原文，**绝不做近似替换**。

### 4.1 R1：版本与扩展

```glsl
// 产物第一行，永远且仅有一行
#version 450 core
```

- 丢弃原 `#version`（语料里有 `130` ×213、`400 compatibility` ×3、`430 compatibility` ×3）。compatibility profile 与 `core` 的差别由 R3 的内建映射补上，**不靠 GL 驱动**。
- compute 程序（`*.csh`）同样用 `450 core`（compute 要求 ≥430，450 足够且与图形侧统一）。
- 6 条 `#extension GL_ARB_*: enable` **全部丢弃**，每条生成一条 `Note`：`dropped-extension`。理由：`ARB_shader_image_load_store` / `ARB_shader_storage_buffer_object` / `ARB_gpu_shader5` / `ARB_shading_language_packing` 在 450 core 里已是核心特性，保留反而在严格编译器上报错。
- **不允许**引入任何 `#include`/`#extension` 之外的、需要外部文件系统的指令（如 `GL_ARB_shading_language_include`）：include 已在阶段 4 拼接完毕。

### 4.2 R2：阶段与 IO 修饰符

| 原 | 目标 | 说明 |
| --- | --- | --- |
| `attribute` | `in` | 仅顶点阶段；语料 16 处 |
| `varying` | VS 里 `out` / FS 里 `in` | 语料 0 处，规则保留 |
| `noperspective` / `flat` / `smooth` / `centroid` | 原样保留 | 450 core 合法 |
| `invariant` | 原样保留 | 同上 |

阶段由入口文件的扩展名决定（`.vsh`/`.fsh`/`.csh`，以及 `.gsh`/`.tcs`/`.tes` 一旦出现即 `SX0301`）。

### 4.3 R3：所有 IO 显式 `layout(location=)`

`naga` **不做** stage IO 的自动分配（`BindingCollision{location:0}`，§2.4），宿主依赖 GLSL 的 location 决定 HLSL 的 `SV_TargetN`/输入槽。因此产物里**每个**顶点输入、每个 varying、每个片元输出都必须带 `layout(location = N)`。

分配策略（确定性，不依赖哈希顺序）：

| 类别 | location 来源 |
| --- | --- |
| 顶点输入 | 固定表（§4.7），未知名字按首次出现顺序续接 |
| VS out / FS in（varying） | 按**声明顺序**编号，两侧按名字对齐；若某名字只在一侧出现 → `SX0302` 并补空声明 |
| FS out | 由 MRT 槽位决定（§4.6），不是声明顺序 |

两侧对齐必须做，因为语料是 `#include` 拼出来的，同一 varying 在 VS 和 FS 的声明顺序不保证相同。

### 4.4 R4：legacy 内建 → 注入符号（全表）

这张表是本模块最核心的**知识资产**：它把"上世纪的 GL 立即模式内建"翻译成"宿主今天能提供的数值"。替换字面量与 Iris 26.1 逐条对齐（`path:line` 见 §13.1），**改动的只是 `iris_*` 的供应方式**——Iris 靠 GL 兼容 profile 与 blaze3d shader JSON，我们靠注入声明 + `plan.json`。

| legacy | 替换为 | 注入的声明 | 宿主提供 |
| --- | --- | --- | --- |
| `gl_Vertex` | `vec4(iris_Position, 1.0)` | `in vec3 iris_Position;` | 顶点属性（POSITION） |
| `gl_Color` | `(iris_Color * iris_transforms.ColorModulator)` | `in vec4 iris_Color;` | 顶点属性（COLOR）+ `ColorModulator` |
| `gl_Normal` | `iris_Normal` | `in vec3 iris_Normal;` | 顶点属性（NORMAL，可能缺 → `SX0504`） |
| `gl_MultiTexCoord0` | `vec4(iris_UV0, 0.0, 1.0)` | `in vec2 iris_UV0;` | 顶点属性（UV0） |
| `gl_MultiTexCoord1` | `vec4(iris_UV2, 0.0, 1.0)` | `in ivec2 iris_UV2;` | 顶点属性（UV2 / lightmap） |
| `gl_MultiTexCoord2` | （改名为 `gl_MultiTexCoord1`，Iris 同款） | — | — |
| `gl_MultiTexCoord3` | `mc_midTexCoord` | `in vec4 mc_midTexCoord;` | 顶点属性，可缺 → 补 `vec4(0.0)` |
| `gl_TextureMatrix[0]` | `iris_transforms.TextureMat` | `SxDynamicTransforms.TextureMat` | `TextureMat` |
| `gl_TextureMatrix[1]` | `iris_LightmapTextureMatrix` | `uniform mat4 iris_LightmapTextureMatrix;` | 宿主常量（可算） |
| `gl_ModelViewMatrix` | **gbuffers 类**：`iris_transforms.ModelViewMat * _iris_internal_translate(iris_transforms.ModelOffset)`；**合成类**：`mat4(1.0)` | `SxDynamicTransforms.{ModelViewMat,ModelOffset}` | `ModelViewMat` + `ModelOffset` |
| `gl_ModelViewProjectionMatrix` | `(iris_ProjMat * gl_ModelViewMatrix)`（**必须在其后应用**，故 R4 内部有子顺序） | — | 由上面两项组合 |
| `gl_ProjectionMatrix` | `iris_ProjMat` | `uniform mat4 iris_ProjMat;` | `ProjMat` |
| `gl_NormalMatrix` | `iris_NormalMat` | `uniform mat3 iris_NormalMat;` | 宿主算 `transpose(inverse(mat3(ModelViewMat)))` |
| `gl_ModelViewMatrixInverse` | `iris_ModelViewMatInverse` | `uniform mat4 ...` | 需宿主额外提供；缺失 → `SX0504` |
| `gl_ProjectionMatrixInverse` | `iris_ProjMatInverse` | `uniform mat4 ...` | 同上 |
| `ftransform` | 注入 `vec4 ftransform() { return gl_ModelViewProjectionMatrix * gl_Vertex; }`（**仅当源码引用**） | — | 展开后由上面各行继续处理 |
| `gl_FragCoord` / `gl_FrontFacing` / `gl_PointCoord` / `gl_VertexID` / `gl_InstanceID` / `gl_Layer` / `gl_PrimitiveID` / `gl_WorkGroupID` / `gl_LocalInvocationID` | 原样保留 | — | GLSL 450 核心内建，naga 与 spvc 均支持 |

**子顺序（同一 pass 内必须按此序，否则二次替换会错）**：

1. `gl_MultiTexCoord*`、`gl_TextureMatrix*`（带下标，先于不带下标的）
2. `gl_ModelViewMatrix`（**先于** `gl_ModelViewProjectionMatrix` 与 `gl_ModelViewMatrixInverse`，因为后者名字以后者为前缀）
3. `gl_ModelViewProjectionMatrix`、`gl_ProjectionMatrix`
4. `gl_NormalMatrix`
5. `gl_Vertex`、`gl_Color`、`gl_Normal`
6. `ftransform`（最后展开，它的函数体会引用已被替换的名字）

**为什么 `gl_ModelViewMatrix` 要乘平移矩阵**：Iris 对 gbuffers 类是 `iris_transforms.ModelViewMat * _iris_internal_translate(iris_transforms.ModelOffset)`（`VanillaTransformer.java:345-368`），因为 vanilla 的 `ModelViewMat` 不含区块偏移、`ModelOffset` 才是每 draw 的平移。这一条不照抄就会出现"地形挤在原点"的经典错位。合成类用 `mat4(1.0)`（`CompositeTransformer.java:78`）是因为全屏 pass 的顶点本来就是裁剪空间的 quad。

### 4.5 R5：纹理函数族改名

把 `GLSL 1.2/1.3` 的采样函数统一到 4.x 名字。语料里实际出现的：`texture2D`(118)、`texture2DLod`(75)、`texelFetch`(101)、`shadow2D`(6)。

| 原 | 目标 |
| --- | --- |
| `texture2D` / `texture2DProj` / `texture2DLod` / `texture2DGrad` / `texture2DBias` | `texture` / `textureProj` / `textureLod` / `textureGrad` / `texture`（带 bias 参数） |
| `texture3D*`、`textureCube*` | 对应 4.x 名 |
| `texelFetch2D` / `texelFetch3D` | `texelFetch` |
| `textureSize2D` | `textureSize` |
| `shadow2D` / `shadow2DProj` / `shadow2DLod` | `texture` / `textureProj` / `textureLod`（目标类型 `sampler2DShadow`） |
| `texture(sampler2D, vec2)` **返回 `vec4`** | 保持（450 里 `texture(sampler2D, vec2)` 就返回 vec4；`sampler2DShadow` 返回 `float`，若源码把它当 `vec4` 用 → `SX0301`） |

`texture2DLod` 与 `textureLod` 的参数顺序不同（`texture2DLod(s, uv, lod)` vs `textureLod(s, uv, lod)` 相同，但 `Proj`/`Grad` 变体不同），**只做名字替换，不做参数重排**——参数重排需要类型信息，属于"猜"，按 §4.0 的不变量拒绝。

### 4.6 R6：MRT —— `DRAWBUFFERS` → `layout(location)`

输入（阶段 6 已解析，§2.2 与 Iris 的 `CommentDirectiveParser` 语义一致）：

```glsl
/* DRAWBUFFERS:03648 */          // → 5 个 MRT 槽，依次写 colortex0/3/6/4/8
/* RENDERTARGETS: 0,5,4,19 */    // → 同上，逗号分隔形式（语料里 deferred1 用）
```

**语义关键点**：`gl_FragData[i]` 里的 `i` 是**列表下标**，不是 colortex 编号。所以

```
DRAWBUFFERS:0364   +   gl_FragData[0..3]   ⇒   MRT 槽 0..3 → colortex 0,3,6,4
```

改写：

```glsl
// 在声明区注入（每个被写入的槽一条）
layout(location = 0) out vec4 iris_FragData0;
layout(location = 1) out vec4 iris_FragData1;
// 引用处
gl_FragData[0] = color;   →   iris_FragData0 = color;
```

`plan.json` 里同时给出 `mrt.targets = [0,3,6,4]`（**槽 → colortex 映射**），宿主据此建 MRT 视图。四种异常都要有确定行为：

| 情况 | 行为 |
| --- | --- |
| 无指令、只用 `gl_FragData[0]` | 视为单目标 `colortex0`，`Warning: default-mrt` |
| 指令槽数 < 实际写入的最大下标 +1 | `SX0302`，按指令长度补齐（不猜补哪个 colortex） |
| `gl_FragData[i]` 用**非常量**下标 | `SX0301`（GLSL 1.3 允许这种写法，450 的 out 变量不允许） |
| 指令出现在被 include 的多处、且值不同 | 取**文件位置最靠后**的一条（`getAppliedDirective` 语义，§2.2），并 `Note` 记录覆盖关系 |

### 4.7 R7：顶点属性名 → location 固定表

| 属性名 | location | 格式 | 备注 |
| --- | --- | --- | --- |
| `iris_Position` | 0 | `R32G32B32_FLOAT` | 对应主体既有 `Position` |
| `iris_Color` | 1 | `R8G8B8A8_UNORM` | 对应 `Color` |
| `iris_UV0` | 2 | `R32G32_FLOAT` | 对应 `UV0` |
| `iris_UV2` | 3 | `R16G16_SINT` | 对应 `UV2`（lightmap） |
| `iris_Normal` | 4 | `R8G8B8A8_SNORM` | **主体顶点格式里没有**（§2.1 只有 4 个属性）→ `SX0504` |
| `mc_midTexCoord` | 5 | `R32G32B32A32_FLOAT` | 与上同，缺失时补 `vec4(0.0)` |
| `at_tangent` | 6 | `R8G8B8A8_SNORM` | 语料 21 处，缺失即 `SX0504` |
| `iris_Entity` | 7 | `R16G16B16A16_SINT` | `mc_Entity` 的整数形态（Iris `patchIntegerAttribute`），属 L1 决策门 VG4 |

未在表中的名字按首次出现顺序从 8 起编号，并 `Warning: unknown-attribute`。

（这张表故意把"主体能供的 4 个"排在前面、把"需要扩顶点格式的"排在后面：**M1 的 demo 只需要 0–3**，第 4 行之后全部属于 L1。）

### 4.8 R8：散装 `uniform` → 显式 std140 块

naga 要求 `layout(binding=)`，`plan.json` 需要偏移，宿主需要知道"哪个寄存器装什么"。三件事指向同一个做法：**把一阶段的所有散装 uniform 收进一个匿名块**。

```glsl
// 包源码
uniform mat4 shadowModelView;
uniform vec3 cameraPosition;
uniform float frameTimeCounter;
uniform vec3 shadowLightPosition;

// 产物（binding 由策略分配，见下）
layout(std140, binding = 1) uniform SxLooseUniforms {
    mat4 shadowModelView;      // offset 0
    vec3 cameraPosition;       // offset 64
    float frameTimeCounter;    // offset 76
    vec3 shadowLightPosition;  // offset 80
};
// 没有实例名 ⇒ 成员名本来就是全局可见的，引用处一个字都不用改
```

- **用匿名块（成员裸名）而不是具名实例**：这条已被实测（§2.4 的表：`layout(std140, binding=0) uniform … {…};` + 裸成员访问 → 解析通过、偏移 `mvp@0 / tint@64`、HLSL 出 `cbuffer`）。它同时消掉了三件麻烦事：不需要引用加前缀、不需要处理同名遮蔽、产物与包源码的可读性一致。**唯一的例外**是 §4.4 的兼容块 `iris_transforms`——那里刻意保留具名实例，让替换字面量与 Iris 1:1 对齐。
- **布局算法**（严格 std140，自行实现，不依赖驱动）：`float/int/bool` 4 字节对齐；`vec2` 8；`vec3`/`vec4` 16；`mat2`=2×vec4、`mat3`=3×vec4、`mat4`=4×vec4（列步长 16）；数组元素步长 = 16 向上取整；块总大小 16 字节对齐。`plan.json` 记录 `matrixStride=16`、`arrayStride`。
- **binding 分配策略**：`0` 保留给 `SxDynamicTransforms`（Iris 那块 `iris_transforms` 的落点：`ModelViewMat`/`ColorModulator`/`ModelOffset`/`TextureMat`），`1` 给 `SxLooseUniforms`，`2–15` 预留给后续块；纹理/采样器从 `100`/`200` 起，永不与块冲突。
- 同名 uniform 在 VS/FS 都出现时：**各自独立成块**（阶段私有），不假设跨阶段共享；宿主可用同一个 CBV 填充两处（`plan.json` 给 `semantic` 让宿主知道可以合并）。
- 上界：单块 ≤ 4096 字节（保守，低于 DX12 的 64KB CBV 上限，也低于主体 `GpuBufferSlice` 的实际容量）；超出 → `SX0401` 并把块按 4096 切分（`SxLooseUniforms_0/_1/...`）。

### 4.9 R9：`mc_Entity` 与其它 Iris 扩展属性

`attribute vec4 mc_Entity;` 在语料里 7 处，是"方块 ID + 类型"的入口（OptiFine 语义：`x` = block id，`y` = render type，`z`/`w` = 附加）。

两种落法，**由 capability 决定而不是由代码分支决定**：

| 宿主能力 | 落法 | 后果 |
| --- | --- | --- |
| 有 `iris_Entity` 属性（L1 / VG4 通过） | `in ivec3 iris_Entity;`，`mc_Entity` 的 `x` 取 `float(iris_Entity.x)` | 完整语义 |
| 没有 | 恒等替换 `vec4(0.0)` + `SX0504 requires-vertex-attribute` | 依赖 `mc_Entity` 的分类会退化（例如所有方块走同一分支），但**不黑屏、不报错** |

`mc_Entity` 的 float→int 转换必须走 `patchIntegerAttribute` 等价物（保留小数位信息：Iris 把 `mc_Entity` 的整数部分放 `iris_Entity.x`、`y/z/w` 放大后装进 `y`），细节见 §13.1 引用处。**v1 不做压缩编码**：直接 `ivec3`，因为 DXGI 顶点格式支持 `R16G16B16A16_SINT`，比 Iris 的"打包进 vec4"更干净。

### 4.10 R10：SSBO / image / compute（只识别，不翻译）

语料实测：`std430 buffer {...}` 2 处、`imageStore` 9 处、`image2D` 2 处、`shared` 2 处、`barrier(` 1 处、`gl_WorkGroupID` 1 处，全部集中在 3 个 `shadowcomp.csh`。

v1 对 `.csh` 的处置：

1. 正常走发现/拼接/预处理（能拿到 macro 环境与 include 图）；
2. `dialect_rewrite` 遇到 `buffer`/`image*`/`shared`/`barrier` **不报错**，只把这些声明登记进 `Decls.ssbos/images`，并生成 `SX0501 requires-compute` + `SX0502 requires-uav`；
3. **不 emit** 该阶段（`plan.json` 里 `stages[].emitted = false`，附 `capability_gap`）。

理由：主体今天没有 compute PSO、没有 UAV 描述表（`vanilla_shader_plan.md` §0.2）。模块的价值在这里体现为**"准确报告缺口"**，而不是假装能翻译。

### 4.11 R11：保留词与冲突

- `texture`、`sample`、`buffer`、`filter`、`precise` 等在 450 里是保留词/内建名。若包把它们当标识符用（`texture` 曾是很常见的 varying 名），改名成 `iris_renamed_<name>` 并记录（Iris `CompatibilityTransformer` 同款）。
- 包源码里出现 `iris_` / `irisMain` / `moj_import` 前缀 → `SX0303` 硬错误（与 Iris 的 `TransformPatcher` 行为一致：这些是内部接口，包不该碰）。

### 4.12 产物：归一化 GLSL 450 的可验证性

`emit` 的输出是**纯文本 GLSL 450 core**，不依赖任何宿主。这带来一个极强的验证手段：

> 把产物直接喂给 `glslangValidator -V`（或 naga 的 `glsl-in`），能过就说明"规范化"这一层是对的；过不了的行号会经 `line_map` 映射回包源码。

这条闭环让 M1 的绝大部分工作**不需要跑游戏**就能验证——`sx-cli` 一个命令扫完 408 个文件。

## 5. `plan.json` —— 渲染图契约

### 5.1 为什么是"第二个产物"

HLSL/GLSL 只是"某个 stage 的代码"。光影包还带了一整套**帧结构**信息：pass 顺序、MRT 布局、渲染目标格式与清屏、采样器绑什么纹理、需要哪些顶点属性、哪些能力缺失。这些在 Iris 里散落在三处（`ProgramDirectives`、`IrisSamplers`、`PackRenderTargetDirectives`），在 DXRMC 里**没有任何等价物**。

因此本模块的第二个交付物是 `plan.json`：它是光影包世界与宿主渲染图之间的**唯一接口**。宿主不认识 `DRAWBUFFERS`、不认识 `colortex3`，只认识这张表。

### 5.2 schema（v1，`schemaVersion: 1`）

```jsonc
{
  "schemaVersion": 1,
  "toolchain": { "crate": "0.1.0", "naga": null, "target": "glsl450" },
  "pack": {
    "name": "ComplementaryUnbound",
    "sourceVersion": "r5.8.1",
    "license": "Complementary License Agreement 1.6",
    "dimensions": ["world0", "world1", "world-1"]
  },
  "programs": [
    {
      "name": "composite5",
      "group": "composite",
      "index": 5,
      "status": "ok",                       // ok | partial | skipped
      "fallbackFrom": null,
      "stages": [
        { "stage": "fragment", "entryFile": "world0/composite5.fsh", "emitted": true },
        { "stage": "vertex",   "entryFile": "world0/composite5.vsh", "emitted": true }
      ],
      "mrt": {
        "source": "drawbuffers",           // drawbuffers | rendertargets | default
        "targets": [3],                     // 槽 → colortex
        "locationCount": 1
      },
      "renderTargets": [
        { "index": 3, "format": "RGBA16F", "clear": true, "clearColor": null, "mipmaps": false },
        { "index": 7, "format": "RGBA8",   "clear": false, "clearColor": [0,0,0,1], "mipmaps": false }
      ],
      "samplers": [
        { "name": "colortex3", "kind": "texture2D", "internalFormat": "RGBA16F",
          "binding": 103, "sampler": "colortex3", "filter": "LINEAR", "wrap": "CLAMP",
          "comparison": false },
        { "name": "shadowtex0", "kind": "texture2D", "internalFormat": "DEPTH32",
          "binding": 104, "sampler": "shadowtex0", "filter": "LINEAR", "wrap": "CLAMP",
          "comparison": true },
        { "name": "noisetex", "kind": "texture2D", "internalFormat": "R8",
          "binding": 145, "sampler": "noisetex", "filter": "LINEAR", "wrap": "REPEAT",
          "source": "procedural:noise", "comparison": false }
      ],
      "uniformBlocks": [
        { "name": "SxDynamicTransforms", "binding": 0, "size": 208,
          "members": [
            { "name": "ModelViewMat", "type": "mat4", "offset": 0,  "matrixStride": 16, "semantic": "MODEL_VIEW" },
            { "name": "ColorModulator","type": "vec4", "offset": 64, "semantic": "COLOR_MODULATOR" },
            { "name": "ModelOffset",   "type": "vec3", "offset": 80, "semantic": "MODEL_OFFSET" },
            { "name": "TextureMat",    "type": "mat4", "offset": 96, "matrixStride": 16, "semantic": "TEXTURE_MATRIX" }
          ] },
        { "name": "SxLooseUniforms", "binding": 1, "size": 96,
          "members": [
            { "name": "shadowModelView",    "type": "mat4", "offset": 0,  "matrixStride": 16, "semantic": "SHADOW_MODEL_VIEW" },
            { "name": "cameraPosition",     "type": "vec3", "offset": 64, "semantic": "CAMERA_POSITION" },
            { "name": "frameTimeCounter",   "type": "float","offset": 76, "semantic": "FRAME_TIME" },
            { "name": "shadowLightPosition","type": "vec3", "offset": 80, "semantic": "SHADOW_LIGHT_POSITION" }
          ] }
      ],
      "vertexInputs": [
        { "name": "iris_Position", "location": 0, "format": "R32G32B32_FLOAT" },
        { "name": "iris_UV0",      "location": 2, "format": "R32G32_FLOAT" }
      ],
      "capabilities": [],
      "diagnostics": [ { "code": "SX0201", "severity": "note", "message": "unknown program skipped: clrwl_gbuffers" } ]
    }
  ],
  "capabilities": { "requires": ["mrt8"], "missing": [] },
  "diagnostics": []
}
```

### 5.3 字段契约

| 字段 | 谁消费 | 语义 |
| --- | --- | --- |
| `programs[].group/index` | 宿主 pass 排序 | 与 Iris 的 `ProgramGroup`/`ProgramArrayId` 同名同序（§2.2），保证"包作者的直觉"成立 |
| `programs[].status` | 宿主决定是否执行 | `ok` = 全部阶段可编译；`partial` = 部分阶段缺失/未发射（如 compute 需 UAV）；`skipped` = 认得但没实现（`clrwl_*`/`voxy_*`），**必须伴有一条诊断** |
| `programs[].fallbackFrom` | 宿主回退 | 该程序是从哪个程序回退来的（§2.2 的回退链），非空说明本体文件不存在 |
| `mrt.targets` | 宿主建 RTV | **有序**列表；`targets[i]` 是槽 `i` 的 colortex 编号。这就是 `DRAWBUFFERS:03648` 的机器可读形态 |
| `renderTargets[]` | 宿主建纹理 | 只列**本程序实际用到**的目标；格式缺省 `RGBA8`、`clear` 缺省 true、清屏色缺省 `[0,0,0,0]` |
| `samplers[]` | 宿主绑 SRV/采样器 | `name` 是**包源码里的标识符**（可能是 `colortex3`/`gcolor`/`noisetex`）。`binding` 写进 GLSL 是必须的（§2.7 结论 2），但**它的数值在 M1 下不权威**：主体会按 resource 发现顺序把 binding 重写成 entries 下标（`D3D12IntermediaryShaderModule.kt:114-140`），真正可靠的对应关系是 `name` + 宿主自己的 reflection（§2.7 结论 8） |
| `samplers[].comparison` | 宿主选采样器类型 | `true` ⇒ 宿主必须给这个采样器绑 comparison 采样器（`D3D12_FILTER_COMPARISON_*`），否则 `.SampleCmp` 在运行时是未定义行为。`sampler2DShadow` 一律 `true`（§2.7 结论 7） |
| `uniformBlocks[]` | 宿主填 CBV | 偏移是**我们自己算的 std140**，不是反射来的；宿主按 `semantic` 填值，未知 semantic 填 0 |
| `vertexInputs[]` | 宿主管线 | 与 §4.7 固定表一致；宿主缺某 format 时据此报 `SX0504` |
| `capabilities` | 宿主选择档位 | 见 §1.2 的 L0/L1/L2c；这是"后端架构派生自能力表"的落点 |

**三条稳定性承诺**（写进 `plan.json` 顶部注释，也是版本兼容策略）：

1. 只**追加**字段，不重命名、不改变已存在字段的语义；`schemaVersion` 整数递增。
2. 数组顺序稳定（程序按 `group` 枚举序、目标按下标、块按 binding、成员按 offset）。
3. 未知字段宿主必须忽略（`#[serde(default)]` 对侧同理）。

### 5.4 与 M1 的关系

M1（只做 GLSL 450）**就已经产出完整 `plan.json`**。这一点很重要：宿主可以在"还不接受 HLSL"的时候，先用 `plan.json` 把 MRT、colortex 池、pass 顺序搭起来——**渲染图工作与 HLSL 工作完全并行，且互不阻塞**。

## 6. 三个后端，一个方言层

### 6.0 为什么是三个而不是一个

v2.0 把"用不用 naga"当成了路线选择。实测下来这个二选一是**伪问题**（§2.4）：naga 的 GLSL 前端只吃 Vulkan 口味的 450，而我们要吃的是 130/compatibility + Iris 方言。所以真正的结构是：

```
                  ┌────────────────────────────────────────────┐
   光影包源码 ──→ │ 方言层（§4）：发现/拼接/预处理/规范化/R1–R11 │ ──→ 归一化 GLSL 450
                  └────────────────────────────────────────────┘         │
                                                                         ├─→ M1：宿主既有链（shaderc → spvc → DXC 6.9）
                                                                         ├─→ M2：SPIR-V 中转（spvc/回归基准）
                                                                         └─→ M3：naga IR → hlsl-out（可选演进）
```

**方言层是唯一的必经之路**，三个后端只是"归一化 GLSL 450 的下游消费者"。这带来两个好处：

- M1 可以**今天**就接进主体，完全不碰 naga 的坑；
- M3 万一走不通（§6.3 的 sampler heap），损失的是"可选的现代化"，不是整个方案。

### 6.1 M1：宿主既有链（**主路线**）

**契约**：`emit_glsl450` 产出的单文件 GLSL 450 core，直接喂给 `D3D12ShaderCompiler.createIntermediary(filename, source, type, includeResolver)`（§2.1）。`includeResolver` 传 `null`——include 已经在阶段 4 拼完。

**必须遵守的四条**（否则会被既有链的假设打翻）：

| # | 约束 | 依据 |
| --- | --- | --- |
| M1-1 | **保留 mixed sampler 声明**（`uniform sampler2D colortex3;`），**不拆分**，但必须写 `layout(binding=N)` | 450 的混合采样器合法；SPIRV-Cross 会自动拆成 `Texture2D` + `SamplerState <name>_sampler`，与主体既有行为一致。缺 binding 会被 glslang 直接拒（§2.7 结论 2）；binding 的**数值**不权威（§2.7 结论 8）。拆分是 M3 的输入约束，见 §6.3 |
| M1-2 | 片元输出**必须**带 `layout(location=)`，顺序即 MRT 槽 | spvc 按 location 生成 `SV_TargetN`；`set_auto_map_locations(true)` 只补未标注的变量 |
| M1-3 | 不产出 `#include` / `#moj_import` / `#extension` | 宿主链的 include 展开只认 vanilla 的 `#moj_import`（§2.1），混用会重复展开 |
| M1-4 | 顶点输入带 `layout(location=)` 且与 §4.7 表一致 | spvc 的 `hlsl_add_vertex_attribute_remap` 按 location 映射，顺序错位会静默取错属性 |

**`R-M1-1` 已结案**（原计划是"造最小着色器看 spvc 是否重排 location"）：§2.7 用 2 槽 MRT + 5 个顶点输入 + `flat` 插值 + 比较采样器做完了这件事，`SV_Target0/1` 与 `TEXCOORD0..4` 严格按 source 的 location 对应，`ps_6_9`/`vs_6_9` 都通过，**没有观察到重排**。同一批实验还顺带确认了 `packoffset` 与我们的 std140 计算一致（§2.7 结论 6）。

**HLSL 文本补丁的影响**：主体对 HLSL 做四类 needle 补丁（§2.1），其中 `void frag_main()\n{` 用来插纹理坐标翻转。§2.7 实测确认**产物里这个 needle 仍然存在**（spvc 把用户 `main()` 改名为 `frag_main()` 是稳定行为，与我们的规范化无关），所以 R6 风险从"中"降为"低"。仍需保留的检查：M1.5 实机验收时 grep 一次 `[FIX] WARN: frag_main needle not found`，若出现说明 spvc 版本换了命名策略，此时在 `plan.json` 里显式告知宿主"此 pass 需要翻转"作为兜底。

### 6.2 M2：SPIR-V 中转（**回归基准，非生产路径**）

不新增任何宿主依赖：用 `glslangValidator -V` 把产物编成 SPIR-V，再用 `spirv-cross --hlsl --shader-model 51` 转 HLSL，最后用 `dxc -T ps_6_9` 校验。三件工具在 CI 里即可获得（Linux 也跑得动）。**这条链不是纸面方案**：§2.7 已经用它把 M1 的手写样本一路跑到 DXIL，三个工具全在本机 Vulkan SDK 里，命令见 §13.2。

它的价值不是"给宿主用"，而是：

1. **在没有 Minecraft 的情况下证明 M1 产物可用**——这是 CI 能跑的最强断言；
2. 给规则改动提供**逐文件的 diff**（HLSL 级别，比 GLSL 级别更能暴露语义漂移）；
3. 复现主体链的行为差异（若 M2 能过而实机不能过，问题在宿主侧而非本模块）。

**明确不做**：本模块不链接 SPIRV-Cross，也不内嵌 glslang。M2 是 CLI 的一个子命令（`sx-cli verify --backend spirv`，外部调用工具链，缺工具则跳过并 `Note`）。

### 6.3 M3：naga 直连（**可选演进，有明确前置研究项**）

M3 的愿望是"模块自己产出 HLSL，宿主不再依赖 SPIRV-Cross"。实测障碍（§2.4）按严重度排列：

| 障碍 | 严重度 | 处置 |
| --- | --- | --- |
| 只吃 Vulkan 450（`InvalidVersion(130)`） | 低 | 方言层已保证是 450 |
| 散装 `uniform` 必须 `layout(binding=)`；`sampler2D` 直接 `NotImplemented` | 中 | 方言层已保证：块化（R8，匿名块已实测可过）+ 拆分为 `texture2D` + `sampler`（M3 专有变体 R4′/R4″） |
| 需区分"块内成员"与"独立 uniform"的语义 | 低 | R8 已用匿名块（实测通过）；只有 compat 块 `iris_transforms` 用具名实例 |
| **每个采样器都被编译成 sampler heap 的间接寻址** | **高** | 见下 |
| `glsl-in` 与 `hlsl-out` 单独开启无法编译（上游 cfg bug） | 低 | 必须同时开 `wgsl-in`，写进 `Cargo.toml` 注释 |

**M3 专有的两条附加规则**（只在 `target = naga_subset` 时启用；M1 **不启用**）：

- **R4′（拆分）**：`uniform sampler2D X;` → `layout(binding = 100+i) uniform texture2D sx_tex_X;` + `layout(binding = 200+i) uniform sampler sx_smp_X;`。`sampler2DShadow` → `texture2DShadow` + `samplerShadow`（naga 的官方测试里没有 shadow 版本的样例，属 spike 项）。
- **R4″（调用点重写）**：`texture(X, uv)` → `texture(sampler2D(sx_tex_X, sx_smp_X), uv)`；`textureLod` / `textureProj` / `textureGrad` 同理，把第一个实参包成 `sampler2D(...)`。
- **为什么 M1 不启用**：GLSL 450 允许 combined sampler，拆分只会让宿主多出一倍绑定与采样器描述；而 M3 不拆就 `NotImplemented("variable qualifier")` 直接失败（§2.4）。

**sampler heap 问题（M3 的真正门槛）**：naga 30 的 HLSL 后端把每个 `SamplerState` 写成

```hlsl
SamplerState nagaSamplerHeap[2048] : register(s0, space0);
static const SamplerState colortex3_sampler = nagaSamplerHeap[nagaGroup0SamplerIndexArray[3]];
```

并额外要求一个 `StructuredBuffer<uint>` 索引表（`sampler_buffer_binding_map` 未提供时会 `unreachable!`，或退化成 `space=255` 的悬空 register）。这对宿主意味着"必须持有一个 2048 项的采样器堆 + 一张索引 buffer"，而 DXRMC 今天用的是 **static samplers**（§2.1）。让宿主为 M3 改造描述符策略，违背了 §1.2 的解耦契约。

三条出路，按代价排序：

| 方案 | 做法 | 代价 | 判断 |
| --- | --- | --- | --- |
| M3-a | HLSL 后处理：把 `nagaSamplerHeap[...]` 的赋值改写成 `static const SamplerState X = { Filter = ...; AddressU = ...; };`（状态由 `plan.json` 的 sampler 描述给定） | 纯文本 pass，宿主零改动 | **推荐**，但要 golden 锁住 naga 输出形态 |
| M3-b | 宿主实现采样器堆 + 索引 buffer | 宿主描述符策略重写，`plan.json` 再加字段 | 与解耦契约冲突，否决 |
| M3-c | fork naga 改 `write_global_sampler` | 长期维护 fork，与 MIT/Apache 兼容但版本跟进成本高 | 仅当 M3-a 不稳定时考虑 |

**结论：M3 的定位是"可选演进"，前置研究项是 M3-a 的可行性验证（1 天，纯文本 pass + 一个 golden）。在 M1 实机通过之前不投入 M3。** v2.0 把 naga 当核心组件是错误的前提，本版纠正为"naga 是可选后端之一"。

### 6.4 选择矩阵

| 需求 | M1 | M2 | M3 |
| --- | --- | --- | --- |
| 今天就能在主体里跑 | ✅ | ❌（CI 用） | ❌ |
| 证明归一化正确 | ✅（间接） | ✅✅ | ✅ |
| 不依赖 SPIRV-Cross | ❌ | ❌ | ✅ |
| 支持 compute 的 HLSL 输出 | ❌ | ✅（spvc 限制同主体） | ✅ |
| 风险 | 低 | 低 | 高（依赖上游内部结构） |

## 7. C ABI

保留 v2.0 §9.1 的全部生命周期约束（版本化结构体、`struct_size`、库内分配库内释放、panic 不得穿越、句柄不可复制、只能追加字段），只重做**函数面**与**数据形状**。

### 7.1 函数面（5 个函数，够用且不再增长）

```c
// 全部返回 int32_t：0 = 成功，负数为 SxStatus，正数为"部分成功"（有诊断但产物可用）
int32_t sx_abi_version(void);                       // 期望 == 1

// 打开一个包：扫描 + 发现 + 预处理环境，返回句柄。plan 可由 plan_json 取。
int32_t sx_pack_open(const uint8_t* root_utf8, uintptr_t root_len,
                     const uint8_t* options_json, uintptr_t options_len,
                     SxHandle* out_handle);

void    sx_pack_close(SxHandle handle);

// 取整包 plan（§5 的 JSON）；或按程序取（program 传 NULL 表示整包）
int32_t sx_plan_json(SxHandle handle,
                     const uint8_t* program_utf8, uintptr_t program_len,
                     SxBytes* out);

// 编译单个阶段：产出归一化 GLSL 450（M1/M2）或 HLSL（M3，由 options_json 选）
int32_t sx_compile(SxHandle handle,
                   const uint8_t* program_utf8, uintptr_t program_len,
                   int32_t stage,                        // 0=vertex 1=fragment 2=compute
                   const uint8_t* options_json, uintptr_t options_len,
                   SxBytes* out);

// 无包裸编译（诊断/CLI 用，也是宿主的"应急单文件"入口）
int32_t sx_normalize(const uint8_t* source_utf8, uintptr_t source_len,
                     const uint8_t* options_json, uintptr_t options_len,
                     SxBytes* out);

void    sx_bytes_free(SxBytes* bytes);              // 与库内分配配对
const char* sx_version_string(void);
```

```c
typedef struct { const uint8_t* ptr; uintptr_t len; } SxBytes;
typedef struct { uint32_t struct_size; uintptr_t opaque; } SxHandle;
```

**为什么是 JSON 进 JSON 出**：宿主是 Kotlin/FFM（§2.1 的 `MCDR_ABI` 范本），跨边界传结构化数据的最短路径是字符串；而且 schema 演进不需要动 ABI（§5.3 的追加承诺）。大文本（HLSL/plan）走 `SxBytes` 以免二次拷贝。

**线程与并发**：`SxHandle` 内部持锁，**允许**多线程并发 `sx_compile`，**不允许**并发 `sx_pack_close`。这一条写进头文件注释，并在 debug 构建里断言。

### 7.2 与解耦契约的对应

| 契约（§1.2） | ABI 上的体现 |
| --- | --- |
| 主体不链接本模块 | 没有静态库、没有 `build.rs` 生成绑定；宿主用 `Linker.nativeLinker().load(...)` 反射加载，失败即功能不可用 |
| 宿主不感知光影包格式 | ABI 里没有任何 `iris`/`colortex`/`DRAWBUFFERS` 字样；全部语义藏在 `plan.json` 的 `semantic` 枚举里 |
| DLL 位置不劫持主体 | 路径由宿主配置项给出（`config/dxr_mc-client.toml` 的 `shaderpack.libraryPath`），**不放进主体 `dxrmc/` 抽取目录**，避免与 `MCDR.dll` 的加载顺序耦合 |
| 开关走 NeoForge 配置 | 宿主侧新增 `[shaderpack] enabled/libraryPath/cacheDir/target` 四项（AGENTS.md 的"三处都要有"清单）；模块本身**零环境变量** |

## 8. 缓存与失效

### 8.1 键

```
blake3( abi_version ‖ module_version ‖ target(M1|M2|M3) ‖
        program_name ‖ stage ‖ env_macro_set_hash ‖ options_hash ‖
        content_hash(spliced_source_after_include) )
```

- 用**拼接后的内容**而不是入口文件，这样 include 改动自动触发失效（不需要维护反向依赖图）。
- `env_macro_set_hash` 覆盖 `MC_*`/维度/程序宏/用户选项（§2.2 的六步里的第 5 步），因为同一份源码在不同宏组合下产物不同。
- 不含时间戳、不含绝对路径（可移植的缓存）。

### 8.2 存储

| 层 | 位置 | 内容 |
| --- | --- | --- |
| 内存 | `SxHandle` 内 LRU（默认 256 项） | 编译产物 |
| 磁盘 | 宿主传入的 `cacheDir`，布局 `<cacheDir>/shaderpack/v1/<key前2>/<key>.{glsl,json}` | 产物 + 对应的 `Plan` 片段 |
| 落盘内容 | 只落**成功**产物与**诊断**，不落异常 | — |

磁盘缓存**必须**带 `key` 校验（读时重算，不匹配即丢弃），因为缓存目录是用户可写区域（§10 的安全性要求）。

### 8.3 失效语义

| 变化 | 影响 |
| --- | --- |
| 包内任意被 include 的文件 | 该程序失效（内容 hash 变化） |
| `shaders.properties` / `block.properties` / `dimension.properties` | 环境宏变 → 全部失效 |
| 用户编辑 shader option（`#define X 2 //[...]`） | 环境宏变 → 相关程序失效（由调用方传 options 决定粒度） |
| 模块版本 / ABI 版本 | 全失效 |
| `target` 变化（M1↔M3） | 全失效 |

## 9. 测试与语料治理

### 9.1 三层测试

| 层 | 语料 | 是否入库 | 断言 |
| --- | --- | --- | --- |
| 单元 | 手写片段（每规则 3–5 例） | ✅ | 规则前后 token 序列精确相等；幂等 |
| golden | `sx-golden` 合成 mini-pack（含 include 环、阶段宏、多 MRT、选项注释） | ✅ | 产物全文快照 + `plan.json` 快照；**每例都用 §2.7 的三命令链跑到 DXIL**（失败即 golden 失败，不需要 Minecraft） |
| 语料回归 | Complementary Unbound r5.8.1 | ❌（许可证） | 统计量 + 每条规则命中数 + 编译成功率 |

第一、二层的编译断言就是 §2.7 那条链：`glslangValidator -V --target-env vulkan1.2` → `spirv-cross --hlsl --shader-model 51` → `dxc -T ps_6_9`。它同时也是唯一能在 CI 里证明"产物真的能被 DXIL 级工具接受"的手段，比"文本相似"强得多。

**合成 mini-pack 是必须的**：真实语料不能入库，而"Golden 测试依赖一个不能入库的目录"等于没有测试。mini-pack 要覆盖 §2.3 里出现过的每一种结构，20 个文件以内。

### 9.2 语料治理

```toml
# corpus.lock（入库）
[corpus]
name     = "ComplementaryUnbound"
version  = "r5.8.1"
files    = 408
lines    = 18978
blake3   = "…"      # 全树内容指纹
[[corpus.root]]
path = "input/ComplementaryUnbound_r5.8.1"
```

- `input/` 进 `.gitignore`（当前是 untracked，本版明确写进去）；
- `corpus.lock` 入库，CI 里没有语料则**跳过并打印 `skip: corpus absent`**，不失败；
- `sx-cli survey` 复现 §2.3 的统计表（构造 → 次数），这是文档数字的唯一来源，随时可重算。

### 9.3 实机验证的边界（写给未来的自己）

以下判断**必须交给用户肉眼确认**，不得用像素统计自证（本项目已有先例：P69 修复前后像素统计几乎一致，但用户看到的是明显差异）：

- 颜色是否正确（HDR/色调映射差异会让统计量趋同）；
- 是否有残影、闪烁、抖动（帧间相关，静态截图看不出来）；
- 阴影是否对齐、水面是否正常、天空是否过曝。

交付方式：`sx-cli` 生成一组固定场景（默认视角/日落/雨天/水下/手持），用户按场景比"有光影 vs 无光影"，结论以用户判断为准。

## 10. 里程碑与决策门

| 里程碑 | 内容 | 验收 | 估时 |
| --- | --- | --- | --- |
| **M0** | workspace、lexer、R1/R2/R5、`sx-cli survey` | 408 文件全部 tokenize 成功、零 panic；survey 数字与 §2.3 一致 | 1 周 |
| **M1** | R3/R4/R6、uniform 块化（R8）、`emit_glsl450`、`plan.json`、C ABI | 全部程序产出 450 GLSL；`glslangValidator -V` 通过率 ≥ 95%；**抽样再跑 `spirv-cross` + `dxc -T ps_6_9/vs_6_9`**（§2.7 的链，已证明可跑通）；失败逐条归类 | 2–3 周 |
| **M1.5** | 宿主接 1 个 pass（建议 `final`，单目标、无顶点属性） | 实机 A/B：开启后画面与 vanilla 一致或按包意图变化；无 WARN 补丁失效 | 1 周（宿主侧） |
| **M2** | SPIR-V 回归后端、HLSL 差分 | CI 可跑；规则改动的 diff 可读（三个工具已在 §2.7 验证可用） | 1 周 |
| **M3** | sampler heap 消解研究（M3-a） | 一个程序的 HLSL 能被 `dxc` 编译且常量/采样行为正确 | 1 周（研究），成败不定 |

**决策门（v2.0 的门重新定义）**：

| 门 | 问题 | 判据 | 不通过怎么办 |
| --- | --- | --- | --- |
| **VG5′** | 归一化是否真能覆盖语料？ | M1 的 `glslangValidator` 通过率 | < 90% 时：先补齐最高频的失败构造（survey 排序），仍不行则**降级为非目标**并只支持 L0 |
| **VG6** | 宿主能否建 MRT + colortex 池？ | `plan.json` 能被宿主消费并实际建出 2 个以上 RTV | 不通过则 M1.5 只能做单目标 pass（`final`/`composite7`），L1 全线阻塞 |
| **VG7** | 顶点属性扩展（复用 v2.0 的 VG4） | `mc_Entity`/`at_tangent` 能否进顶点格式 | 不通过则退化为"常量属性 + `SX0504`"，效果降级但可跑 |
| **VG8** | naga 直连是否值得？ | M3-a 的 golden 稳定性 | 不通过则永久放弃 M3，保留 M1/M2 |

依赖关系：M1 不依赖 VG6（产物与渲染图解耦）；M1.5 依赖 VG6；L1 依赖 VG6 + VG7；M3 依赖 VG8。

## 11. 风险

| # | 风险 | 概率 | 影响 | 触发信号 | 对策 |
| --- | --- | --- | --- | --- | --- |
| R1 | naga 前端不可用 | **已发生** | 高 | `InvalidVersion`/`NotImplemented` | 方言层承担，M1 绕开（§6.1） |
| R2 | 宿主不接 MRT/colortex | 高 | 中 | `plan.json` 被读取但没有 RTV | `plan.json` 先交付，渲染图工作并行；L0 仍可交付 |
| R3 | 语料是 26.3 的，参考实现是 26.1 | **已发生** | 中 | `clrwl_*`/`voxy_*`/`pack.json` 未知 | 未知程序跳过 + 诊断；不臆造语义（§2.3） |
| R4 | 规则过拟合 Complementary | 中 | 高 | 换包后大面积失败 | mini-pack + 用户提供第二套语料（建议 BSL 或 SEUS，需用户授权本地使用） |
| R5 | `auto_map_locations` 与显式 location 冲突 | 低 | 中 | MRT 顺序错位 | **已由 §2.7 结案**：2 槽 MRT + 5 个顶点输入 + `flat` 插值 + 比较采样器全部按 source location 正确落到 `SV_TargetN`/`TEXCOORDn`，未观察到重排 |
| R6 | HLSL needle 补丁失效 | 低 | 中 | `[FIX] WARN: frag_main needle not found` | **已由 §2.7 结论 9 降低**：`void frag_main()` 仍在产物里；既有代码会降级为 WARN，必要时在 `plan.json` 里加 `requiresYFlip` |
| R7 | 许可证 | 低 | 高（法律） | 语料/代码被入库 | §2.6 矩阵 + `input/` gitignore + CI 检查（见下） |
| R8 | ABI 漂移 | 低 | 中 | 宿主读不到字段 | 追加式演进 + `abi_version` + 未知字段忽略 |
| R9 | 全量编译太慢（408 文件） | 低 | 低 | `sx-cli survey` 超时 | 并行 + 缓存；实测目标 < 30s 全量 |

**许可证的自动化检查**（R7 的对策必须能执行）：CI 里加一条 `git ls-files | grep -E '^Illusion-shaders-extension/(input|ref_shader_loader)/'`，有输出即失败。这条比文档里的"请注意"有用得多。

## 12. 交付清单

### 12.1 本模块新增（全部落在 `Illusion-shaders-extension/` 内）

| 类别 | 内容 |
| --- | --- |
| 代码 | `crates/sx-core`（§3.2 的模块）、`crates/sx-abi`、`crates/sx-cli`、`crates/sx-golden` |
| 文档 | 本文件；`docs/dialect-rules.md`（R1–R11 的可执行规格，规则表由测试生成）；`docs/plan-schema.md`（§5 的机器可读版） |
| 测试 | 单元 + golden + `corpus.lock` + CI 许可证检查 |
| 构建 | `Cargo.toml`（workspace，`resolver="3"`，`naga` 只在不默认的 feature 里）、`rust-toolchain.toml`（锁 1.98.1，与实测环境一致） |
| 忽略 | `.gitignore` 增加 `input/`、`ref_shader_loader/`、`target/` |

### 12.2 主体侧（**非必要不动**，且全部为 additive）

| repo | 改动 | 必要性 |
| --- | --- | --- |
| Illusion | `IllusionConfig` 增 `[shaderpack]` 四项 | 需要（否则无开关，违反 AGENTS.md） |
| Illusion | 一个 `ShaderPackBridge`（FFM 加载 + `sx_*` 调用 + 失败降级） | 需要（否则模块不可达） |
| Illusion | `ShaderSource.get` 的一个实现（把 `sx_compile` 结果按 `Identifier` 返回） | 需要（M1 的接触面） |
| MCDR | **无** | — |
| Illusion | MRT/colortex 渲染图 | 只在 VG6 通过后做，属于主体既有 roadmap（`vanilla_shader_plan.md` 的"包驱动渲染图"），**不是本模块的一部分** |

**"非必要不改"的具体含义**：上述前三项加起来不超过 3 个文件、不动渲染管线、不动 `D3D12ShaderCompiler` 的任何一行。如果实施过程中发现要改 `D3D12ShaderCompiler`，那说明 M1 的契约（§6.1）没守住，应回头改模块而不是改主体。

## 13. 附录

### 13.1 证据索引（关键 `path:line`）

| 结论 | 证据 |
| --- | --- |
| 宿主着色器链无 glslang、无中间 IR | `Illusion/.../shader/D3D12ShaderCompiler.kt`（shaderc→spvc）、`MCDR/src/core/d3d12_device.cpp:5637`（DXC） |
| 编译器入口是纯 String | `Illusion/.../shader/D3D12ShaderCompiler.kt:58-59` |
| 绑定按出现顺序分配 | `.../D3D12IntermediaryShaderModule.kt:114-140`、`.../D3D12Device.kt:811`、`d3d12_device.cpp:5664` |
| HLSL needle 补丁 | `D3D12ShaderCompiler.kt:188-196/212-219/252-272`、`mesh/D3D12MeshletShader.kt:48-52` |
| FFM 范本 | `.../d3d12/extern/MCDR_ABI.kt:32-33/277-278/419-424` |
| Iris 指令取"最后一次" | `Iris/common/.../parsing/CommentDirectiveParser.java:26-60`、`properties/ProgramDirectives.java:149-161` |
| 指令在 include+预处理之后采集 | `Iris/common/.../shaderpack/ShaderPack.java:286-320`、`ProgramDirectives.java:115/124-130` |
| `gl_ModelViewMatrix` 的区块偏移 | `Iris/common/.../transformer/VanillaTransformer.java:345-368` |
| 合成 pass 的 MV 是单位矩阵 | `Iris/common/.../transformer/CompositeTransformer.java:78` |
| `DRAWBUFFERS` 是注释 | `ProgramDirectives.java:54-74`；`CommentDirectiveParser` 的类内测试 `:84-151` |
| colortex 上限 32 | `Iris/common/.../gl/IrisLimits.java:11` |
| naga sampler heap | `naga-30.0.1/src/back/hlsl/help.rs:2027-2053`、`writer.rs:1224-1263` |
| naga 只吃 450 | `naga-30.0.1/src/front/glsl/mod.rs` 文档头 |
| naga 拒绝 `sampler2D` 声明 | `naga-30.0.1/src/front/glsl/parser/declarations.rs:444-450` |
| naga **接受**匿名 uniform 块 + 裸成员访问（R8 的依据） | 本机 spike 实测：偏移 `mvp@0 / tint@64`，HLSL `cbuffer global : register(b0)` |
| naga 分离采样器需 `sampler_buffer_binding_map`，否则 `unreachable!` | `naga-30.0.1/src/back/hlsl/help.rs:2088` |
| naga 分离采样器官方样例 | `naga-30.0.1/src/front/glsl/parser_tests.rs:346-358` |
| **M1 契约端到端通过**（GLSL450→SPIR-V→HLSL→DXIL `ps_6_9`/`vs_6_9`） | §2.7；样本 `docs\spike\composite5.frag`、`docs\spike\gbuffers_terrain.vert` |
| glslang 要求块与采样器都写 binding | §2.7 结论 2：`uniform/buffer blocks require layout(binding=X)`、`sampler/texture/image requires layout(binding=X)` |
| 450 core 下 `texture2D` 不是函数 | §2.7 结论 3：`'sampler/image' : cannot construct this type` |
| spvc 的 `packoffset` 与我们的 std140 计算一致 | §2.7 结论 6：`c0/c4/c4.w/c5/c5.w/c6/c6.z` |
| `sampler2DShadow` → `SampleCmp`，DXIL 通过 | §2.7 结论 7 |
| `void frag_main()` needle 稳定存在 | §2.7 结论 9（样本 HLSL 第 71-72 行） |
| 主体分层文化 | `vanilla_shader_plan.md` §0.2、§2.6 |

### 13.2 复现命令

```powershell
# 语料统计（§2.3 的数字来源）
cd Illusion-shaders-extension
cargo run -p sx-cli -- survey input/ComplementaryUnbound_r5.8.1

# 归一化 + 验证（不需要 Minecraft）
cargo run -p sx-cli -- normalize --pack input/ComplementaryUnbound_r5.8.1 --all --out target/norm
cargo run -p sx-cli -- verify   --in target/norm --backend glslang    # 需要 glslangValidator
cargo run -p sx-cli -- verify   --in target/norm --backend spirv      # M2，需要 spirv-cross + dxc

# 单程序 + plan
cargo run -p sx-cli -- compile --pack input/... --program composite5 --stage fragment --emit glsl450 --plan
```

**§2.7 的验证链本身**（不依赖本模块；样本在 `docs\spike\`，工具在 Vulkan SDK 的 `Bin\` 下）：

```powershell
cd Illusion-shaders-extension\docs\spike
glslangValidator -V --target-env vulkan1.2 -S frag -o composite5.frag.spv composite5.frag
spirv-cross composite5.frag.spv --hlsl --shader-model 51 --output composite5.hlsl
dxc -T ps_6_9 -E main -Fo composite5.dxil composite5.hlsl     # vertex 用 -S vert / vs_6_9
```

### 13.3 naga 30.0.1 的报错原文（用于诊断映射）

| 输入 | 结果 |
| --- | --- |
| `#version 130` | `InvalidVersion(130)` |
| `uniform sampler2D s;` | `NotImplemented("variable qualifier")`，span 覆盖 `sampler2D` |
| `uniform vec3 c;` | `SemanticError("uniform/buffer blocks require layout(binding=X)")` |
| 三个未标注 location 的 `in` | 校验 `BindingCollision { location: 0 }` |
| 两个 `layout(location=0/1) out vec4` | 成功，HLSL `SV_Target0`/`SV_Target1` |

### 13.4 与 v2.0 的章节映射

| v2.0 | 本版 | 变化 |
| --- | --- | --- |
| §1 设计决策 / §3 先行技术审计 | §0、§2 | 前提被实测推翻，重写 |
| （v2.0 无对应） | §2.7 | **新增**：M1 契约的端到端离线验证（GLSL450 → DXIL `ps_6_9`/`vs_6_9`），把"推理"变成"实测" |
| §4 Rust Workspace | §3.2 | 保留多 crate 思想，改为 `sx-*` 命名与单向依赖 |
| §5 核心数据流 | §3.1 | 14 步保留，补失败变体与顺序约束 |
| §6 兼容层 | §2.2、§4 | 从"设计 VFS"改为"照 Iris 的 6 步与指令规则实现" |
| §7 Cyntax 与 Naga 集成 | §6 | 删除 cyntax 改造；naga 降级为可选后端 |
| §8 语义映射 | §4.3–§4.10 | 变成可执行的 R1–R11 |
| §9 C ABI | §7 | 结构保留，函数面重做 |
| §10 诊断反射输出 | §5 | 升级成 `plan.json` 渲染图契约 |
| §11 缓存并发取消 | §8 | 保留，键改成内容拼接 + 宏集 |
| §12 安全性 | §7.2、§8.2 | 并入 ABI 与缓存的约束 |
| §13 测试与语料 | §9 | 补语料治理与许可证自动化 |
| §14 CLI | §12.1、§13.2 | 保留，作为验证主力 |
| §15 分阶段计划 | §10 | 里程碑与决策门重排 |
| §16 风险 | §11 | 新增已发生的 R1/R3 |
| §17 交付清单 | §12 | 区分模块内与主体侧 |
| §18 参考资料 | §13.1 | 改为本地 `path:line` |
| §19 最终建议 | §0.2 | 结论替换 |
