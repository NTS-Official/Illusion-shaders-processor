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
| 源文件 | `*.vsh`/`*.vert` = vertex，`*.fsh`/`*.frag` = fragment，`*.comp`/`*.csh` = compute；也可用 `--stage` 覆盖 |
| include 根 | 传 `shaders/` 目录。`#include <minecraft:name>` 依次尝试 `<root>/include/<name>`、`<root>/<name>`、`<root>/shaders/include/<name>`；光影包的包绝对路径（`#include "/program/x.glsl"`）去掉前导 `/` 后按 `<root>/<相对路径>`、`<root>/shaders/<相对路径>` 再找一遍 |
| 宏 | `NAME`（无值）或 `NAME=VALUE`；值按 GLSL 预处理语义做对象式展开，最多 8 轮。无函数式宏 |
| 阶段宏 | 转换器总会定义当前 stage 的宏（VERTEX_SHADER / FRAGMENT_SHADER / COMPUTE_SHADER），供光影包的共享 include 分岔；命令行 -D 同名时以命令行值为准 |
| 条件编译 | `#ifdef/#ifndef/#if/#elif/#else/#endif`；`#if` 支持 `defined(X)`、整数算术、比较、位运算、逻辑运算；指令行尾的 `//` 与 `/* */` 注释按空白处理（光影包常用 `#if X == 0 // Potato`） |
| 入口 | 固定 `main` |
| 着色器模型 | `--shader-model`，默认 `6_0`，上限 `6_9` |
| 寄存器布局 | `--register-layout spirv`（默认，四类资源各自从 0 计数）或 `host`（宿主根签名一条资源一个寄存器：cbuffer 在前、texture 接着、sampler 复用所属 texture、storage 最后） |
| 显式寄存器 | `--binding NAME=REGISTER`，可重复。cbuffer 用结构体名（`DynamicTransforms`），texture 用变量名（`Sampler0`），split 出来的 sampler 沿用 texture 的名字 |
| 顶点属性顺序 | `--vertex-attribute Position,UV0,Color`，可重复。顶点输入按该顺序编号 `LOC0..`，并**覆盖** shader 自带的 `layout(location = N)` |
| 阶段接口补齐 | `--vertex-output LOC0=float,LOC1=float2`（可重复，仅片元；CLI 之外由 ABI 的 `vertex_outputs` 承担）。按顶点输出顺序重建片段参数表，缺的位置补成 `dxr_pad<loc>` 并配一条 `discard` 守卫，使两个阶段的寄存器掩码一致（见 §7 第 6 条）。空表 = 不补齐 |
| Y 翻转 | `--flip-y`（默认关）。开启后在每条 `gl_Position` **整值赋值**之后插入 `gl_Position.y = -gl_Position.y;`；成员赋值不处理，避免重复翻转。宿主 `flipY` 管线用 |
| 纹理坐标翻转 | `--flip-texcoord texCoord`（默认关，仅片元）。把该全局 `in` 的读取换成一个 `y` 翻转的局部副本；宿主 `lightmap` 管线用 |
| 光影包兼容 | `--legacy-builtins`（默认关）。改写光影包仍在用的固定管线内建与 GLSL 1.20 语法（`gl_Vertex`/`gl_Color`/`gl_TextureMatrix[n]`/`ftransform()`/`attribute` …，见 §3 第 16 条），并把常量缓冲里的 `bool` 成员降级成 `int`（第 17 条）。原版 26.3 语料不引用这些名字，关着时行为不变 |

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

预处理只认真正位于代码里的指令：块注释与行注释里的 `#define` / `#if` / `#include` 是注释文本，既不展开宏也不参与条件编译；未闭合的块注释、未闭合的函数式宏参数表、不配对的 `#if`/`#endif`、找不到的 include 都是 `preprocess` 硬错误（消息形如 `unterminated comment`、`#define <name> has an unterminated parameter list`），不做“尽量猜”的回退。

| 顺序 | 改写 | 原因 |
| --- | --- | --- |
| 1 | `gl_VertexIndex` → `int(gl_VertexIndex)` | naga 把该内建建模为 `u32`，赋给 `int` 会触发 `InvalidStoreTypes` |
| 2 | `const int n = <expr>; arr[n]` → `arr[(<expr>)]` | naga 要求数组长度是常量表达式 |
| 3 | `const T[] a = T[](…)` / `T a[] = …` 补全元素个数 | naga 不接受无长度数组 |
| 4 | `uniform isamplerBuffer X;` → `layout(std430, binding = 48+n) readonly buffer XBuffer { int data[]; } X;` + 读取用的 `int dxr_texel_X(int index)`；`texelFetch(X, i).r` → `dxr_texel_X((i))` | naga 与 HLSL 都没有 texel buffer 类型；naga 把只读 SSBO 输出为 `ByteAddressBuffer`（SRV）。语料里的 buffer texture 都是 `R8_SINT` 视图，一个 texel 只占一个字节，所以 `dxr_texel_X` 从 32 位字的低字节取出并按 `int` 符号扩展（直接读 32 位会把相邻四个 texel 拼进来）。块实例沿用 uniform 名，宿主才能按名字对上它的绑定条目 |
| 5 | `uniform Block { … };` → `layout(binding = N) uniform Block { … };` | naga 要求 uniform block 显式 binding；N 从 0 递增（已有 `layout(binding)` 的块跳过） |
| 6 | `uniform sampler2D X;` → `layout(binding = 16+2i) uniform texture2D X;` + `layout(binding = 17+2i) uniform sampler X_sampler;` | naga 不支持 combined image sampler；两个 binding 必须不同 |
| 7 | 形参 `sampler2D p` → `texture2D p, sampler p_sampler`（紧跟原参数插入），调用点补 `, p_sampler` | naga 的类型表没有 `sampler2D`，但支持 opaque 形参 |
| 8 | `texture(X, …)` 等 → `texture(sampler2D(X, X_sampler), …)`；cube 用 `samplerCube` | naga 只接受显式组合的构造函数 |
| 9 | 全局 `in`/`out`：顶点输入按宿主属性顺序写 `layout(location = N)`（覆盖已有值）；其余缺失的 location 按声明顺序补 | 宿主按顶点格式元素顺序把 `semanticNames` 对到 attribute slot，与 shader 的声明顺序无关；VS 输出与 PS 输入按 location 配对 |
| 10 | `--flip-y` 时，每条 `gl_Position = …;` 之后追加 `gl_Position.y = -gl_Position.y;` | 宿主原有的 Y 翻转是对 SPIRV-Cross 的 HLSL 打正则补丁；naga 的顶点输出形态不同，补丁必须发生在 IR 生成之前 |
| 11 | `gl_PointSize = …;` 整句删除 | D3D12 没有点大小；naga 的 HLSL 后端对 `PointSize` 直接报错，留着会让整个着色器转不出来。manifest 的 `warnings` 会记一条 |
| 12 | `--flip-texcoord name` 时，在 `main` 开头插入 `<type> nameFlipped = <type>(name.x, 1.0 - name.y[, name.z[, name.w]]);`，并把 `main` 内对 `name` 的读取全部改成 `nameFlipped` | naga 不允许对 `in` 变量赋值（`Variable cannot be used in LHS position`），只能复制到局部再改写读取 |
| 13 | `--uniform-block Name` 时，把全局作用域的松散 `uniform <type> <name>;`（含数组与初始化器）收进 `layout(std140, binding = N) uniform Name { … };`，N 取现有块数 | naga 要求所有 uniform 都是带 binding 的块；光影包把 uniform 定义交给宿主按名字填充。初始化器被丢弃并记 warning，moved <n> loose uniform(s) 也会写进 manifest 的 warnings |
| 14 | 旧式采样函数改写：`texture2D`/`texture3D`/`textureCube(x, …)` → `texture(sampler2D(x, x_sampler), …)`，`texture2DProj` → `textureProj`，`texture2DGrad`/`textureCubeGrad` → `textureGrad`，`texture2DProjGrad` → `textureProjGrad`，`texture2DLod`/`textureCubeLod` → `textureLod`，`texture2DProjLod` → `textureProjLod`；`shadow2D(x, p)` → `vec4(texture(sampler2DShadow(x, x_sampler), p))`。只在调用位改写，类型声明里的 `texture2D` 不动；首参不是裸采样器变量时原样保留 | 光影包仍用 GLSL 1.2 风格采样函数，naga 没有对应内建；`texture2D` 同时是拆分后 sampler 的类型名；shadow2D 包一层 `vec4` 让旧的 .x/.z 取值继续有效 |
| 15 | `gl_FragData[i]` → `iris_FragData<i>`，并在 `#version` 之后注入 `layout(location = i) out vec4 iris_FragData<i>;` | 槽号 `i` 是 `DRAWBUFFERS`/`RENDERTARGETS` 指令里的**列表下标**，不是 colortex 编号；无指令或越界直接报错，不做猜测。映射写入 manifest 的 `mrt.targets`。 |
| 16 | `--legacy-builtins` 时改写固定管线内建：`gl_Vertex`→`vec4(iris_Position, 1.0)`、`gl_Color`→`(iris_Color * iris_ColorModulator)`、`gl_Normal`→`iris_Normal`、`gl_MultiTexCoord0..3`→`iris_UV0`/`iris_UV2`/`mc_midTexCoord`、`gl_ModelViewMatrix`→`iris_ModelViewMat * iris_ModelOffsetMatrix()`、`gl_ModelViewProjectionMatrix`→`(iris_ProjMat * iris_ModelViewMat * iris_ModelOffsetMatrix())`、`gl_ProjectionMatrix`/`gl_NormalMatrix`/`gl_TextureMatrix[0/1]`→同名 `iris_*` uniform、`gl_ModelViewMatrixInverse`/`gl_ProjectionMatrixInverse`→`iris_ModelViewMatInverse`/`iris_ProjMatInverse`、`ftransform()`→`(iris_ProjMat * iris_ModelViewMat * iris_ModelOffsetMatrix() * vec4(iris_Position, 1.0))`；`attribute` 关键字→`in`。用到的名字会注入声明，`iris_ModelOffsetMatrix()` 作为辅助函数在需要时注入（`iris_ModelOffset` 构造平移矩阵） | D3D12 没有固定管线状态，这些值只能由宿主按 Iris 的兼容层语义填充；注入的 `in` 随后由第 9 条分配 location，注入的 `uniform` 可由第 13 条收进宿主填充的块。合成类程序必须把 `iris_ModelOffset` 设成 0（转换器不区分程序类别）。其它 `gl_TextureMatrix` 层是硬错误 |
| 17 | `--legacy-builtins` 时把 `bool` uniform（松散的与块成员）改写成 `int <name>_value`，另在 `#version` 后声明 `bool <name>;`、在 `main` 首行赋值 `<name> = <name>_value != 0;` | naga 把 `bool` 放进常量缓冲会让校验器报 `InvalidHandle(LocalVariable 0)`（最小复现：块里一个 `bool` 成员）；D3D12 的常量缓冲也确实没有 `bool` |
| 18 | `--legacy-builtins` 时：`gl_Fog.<field>` → `gl_Fog_<field>` 并注入 `uniform vec4 gl_Fog_color;` / `uniform float gl_Fog_<density/start/end/scale>;`；`gl_VertexID`/`gl_InstanceID` → `gl_VertexIndex`/`gl_InstanceIndex`；未声明的 `projectionMatrix`/`modelViewMatrix`/`normalMatrix` 与 `vaPosition`/`vaNormal`/`vaColor`/`vaUV0..2` 按 Iris 兼容层类型补声明 | `gl_Fog` 是固定管线雾状态，naga 没有该结构；`va*` 是 Iris 的 core-profile 兼容属性，值由宿主按自己的顶点格式提供 |
| 19 | `uniform image3D X;` → `layout(binding = 64+n, rgba32f) uniform image3D X;`（`uimage`/`iimage` 分别补 `rgba32ui`/`rgba32i`，已有 binding 的改写为 `64+n`） | naga 要求 image uniform 必须带 `binding` 与格式限定符；缺格式时按分量类型补，缺 binding 时从 64 起分配（在 sampler 拆分之后执行，避免与 texture 的 `16+2i` 撞车） |

第 4 条要求 `texelFetch` 必须带 `.r`/`.x` 分量选择（语料如此）；不带分量或选其它分量是硬错误。降级的缓冲只能按 32 位元素读取，宿主侧必须提供相应的 32 位视图，见 §7。

第 6～8 条支持的 sampler 类型矩阵：形状 `1D / 1DArray / 2D / 2DArray / 2DMS / 2DMSArray / 3D / Cube / CubeArray` × 分量 `float / int / uint`（`sampler*` / `isampler*` / `usampler*`）。对应 HLSL 类型分别是 `Texture1D`…`TextureCubeArray` 加 `float4` / `int4` / `uint4`；`2DMS` 在 HLSL 里拼成 `Texture2DMS`。两类不支持：

- **比较采样器**（`sampler2DShadow` / `samplerCubeShadow` / `sampler2DArrayShadow`）：拆成 `texture2D` + `samplerShadow`，取样本处的 `texture(X, vec3(uv, ref))` 走 `sampler2DShadow(X, X_sampler)`。HLSL 侧 naga 生成 `Texture2D<float>` + `SamplerComparisonState` 与 `SampleCmp`（已实测：`Texture2D<float> shadowmap : register(t0); SamplerComparisonState shadowmap_sampler : register(s0); ... shadowmap.SampleCmp(shadowmap_sampler, uv, ref)`）。拆分是不动点：替换出来的 `samplerShadow` 没有图像 shape，不会被再次当成组合式 sampler（`splits_comparison_samplers_once` 钉住；split pass 另有 token 增长上限，超过即报错而不是继续拆）。注意宿主还没接：native 的静态采样器一律非比较型，这类管线切到转换器前需要 native 支持比较采样器（见 §7）。
- **整数纹理的采样**：`isampler*`/`usampler*` 只支持 `texelFetch`（naga 的校验器只允许对整数 image 做 `textureLoad`）。对整数纹理调用 `texture*` 会报错并指向 `texelFetch`，不再落到 naga 内部的 `InvalidImageClass`。

改写登记进 manifest 的 `warnings`（非空时）与资源表（binding 号）。改写失败（include 找不到、条件未闭合、`#if` 无法求值）是硬错误，不做回退。

`gl_VertexIndex` 以外的内建不做改写；语料实际用到 `gl_Position`、`gl_FragCoord`、`gl_FragDepth`、`gl_FrontFacing`、`gl_PointSize`。其中 `gl_PointSize` 的赋值会被删掉（见 §3 第 11 条）。

## 4. 输出约定

HLSL 是单文件、自包含的 `Shader Model 6.x` 源码，入口名 `main`，内部实现函数由 naga 生成（`main_1`、`ConstructVertexOutput` 等）。

**资源声明**

- cbuffer：`cbuffer global_<n> : register(b<n>) { <Type> global_<n>; }`，`<Type>` 是 GLSL uniform block 的结构体；按 IR 全局变量顺序编号。
- texture：`Texture2D<float4> X_ : register(t<n>)`、`TextureCube<...>`、`Texture2D<int4>` 等由元素类型决定。
- sampler：`SamplerState X_sampler : register(s<n>)` 或 `SamplerComparisonState`（`static_samplers` 打开时；关闭时 naga 会退回 sampler heap，本转换器永远打开）。
- storage buffer：降级的 buffer texture（只读）输出为 `ByteAddressBuffer X : register(t32+n)`，按 32 位元素 `Load`（`Load(i*4)`）；可写 storage buffer 由 naga 输出 `RWByteAddressBuffer : register(u<n>)`，语料尚未出现，宿主也没有 UAV 槽位。
- storage image：`uniform image3D X;` → `RWTexture3D<float4> X : register(u<n>)`（`uimage` → `uint4`、`iimage` → `int4`）。naga 要求每个 image 都带 `layout(binding = …)`，方言层对缺失的补 `layout(binding = <n>, rgba32f)` 并把已有的 binding 一并改写成自己的编号；缺格式限定符时按元素类型给默认格式（`image`/`uimage`/`iimage` → `rgba32f`/`rgba32ui`/`rgba32i`），因为宿主（Iris）是在 dispatch 时按图像单元决定格式的，源码里通常不写。可用 `imageLoad`/`imageStore`/`imageAtomic*`；宿主绑定仍只有 SRV/CBV 槽位，带 storage image 的管线保留旧链（§7 第 8 条）。
- 四类寄存器各自从 0 独立计数，`space = 0`；storage buffer 从 32 起、storage image 从 64 起，两者都先 `first_free_binding` 跳过源里已声明的 binding，因此与 texture 的 `t0..` 不冲突。
- `--register-layout host` 时改为宿主编号：cbuffer `b0..`、texture 紧接着排、split sampler 复用所属 texture 的寄存器、storage 再往后；显式 `--binding` 逐条覆盖。宿主根签名的寄存器号就是资源条目下标，因此**两端必须用同一套编号**。

**布局**

- cbuffer 成员按 std140 语义排布，naga 会在结构体尾部插入 `int _end_pad_N` 补齐。宿主按 std140 写入即可，不要按 HLSL 的 16 字节打包重新排列。
- 常量缓冲区的 `register` 与 GLSL 的 `binding` 无对应关系；宿主以 manifest 的 `register` 为准。

**stage IO**

- `layout(location = N) in/out` → `LOC<N>` 语义；`gl_Position` → `SV_Position`，其余内建按 HLSL 常规（`SV_Depth`、`SV_IsFrontFace`、`SV_VertexID` 等）。
- VS 的输出结构体与 PS 的输入结构体由各自着色器的语义生成；同一 `LOC<n>` 的语义名与类型一致才能配对。字段名不参与配对。
- 标识符可能被 naga 重命名（例如与生成名冲突时追加 `_`，`Sampler2` → `Sampler2_`）。manifest 记录 IR 名；**以 HLSL 里的声明文本为准**。

**合法性**

产物是标准 HLSL，不需要任何字符串级后处理：语料里 72 个着色器在 DXC 1.9.0 下以 `vs/ps/cs_6_0` 与 `6_9` 全部编译通过（`-E main`）。`core/debug_point.vsh` 的 `gl_PointSize` 赋值在方言层被丢弃（D3D12 无点大小），manifest 的 `warnings` 里会记一条。

## 5. manifest schema

`schema = "illusion.shader-manifest/1"`。

```json
{
  "schema": "illusion.shader-manifest/1",
  "shader": { "name": "terrain.vsh", "stage": "vertex", "entry": "main", "hlsl_file": "terrain.vsh.hlsl", "shader_model": "6_0" },
  "resources": {
    "textures": [ { "name": "Sampler2", "hlsl_type": "Texture2D<float4>", "register": 0, "space": 0, "group": 0, "binding": 16 } ],
    "samplers": [ { "name": "Sampler2_sampler", "register": 0, "space": 0, "group": 0, "binding": 17, "comparison": false, "texture": "Sampler2" } ],
    "constant_buffers": [ { "name": "constants_Fog", "hlsl_type": "Fog", "register": 0, "space": 0, "group": 0, "binding": 0, "size": 40, "members": [ { "name": "FogColor", "hlsl_type": "float4", "offset": 0, "size": 16 } ] } ],
    "storage_buffers": [ { "name": "CloudFaces", "hlsl_type": "ByteAddressBuffer", "register": 32, "space": 0, "group": 0, "binding": 48, "size": 0, "members": [] } ],
    "storage_images": [ { "name": "floodfill_img", "hlsl_type": "RWTexture3D<float4>", "register": 0, "space": 0, "group": 0, "binding": 64, "format": "Rgba16Float", "access": "StorageAccess(STORE)" } ]
  },
  "stage_io": {
    "inputs": [ { "name": "Position", "location": 0, "hlsl_type": "float3", "interpolation": "Perspective" } ],
    "outputs": [ { "name": "texCoord0", "location": 3, "hlsl_type": "float2", "interpolation": "Perspective" } ],
    "builtins": [ { "name": "gl_Position", "builtin": "Position { invariant: false }", "hlsl_type": "float4" } ]
  },
  "mrt": { "targets": [ 0, 3, 6, 4 ] },
  "warnings": []
}
```

字段约定：

- `group` = GLSL 的 `set`（原版恒为 0）；`binding` = 转换器注入的 binding（块从 0 起、texture `16+2i`、sampler `17+2i`、降级的 buffer texture `48+n`、storage image `64+n`，后两类都用 `first_free_binding` 跳过源里已声明的 binding）；`register`/`space` = 实际 HLSL 声明。
- `size` 与成员的 `offset`/`size` 是**按 HLSL 常量缓冲打包规则算出的字节布局**（标量 4 字节；两分量与四分量向量按自身字节数对齐、三分量向量按 4 字节对齐；成员不跨 16 字节边界；矩阵每列占 16 字节；数组元素按 16 字节步进）。`size` 只是已声明成员的打包大小，属**下界**：naga 会在结构体尾部补 `_end_pad_N` 成员让整个缓冲对齐，DXC 报告的尺寸可能更大，因此不要拿 `size` 当缓冲分配大小的唯一依据。
- `verify` 用 DXC 的 `!dx.typeAnnotations`（`-Zi` 才会输出）逐条比对 `size`（下界）与成员 `offset`；这是当前唯一可信的布局来源。
- `interpolation` 为 `null` 表示该 IO 用了默认（`smooth`/`Perspective`）。
- `mrt` 只在着色器写 `gl_FragData` 时出现：`targets[slot]` 是槽对应的 colortex 编号（来自 `DRAWBUFFERS`/`RENDERTARGETS`），宿主据此建 MRT 视图。
- 字段名由 `src\manifest.rs` 的 `manifest_keys_match_the_documented_schema` 钉住（顶层、`shader`、`resources`、各资源条目、`stage_io`、`Io`），改字段会先让单测红。
- `warnings` 记录“改写发生、语义仍然成立但需要宿主知情”的提示：texel buffer 降级（每个 texel 只取 32 位字的低字节）、`gl_PointSize` 被丢弃等。宿主必须把它写进日志，否则这些降级对玩家不可见。

输出是确定性的：同一输入字节 + 同一 defines 集合 → 相同 HLSL 与 manifest（defines 用 `BTreeMap`，资源按 IR 顺序）。

## 6. 失败模式与已知限制

错误按阶段分类，消息前缀固定：

- `preprocess`：include 缺失/循环、未闭合条件、`#if` 求值失败、未闭合注释、`#define` 参数表未闭合。
- `dialect`：无法识别的 sampler 声明形态。
- `GLSL frontend rejected the shader`：naga 前端拒绝（通常是缺少宿主宏，或用了 naga 不支持的 GLSL 构造）。
- `shader failed validation`：naga 校验失败（IR 不合法）。
- `HLSL backend failed`：后端能力缺口。

只要位置能对上，消息里就带 `文件:行号: `。GLSL 诊断原本只有归一化文本里的字节偏移，转换器用 `lexer::render_with_lines` 的行映射 + 预处理的 `originals` 把它复原成作者能直接打开的位置：`#include` 里的错误指向被包含的文件（`lib/typed.glsl:5`），方言层拼进去的片段（`dxr_pad<loc>`、`dxr_texel_X` 辅助函数）按拼接处的源行记账，不会冒充第 1 行。校验错误取 `WithSpan` 里最内层的可定位跨度，并把 `source()` 链拼在消息后面（`Function [0] 'typed' is invalid: The type of [7] doesn't match the type stored in [8]`），因为 `ValidationError` 自己的 `Display` 只到“is invalid”。

原版 26.3 语料（72 个 shader）在给定 `tests/fixtures/corpus-defines.json` 时 **72 个全部可转换**；能力缺口只剩下面这些需要宿主配合的部分：

| 着色器 | 原因 | 处理 |
| --- | --- | --- |
| `core/debug_point.vsh` | `gl_PointSize` 写入；D3D12 没有点大小，赋值在方言层被删掉并记 warning | 点大小不生效；该着色器是调试用途 |

`core/clouds.vsh` 的 `isamplerBuffer` 已按 §3 第 4 条降级为只读 storage buffer，并要求宿主在同一资源上建 **RAW 视图**（`DXGI_FORMAT_R32_TYPELESS` + `D3D12_BUFFER_SRV_FLAG_RAW`）。语料里的 buffer texture 是 `R8_SINT`：一个 texel 只占一个字节，转换器生成的 `dxr_texel_X` 从 32 位字的低字节取值并按 `int` 符号扩展，所以宿主照原样绑定字节数据即可，不需要重新打包；直接读 `Load(i*4)` 会把相邻四个 texel 拼进同一个 `int`。

其他限制：sampler binding array 在 `static_samplers` 下不支持（naga 明确报错）；`#extension` 一律丢弃；函数式宏支持 `#define f(a, b) …`（不能跨行，不支持 `#`/`##`）；整数纹理采样只允许 `texelFetch`；比较采样器见 §3；float 型 buffer texture（`uniform samplerBuffer`）不支持——它的 texel 语义依赖原始格式的归一化，转换器按整数位取字节（§3 第 4 条），方言层直接报错。`image*` uniform 已能转换（`RWTexture*` + `u<n>`，见 §3），但**宿主还不能绑定 UAV**：带 storage image 的管线在 primary 路径上按 `storage image` 跳过（§7 第 8 条）。包里的 compute 着色器还要靠加载器提供的特性宏（`IRIS_FEATURE_CUSTOM_IMAGES`、`COLORED_LIGHTING` 等）才把 `image*` 与采样器声明出来，离线跑要自己用 `-D` 补。

跨行块注释里开始的那一行整行原样保留（不展开宏）；宏写在行首、指令在注释外开始的写法不受影响。

### 6.1 光影包语料（Complementary r5.8.1，219 个入口文件）

带 `--uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json` 的 `audit <pack>\shaders --include <pack>\shaders` 结果：**195 compiled / 24 failed / 219**（216 个 `.vsh/.fsh` + 3 个 `.csh`）。24 个失败全是外部宿主程序的匹配符号：Colorwheel 的 `clrwl_*`（12）、Distant Horizons 的 `dh_*`（12）。3 个 `shadowcomp.csh` 声明 `writeonly uniform image3D`，转换器产出 `RWTexture3D` + `u<n>`（§3 storage image）并把 UAV 记进 manifest 的 `storage_images`；它们只有定义了加载器的特性宏（`IRIS_FEATURE_CUSTOM_IMAGES`、`COLORED_LIGHTING`）才会声明出 `voxel_sampler` 与 `image*`，`tests\fixtures\pack-compute-defines.json` 就是这份集合（不带时这 3 个报 `Unknown variable: voxel_sampler`）。用 `--skip dh_ --skip clrwl_` 排除外部宿主程序后是 **183 compiled / 0 failed / 183**，`verify` 同为 183/183（其中 3 个 compute 各 `textures=25 samplers=25 cbuffers=1`，UAV 组与 manifest 逐项对上）。

| 失败类（首个错误） | 次数 | 说明 |
| --- | --- | --- |
| `Unknown function 'clrwl_computeFragment'` | 12 | Colorwheel 程序，未安装该模组时 Iris 也不编译 |
| `UnknownVariable("dhProjection")` | 12 | Distant Horizons 程序（`dh_terrain` / `dh_water`） |
| `UnvisitedExpression` | 0 | 曾出现在 `gbuffers_skytextured.fsh`，由 naga 侧补丁放宽（死表达式不参与代码生成，见 `docs\shader_pack_frontend.md` §10.1） |
| （已可转换）`Unknown variable: voxel_sampler` | 0 | 3 个 `world*/shadowcomp.csh` 需要加载器特性宏，由 `tests\fixtures\pack-compute-defines.json` 提供后会转成 UAV 并 `verify` 通过 |

已经落地的门槛：

- `--uniform-block`：把松散 uniform 收进一个由宿主填充的 std140 块（规则见 §2、§3 第 13 条）；
- 函数式宏与反斜杠续行：`#define f(a, b) …` 在预处理阶段展开（§2 宏行；`#`/`##` 仍不支持，宏调用必须单行）；
- 条件编译里的宏值递归展开：#define A B 与 #define B 2 让 #if A == 2 成立（此前只取第一层文本，非数字即当成 1）；
- 旧式采样函数改写：`texture2D` 族与 `shadow2D`（§3 第 14 条）；
- 阶段宏：共享 include 里的 `#ifdef VERTEX_SHADER` / `FRAGMENT_SHADER` 分支会正确分岔（§2 阶段宏行）；
- `--legacy-builtins`：固定管线内建（§3 第 16 条）、`gl_Fog` 与 Iris 兼容 uniform/属性声明（第 18 条）、GLSL 1.20 的 `attribute` 关键字与 `bool` uniform 降级（第 17 条）；
- storage image：`image*` → `RWTexture*` + `u<n>`，缺 binding/格式由方言层补（§3），manifest 增 `storage_images`（§5），`verify` 比对 UAV 组；带 storage image 的管线在宿主上整条跳过；
- naga 侧补丁：入口函数继承全局上下文的 `local_variables`（全局初始化器里的三元表达式会生成临时局部变量），见 [shader_pack_frontend.md](shader_pack_frontend.md) §10。

结论：光影包支持（M4 的 shader pack frontend）剩下的都是**外部宿主程序、加载器预定义与宿主侧 UAV 绑定**——Colorwheel、Distant Horizons 的符号，Iris 注入的 `MC_RENDER_STAGE_*` 一类常量，以及宿主对 `RWTexture*` 的 UAV 槽位（转换器侧已经能产出）。前端契约、语料基线、legacy 内建展开表与 MRT 规则见 [shader_pack_frontend.md](shader_pack_frontend.md)。原版 26.3 语料不受影响（72/72 仍全可转换、`verify` 72/72）。
## 7. 宿主接入（已实现 shadow 与 primary 两条路径）

宿主入口是 `Illusion\src\main\kotlin\dev\nts\expdxr\d3d12\render\shader\D3D12ShaderCompiler.kt` 的 `createIntermediary(filename, source, type, includeResolver)`，现役实现仍是 `shaderc → SPIR-V → SPIRV-Cross → HLSL`；转换器通过 `IllusionShaderBridge`（FFM，JSON 进 / JSON 出）并行接入，**旧链始终保留**：

- **shadow**（`[debug] shaderConverter`，默认关）：`D3D12Device.compileShader` 在 `createIntermediary` 之前把同一份展开后的 GLSL 再转一遍，只记日志与产物（`dxrmc/converter/`），出图不变。
- **primary**（`[debug] shaderConverterPrimary` + `[debug] shaderConverterPipelines`）：命中的管线直接用转换器的 HLSL 与 `semanticNames`，未命中的继续走旧链，关掉开关即可完全回退。

接回时必须遵守的约定：

1. **寄存器**：根签名的寄存器号就是 `D3D12CompiledShader.entries` 的下标，所以请求要带 `register_layout = "host"` 与按 entries 组装好的 `bindings`（cbuffer 用结构体名、texture 用变量名、sampler 沿用所属 texture 的名字）。缺 `bindings` 时两侧编号不一致，`CreateGraphicsPipelineState` 返回 `0x80070057`。
2. **顶点输入**：请求带 `vertex_attributes`（宿主顶点格式元素顺序），转换器覆盖 shader 自带的 `layout(location)`；`semanticNames` 改成 `LOC<location>`，与 `MCDRPipelineDesc.InputElement` 一一对应。naga 会裁掉 shader 不读取的输入，所以布局元素可以多于转换后的参数：宿主按名字把**主转换响应**的 `stage_io.inputs[]` 对回格式元素，被裁掉的元素用 `UNUSED<下标>`；不要用影子转换缓存的 manifest 里的 location（那是按声明顺序编号的，会错位）。

3. **Y 翻转**：`flipY` 管线传 `flip_y = true`，由转换器插入翻转；宿主不再对 HLSL 打 `gl_Position` 正则补丁。
4. **静态采样器**：宿主根签名对每个 SRV 绑定生成 `D3D12_STATIC_SAMPLER_DESC`（`s<register>`），因此 naga 必须走 `static_samplers`，不能有 sampler heap。
5. **storage_buffers**：`ByteAddressBuffer` 只接受 RAW 视图（`R32_TYPELESS` + RAW 标志；类型化视图会被调试层判为 view format mismatch）。原版 `CloudFaces` 是 `R8_SINT` texel buffer，宿主在同一资源上建 RAW 视图，转换器的 `dxr_texel_X` 按字节读出每个 texel（位移量 `(index & 3) * 8`），因此**不需要**重新打包数据。
6. **storage_images**：`RWTexture*` 需要 UAV 槽位，而当前宿主绑定只有 SRV/CBV 条目。转换器照常产出 UAV（`u<n>` 编号从 cbuffer+texture+storage buffer 之后继续，`space = 0`），manifest 的 `resources.storage_images[]` 记 `hlsl_type`/`register`/`format`/`access`；宿主看到非空就整条管线留在旧链（`Illusion\D3D12Device.applyConverterPrimary` 的 `storage image` 分支）。格式来自源码的 `layout(…, rgba16f)`，缺省按元素类型补 `rgba32f`/`rgba32ui`/`rgba32i`，宿主将来建 UAV 视图时以 manifest 为准。`verify` 把 UAV 组列入比对（`BindingKind::Uav`，比寄存器、space 与类型族 `RWTexture1D/2D/3D`、`RWByteAddressBuffer`、`RWStructuredBuffer`；元素宽度与视图格式不比，以 manifest 的 `format` 为准）。
7. **阶段接口**：片段转换要带 `vertex_outputs`（顶点响应的 `stage_io.outputs[]`：`location` + `hlsl_type` + `interpolation`）。D3D12 按 `(semantic, index)` 的寄存器与分量掩码链接两个阶段，而两个阶段各自打包输入：顶点写 `LOC0`(float)、`LOC1`(float2) 时占 `reg0.x` 与 `reg0.yz`，片元只声明 `LOC1` 会被打进 `reg0.xy`，掩码不是子集，`CreateGraphicsPipelineState` 拒绝（调试层 `#662`）。转换器按顶点顺序重建片段参数表、缺的补成 `dxr_pad<loc>`，并用 `if (dxr_pad<loc> == 1234.5678) discard;` 读取，避免编译器把未用输入裁掉；补齐条数记一条 manifest warning（`padded N fragment input(s) …`）。空表表示不做补齐（离线 audit/verify 就是这么跑的）。
8. **不切 primary 的情形**（宿主逐条跳过并在关闭时按原因汇总，见 `Illusion\docs\shaders\README.md` §16）：只剩转换失败一类的个别管线——`meshlet variant` 只在 `D3D12MeshletShader.build` 连不剔除的 MS 都拼不出来时出现。地形的 meshlet 变体已能直接吃转换器产物：`build` 支持 `XxxOutput main(参数 : LOC<n>, …)` 形态（按参数重建 `DxrVertexInput`、入口改 `vs_main`），AS 剔除解析的四个 uniform 名对裸名（`ProjMat`）与 spvc 的唯一化前缀（`_81_ProjMat`）都成立。
9. **补丁等价**：旧链对 SPIRV-Cross 产物打的三类补丁都已收进转换器——`flipY` 走 `flip_y`；`lightmap` 更新 pass 与「顶点是 `screenquad`、片元按 `texCoord` 取样」的全屏 blit 都走 `flip_texcoord = "texCoord"`（见 §3 第 12 条）。后者漏掉会让 `blit_depth` 把镜像的深度写进 OIT depth bounds，云的半透明几何被整体剔除。宿主不再对这些 HLSL 打补丁。
10. **设备拒绝时回退**：转换产物能过 DXC 但 `dx12CreateGraphicsPipeline = 0`（典型是 `#662` 阶段链接）时，宿主把产物 dump 到 `dxrmc/converter/failed_<pipeline>.*` 后用 SPIR-V 链重建同一条管线，只记一条 warn。接回时不要把这个回退删掉：单条产物出错只应退回旧链，不能让管线失效。

反射校验（M2）：`verify` 子命令已用 DXC 编译，并比对 register/space/类型族与 stage IO（`ISG1`/`OSG1` 的 location、分量数、分量类型）；cbuffer 字节偏移仍留给后续。

宿主与转换器之间的接口是 C ABI（JSON 进、JSON 出），请求/响应 schema、返回值、内存所有权与加载方式见 [`shader-convert-abi.md`](shader-convert-abi.md)。

## 8. 用法

发布产物是 DLL，命令行只用于开发与测试：

```powershell
# 发布物：target\release\illusion_shader.dll（workspace 默认成员只有库，不会构建 exe）
cargo build --release

# 编译单个着色器（开发用 CLI，产物写到 out\ 下：<文件名>.hlsl + <文件名>.manifest.json）
cargo run -p shader-convert-cli -- convert <shader> --out out --include <shaders 根> -D SOME_DEFINE

# 按宿主根签名编号，并显式钉住个别寄存器与顶点属性顺序（宿主接入时的实际用法）
cargo run -p shader-convert-cli -- convert core\position_tex_color.fsh --out out --include <shaders 根> ^
  --register-layout host --binding DynamicTransforms=1 --binding Sampler0=2 ^
  --vertex-attribute Position,UV0,Color

# Y 翻转管线（宿主 flipY = true）
cargo run -p shader-convert-cli -- convert core\item_cutout.vert --out out --include <shaders 根> ^
  --register-layout host --vertex-attribute Position,UV0,Color --flip-y

# 查看预处理 + 方言归一之后真正喂给 naga 的 GLSL
cargo run -p shader-convert-cli -- normalize <shader> --include <shaders 根>

# 批量审计一个 shader 目录，退出码 = 失败数
cargo run -p shader-convert-cli -- audit <shaders 根> --defines-file tests\fixtures\corpus-defines.json

# 光影包：排除需要外部宿主程序的程序（按路径子串，可重复；语料深，用 release）
cargo run --release -p shader-convert-cli -- audit <pack>\shaders --include <pack>\shaders --uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json --skip dh_ --skip clrwl_

# DXC 编译 + 与 manifest 比对 register/space 与 stage IO，退出码 = 校验失败数
cargo run -p shader-convert-cli -- verify <shaders 根> --defines-file tests\fixtures\corpus-defines.json --dxc "<dxc.exe 路径>"
```

`-D` 与 `--defines-file` 可以同时使用：文件里的 `default` 先铺开，再叠加路径后缀命中的条目，最后命令行 `-D` 覆盖。

四个子命令各自只接受自己那份选项清单，拼错或把别的子命令的选项塞进来会报 `` `audit` does not take `--out` `` 并打印用法，不会被静默忽略（CLI 单测钉住）。

`verify` 的输出里 `ok` 表示 manifest 承诺的每个 register 都被 DXC 绑定在同一个 `space`、类型族一致，且 `stage_io` 的每个参数都在 `ISG1`/`OSG1` 里以相同 location、分量数与分量类型出现；`pruned=N` 表示有 N 个 manifest 资源被 DXC 判定为未使用而裁掉（不算失败）。找不到 `dxc` 时按 `--dxc` → `PATH` → `VULKAN_SDK` 的顺序查找。细节与局限见 `docs/verification.md`。

### 8.1 机器可读报告

`audit` 与 `verify` 都支持 `--report <file>`，写出的 JSON 是 CI 的接口，字段名与 `schema` 字符串只在升版本时改：

- `illusion.shader-audit/1`：`{ schema, root, total, compiled, failed, failures: [{ shader, error }] }`；`error` 是完整（未截断）的错误链。
- `illusion.shader-verify/1`：`{ schema, root, dxc, shader_model, total, verified, failed, entries: [{ shader, ok, bindings, inputs, outputs, pruned, warnings[], findings[], error? }] }`；`entries` 按 shader 路径排序，`error` 只在转换或 DXC 阶段就失败时出现，`findings` 是资源与 stage IO 的失败原因，`warnings` 是 manifest 的 warning（空数组省略），例如被删掉的 `gl_PointSize` 赋值。

两条命令都按 shader 路径排序输出，同一目录两次运行的报告逐字节一致，CI 可以直接 diff。转换产物本身也逐字节确定：同样的输入与选项两次运行产出相同的 HLSL 与 manifest（`tests\deterministic.rs` 钉住）。标准输出（`ok ...` / `fail ...` 行与汇总行）只是给人看的，不保证稳定；两者都以退出码 = 失败数收尾。字段名由 `src\report.rs` 的 `audit_report_keys_match_the_documented_schema` / `verify_report_keys_match_the_documented_schema` 钉住，改字段会先让单测红。


- **宿主形态快照**：`tests\fixtures\golden\host_bindings.fsh` 钉住 `register_layout = host` 与显式 `bindings`（cbuffer `b3`、texture/sampler `t7`/`s7`）；`host_inputs_declared.vsh` 钉住属性顺序覆盖声明里的 `layout(location)`；`src\abi.rs` 的 `honours_every_documented_request_field` 走一遍 C 入口，确认文档里的每个请求字段都真的到达编译器（serde 漏字段曾让 `bindings` 静默失效）。










