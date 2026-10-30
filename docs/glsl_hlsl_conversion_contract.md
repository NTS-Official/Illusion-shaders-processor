# GLSL → HLSL 转换约定

本文件是转换阶段（原版 Minecraft GLSL → HLSL + 资源清单）的输入/输出契约。宿主（Illusion）按本契约接回时不应需要修改转换器，转换器也不依赖宿主源码。

## 1. 范围

- 输入：一个原版着色器源文件（`core/*.vsh|*.fsh`、`post/*`），其 `#include <minecraft:...>` 闭合在 `shaders/` 目录内。
- 输入：该 pipeline 的宏定义集合与 include 根目录（见 §2）。宏是**输入契约的一部分**，不是转换器的内部细节。
- 输出：一个自包含 HLSL 文件（无 `#include`）与一个 `illusion.shader-manifest/1` JSON（见 §5）。
- 不做：DXC 调用、DXIL 反射、PSO/根签名构造、缓存。这些属于后续阶段，本期只保证“GLSL 能稳定变成可编译的 HLSL 与正确清单”。

## 2. 输入约定

| 项 | 约定 |
| --- | --- |
| 源文件 | `*.vsh`/`*.vert` = vertex，`*.fsh`/`*.frag` = fragment，`*.comp` = compute；也可用 `--stage` 覆盖 |
| include 根 | 传 `shaders/` 目录。`#include <minecraft:name>` 依次尝试 `<root>/include/<name>`、`<root>/<name>`、`<root>/shaders/include/<name>` |
| 宏 | `NAME`（无值）或 `NAME=VALUE`；值按 GLSL 预处理语义做对象式展开，最多 8 轮。无函数式宏 |
| 条件编译 | `#ifdef/#ifndef/#if/#elif/#else/#endif`；`#if` 支持 `defined(X)`、整数算术、比较、位运算、逻辑运算 |
| 入口 | 固定 `main` |
| 着色器模型 | `--shader-model`，默认 `6_0`，上限 `6_9` |

`--defines-file` 接受如下 JSON，匹配规则是 **`default` → 路径后缀命中的条目 → 命令行 `-D`**，后者覆盖前者：

```json
{
  "default": { "OIT_COEFF_COUNT": "8" },
  "core/rendertype_end_portal.fsh": { "PORTAL_LAYERS": "15" },
  "core/oit_composite.fsh": { "OIT": null }
}
```

`null` 表示只定义名字（等价 `-D NAME`）。原版语料所需的最小集合见 `tests/fixtures/corpus-defines.json`。

## 3. 预处理与方言归一

转换器先做预处理（展开 include/宏、丢弃 `#extension`/`#pragma`、`#version 330` 改写为 `#version 450 core`），再按下列顺序做**局部、保语义**的改写。所有改写都以 token 为单位，不做正则替换，也不在生成的 HLSL 上做事后字符串修补。

| 顺序 | 改写 | 原因 |
| --- | --- | --- |
| 1 | `gl_VertexIndex` → `int(gl_VertexIndex)` | naga 把该内建建模为 `u32`，赋给 `int` 会触发 `InvalidStoreTypes` |
| 2 | `const int n = <expr>; arr[n]` → `arr[(<expr>)]` | naga 要求数组长度是常量表达式 |
| 3 | `const T[] a = T[](…)` / `T a[] = …` 补全元素个数 | naga 不接受无长度数组 |
| 4 | `uniform isamplerBuffer X;` → `layout(std430, binding = 48+n) readonly buffer XBuffer { int data[]; } X_buf;`；`texelFetch(X, i).r` → `X_buf.data[(i)]` | naga 与 HLSL 都没有 texel buffer 类型；naga 把只读 SSBO 输出为 `ByteAddressBuffer`（SRV） |
| 5 | `uniform Block { … };` → `layout(binding = N) uniform Block { … };` | naga 要求 uniform block 显式 binding；N 从 0 递增（已有 `layout(binding)` 的块跳过） |
| 6 | `uniform sampler2D X;` → `layout(binding = 16+2i) uniform texture2D X;` + `layout(binding = 17+2i) uniform sampler X_sampler;`（`samplerCube` 同理，用 `textureCube`） | naga 不支持 combined image sampler；两个 binding 必须不同 |
| 7 | 形参 `sampler2D p` → `texture2D p, sampler p_sampler`（紧跟原参数插入），调用点补 `, p_sampler` | naga 的类型表没有 `sampler2D`，但支持 opaque 形参 |
| 8 | `texture(X, …)` 等 → `texture(sampler2D(X, X_sampler), …)`；cube 用 `samplerCube` | naga 只接受显式组合的构造函数 |

第 4 条要求 `texelFetch` 必须带 `.r`/`.x` 分量选择（语料如此）；不带分量或选其它分量是硬错误。降级的缓冲只能按 32 位元素读取，宿主侧必须提供相应的 32 位视图，见 §7。

改写登记进 manifest 的 `warnings`（非空时）与资源表（binding 号）。改写失败（include 找不到、条件未闭合、`#if` 无法求值）是硬错误，不做回退。

`gl_VertexIndex` 以外的内建不做改写；语料实际用到 `gl_Position`、`gl_FragCoord`、`gl_FragDepth`、`gl_FrontFacing`、`gl_PointSize`。

## 4. 输出约定

HLSL 是单文件、自包含的 `Shader Model 6.x` 源码，入口名 `main`，内部实现函数由 naga 生成（`main_1`、`ConstructVertexOutput` 等）。

**资源声明**

- cbuffer：`cbuffer global_<n> : register(b<n>) { <Type> global_<n>; }`，`<Type>` 是 GLSL uniform block 的结构体；按 IR 全局变量顺序编号。
- texture：`Texture2D<float4> X_ : register(t<n>)`、`TextureCube<...>`、`Texture2D<int4>` 等由元素类型决定。
- sampler：`SamplerState X_sampler : register(s<n>)` 或 `SamplerComparisonState`（`static_samplers` 打开时；关闭时 naga 会退回 sampler heap，本转换器永远打开）。
- storage buffer：降级的 buffer texture（只读）输出为 `ByteAddressBuffer X_buf : register(t32+n)`，按 32 位元素 `Load`；可写 storage buffer 由 naga 输出 `RWByteAddressBuffer : register(u<n>)`，语料尚未出现。
- 四类寄存器各自从 0 独立计数，`space = 0`；storage buffer 从 32 起，因此与 texture 的 `t0..` 不冲突。

**布局**

- cbuffer 成员按 std140 语义排布，naga 会在结构体尾部插入 `int _end_pad_N` 补齐。宿主按 std140 写入即可，不要按 HLSL 的 16 字节打包重新排列。
- 常量缓冲区的 `register` 与 GLSL 的 `binding` 无对应关系；宿主以 manifest 的 `register` 为准。

**stage IO**

- `layout(location = N) in/out` → `LOC<N>` 语义；`gl_Position` → `SV_Position`，其余内建按 HLSL 常规（`SV_Depth`、`SV_IsFrontFace`、`SV_VertexID` 等）。
- VS 的输出结构体与 PS 的输入结构体由各自着色器的语义生成；同一 `LOC<n>` 的语义名与类型一致才能配对。字段名不参与配对。
- 标识符可能被 naga 重命名（例如与生成名冲突时追加 `_`，`Sampler2` → `Sampler2_`）。manifest 记录 IR 名；**以 HLSL 里的声明文本为准**。

## 5. manifest schema

`schema = "illusion.shader-manifest/1"`。

```json
{
  "schema": "illusion.shader-manifest/1",
  "shader": { "name": "terrain.vsh", "stage": "vertex", "entry": "main", "hlsl_file": "terrain.vsh.hlsl", "shader_model": "6_0" },
  "resources": {
    "textures": [ { "name": "Sampler2", "hlsl_type": "Texture2D<float4>", "register": 0, "space": 0, "group": 0, "binding": 16 } ],
    "samplers": [ { "name": "Sampler2_sampler", "register": 0, "space": 0, "group": 0, "binding": 17, "comparison": false, "texture": "Sampler2" } ],
    "constant_buffers": [ { "name": "constants_Fog", "hlsl_type": "Fog", "register": 0, "space": 0, "group": 0, "binding": 0, "members": [ { "name": "FogColor", "hlsl_type": "float4" } ] } ],
    "storage_buffers": [ { "name": "CloudFaces_buf", "hlsl_type": "ByteAddressBuffer", "register": 32, "space": 0, "group": 0, "binding": 48, "members": [ { "name": "data", "hlsl_type": "int" } ] } ]
  },
  "stage_io": {
    "inputs": [ { "name": "Position", "location": 0, "hlsl_type": "float3", "interpolation": "Perspective" } ],
    "outputs": [ { "name": "texCoord0", "location": 3, "hlsl_type": "float2", "interpolation": "Perspective" } ],
    "builtins": [ { "name": "gl_Position", "builtin": "Position { invariant: false }", "hlsl_type": "float4" } ]
  },
  "warnings": []
}
```

字段约定：

- `group` = GLSL 的 `set`（原版恒为 0）；`binding` = 转换器注入的 binding（块从 0 起、texture `16+2i`、sampler `17+2i`、降级的 buffer texture `48+n`）；`register`/`space` = 实际 HLSL 声明。
- `members` 只记录名字与 HLSL 类型，**不记录字节偏移**。偏移由 std140 与 HLSL 的对应关系决定，需要机器可读偏移时用 DXC reflection（M2）；不要拿这里的数据自行猜布局。
- `interpolation` 为 `null` 表示该 IO 用了默认（`smooth`/`Perspective`）。
- `warnings` 目前恒为空数组，保留给“改写发生但语义仍然成立”的提示。

输出是确定性的：同一输入字节 + 同一 defines 集合 → 相同 HLSL 与 manifest（defines 用 `BTreeMap`，资源按 IR 顺序）。

## 6. 失败模式与已知限制

错误按阶段分类，消息前缀固定：

- `preprocess`：include 缺失/循环、未闭合条件、`#if` 求值失败。
- `dialect`：无法识别的 sampler 声明形态。
- `GLSL frontend rejected the shader`：naga 前端拒绝（通常是缺少宿主宏，或用了 naga 不支持的 GLSL 构造）。
- `shader failed validation`：naga 校验失败（IR 不合法）。
- `HLSL backend failed`：后端能力缺口。

原版 26.3 语料（72 个 shader）在给定 `tests/fixtures/corpus-defines.json` 时 **71 个可转换**，1 个为已知能力缺口：

| 着色器 | 原因 | 处理 |
| --- | --- | --- |
| `core/debug_point.vsh` | `gl_PointSize`；HLSL 没有对应输出语义，naga 报 `Unsupported builtin PointSize`（`naga/src/back/hlsl/conv.rs:189-191`） | 调试用着色器；不转换，或去掉该输出后重编译 |

`core/clouds.vsh` 的 `isamplerBuffer` 已按 §3 第 4 条降级为只读 storage buffer；它要求宿主把 `CloudFaces` 绑定成 **32 位 `int` 元素**（`R32_SINT` 视图或等价物），否则 `ByteAddressBuffer.Load(i*4)` 读到的是打包后的 4 个 `R8_SINT` 字节。

其他限制：sampler binding array 在 `static_samplers` 下不支持（naga 明确报错）；`#extension` 一律丢弃；函数式宏不支持（语料没有）。

## 7. 宿主接入（下一阶段，不在本期改代码）

现役宿主入口是 `Illusion\src\main\kotlin\dev\nts\expdxr\d3d12\render\shader\D3D12ShaderCompiler.kt` 的 `createIntermediary(filename, source, type, includeResolver)`，它当前做的是 `shaderc → SPIR-V → SPIRV-Cross → HLSL`。转换器替代这一段：

1. 宿主传入：着色器名、GLSL 源（或路径）、stage、include 根、该 pipeline 的 defines。
2. 转换器返回：HLSL 文本 + manifest。
3. 宿主把 HLSL 交给 `MCDR.dll` 的 DXC（`DxcCreateInstance`），并按 manifest 装配根签名：cbuffer 用 `b<register>`、texture 用 `t<register>`、sampler 用 `s<register>` 静态采样器、`storage_buffers` 里 `ByteAddressBuffer` 用 `t<register>` 的缓冲 SRV。**不能再有 sampler heap**（naga 的默认输出形态），这要求根签名把 sampler 声明为 static。
4. `storage_buffers` 的 `ByteAddressBuffer` 只接受 32 位元素视图：原版 `CloudFaces` 是 `R8_SINT` texel buffer，宿主接回时必须在同一资源上建 `R32_SINT` 视图（元素数 = 原 texel 数 / 4），或把 4 个 texel 预先打包成一个 `int`。
5. 反射校验（M2）：拿 DXC reflection 的绑定表与 manifest 比对，不一致就是转换器或契约的 bug。

在接回之前，宿主自身的行为不受影响：本期不修改 `Illusion`、`MCDR`、`Illusion-Creation` 的任何源码。

宿主与转换器之间的接口是 C ABI（JSON 进、JSON 出），请求/响应 schema、返回值、内存所有权与加载方式见 [`shader-convert-abi.md`](shader-convert-abi.md)。

## 8. 用法

发布产物是 DLL，命令行只用于开发与测试：

```powershell
# 发布物：target\release\illusion_shader.dll（workspace 默认成员只有库，不会构建 exe）
cargo build --release

# 编译单个着色器（开发用 CLI，产物写到 out\ 下：<文件名>.hlsl + <文件名>.manifest.json）
cargo run -p shader-convert-cli -- convert <shader> --out out --include <shaders 根> -D SOME_DEFINE

# 查看预处理 + 方言归一之后真正喂给 naga 的 GLSL
cargo run -p shader-convert-cli -- normalize <shader> --include <shaders 根>

# 批量审计一个 shader 目录，退出码 = 失败数
cargo run -p shader-convert-cli -- audit <shaders 根> --defines-file tests\fixtures\corpus-defines.json
```

`-D` 与 `--defines-file` 可以同时使用：文件里的 `default` 先铺开，再叠加路径后缀命中的条目，最后命令行 `-D` 覆盖。