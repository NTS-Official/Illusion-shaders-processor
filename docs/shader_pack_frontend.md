# 光影包前端（M4）契约

本文件定义把 Iris 风格光影包交给 `illusion_shader` 之前、必须完成的前端工作，以及宿主必须提供的值。转换器本身只接受已经归一化的 GLSL（见 [glsl_hlsl_conversion_contract.md](glsl_hlsl_conversion_contract.md)），本文件是它的上游契约。

## 1. 范围

- 输入：一个光影包目录（`<pack>\shaders` 及其子目录）。
- 输出：每个程序文件的归一化 GLSL，满足转换器的输入约定（`#version 450 core`、无 include、无松散 uniform、无 legacy 采样函数）。
- 不属于本层：HLSL 生成、资源寄存器分配、PSO 装配（分别在转换器与宿主）。

## 2. 语料基线

基线是 Complementary Unbound r5.8.1（408 文件 / 18,978 行）。数字用于验收，改动语料时重新测量。

| 指标 | 值 |
| --- | --- |
| 文件 | `.fsh` 108、`.vsh` 108、`.csh` 3、`.glsl` 189 |
| `#version` | `130` × 213、`400 compatibility` × 3（三个维度的 `shadow.vsh`）、`430 compatibility` × 3（`shadowcomp.csh`） |
| legacy 内建 | `texture2D(` 118、`texture2DLod` 75、`shadow2D` 6、`gl_FragData` 66、`gl_MultiTexCoord*` 27、`gl_TextureMatrix` 30、`gl_NormalMatrix` 22、`ftransform(` 23、`gl_Vertex` 17、`gl_Color` 20 |
| 现代构造 | `varying` 0、`texelFetch` 101、`usampler*` 4、`sampler2DShadow` 2、`std140` 0、`std430` 2 |
| MRT | `DRAWBUFFERS` 43 处（最大 5 槽）、`RENDERTARGETS` 5 处（含 `0,5,4,19,18` 这种非连续且大于 8 的编号） |
| 顶点属性 | `mc_Entity` 7、`mc_midTexCoord` 21、`at_tangent` 21 |
| compute | `shadowcomp.csh` 3 个（`imageStore` 9、SSBO 2） |

当前转换器跑这 219 个入口文件（216 个 `.vsh/.fsh` + 3 个 `.csh`）的结果是 **195 compiled / 24 failed**：24 个全是外部宿主程序的匹配符号（Colorwheel 12 个 `clrwl_computeFragment`、Distant Horizons 12 个 `dhProjection`）。3 个 `.csh` 由 `tests\fixtures\pack-compute-defines.json` 提供加载器特性宏（`IRIS_FEATURE_CUSTOM_IMAGES`、`COLORED_LIGHTING`），带上它排除外部程序后是 **183 compiled / 0 failed**，`verify` 183/183（含 3 个 compute 的 UAV 组），失败集中在 `gl_TextureMatrix`（108 处）、包内自定义工具函数（51 处）、多参数纹理构造（28 处）、`gl_FragData`（14 处）、Distant Horizons 与模组 uniform（12 处）。这些缺口都必须在本层解决，转换器不为它们增加特例。

## 3. 程序模型

一个"程序"不是文件，是四元组：**元文件 × 阶段 × 维度 × 程序宏**。

- 元文件（例如 `world0\composite.fsh`）只有 `#version`、几条 `#define` 和一条 `#include "/program/composite.glsl"`。
- 被 include 的 `program\*.glsl` 用 `#ifdef VERTEX_SHADER` / `#ifdef FRAGMENT_SHADER` 写两个阶段，用 `#define OVERWORLD` / `NETHER` / `END` 区分维度。
- 阶段与维度宏必须由前端定义，不能靠源文件自带。
- 程序编号不连续：`program\composite2.glsl` 没有任何入口文件。**程序发现以入口文件为准**，不要按 `composite0..7` 循环。
- `world0` / `world1` / `world-1` 各 76 个文件，内容同构，必须按维度分别展开与编译。
- 参考实现不认识 `clrwl_*`、`voxy_*`、`pack.json`、`colorwheel.properties`。遇到未知程序必须记录诊断并跳过，不能中断整个包。

## 4. 预处理顺序

顺序错了 MRT 与宏都会错。按下列次序执行：

1. **发现程序文件**：按入口文件与维度目录枚举。
2. **展开 `#include`**：相对"包含者所在目录"解析；检测环；`#include` 先于一切其它预处理指令，include guard 无效。
3. **宏展开**：对象式与函数式宏都要支持；`#version`、`#extension` 先换成占位符再还原，避免被宏处理破坏；环境宏通过注入宏表提供，不写进源文本（保住行号）。
4. **`#version` 归一**：小于 `330` 一律提到 `330 core`；`compatibility` 改为 `core`。
5. **采集注释与 const 指令**：`DRAWBUFFERS`、`RENDERTARGETS`、`colortexNFormat` 等。
6. **逐项改写**：legacy 内建（§6）、纹理函数族（§7）、MRT 输出（§8）、阶段与维度宏。

## 5. 注释与 const 指令

- 同种指令取**最后一次**出现，且前面必须紧接 `/*`（允许 `/*DRAWBUFFERS:321*/` 这种无空格写法）。
- `DRAWBUFFERS` 与 `RENDERTARGETS` 同时出现时，取源码位置更靠后的那条。
- `colortex` 上限 32；`gcolor`、`gdepth`、`gnormal`、`composite`、`gaux1..4` 是 0..7 的别名。
- `RENDERTARGETS` 是注释指令，不是 `shaders.properties` 的键；`colortexNFormat`、`colortexNClear`、`colortexNClearColor`、`colortexNMipmapEnabled` 是 const 指令。
- `#moj_import` 一律拒绝。

## 6. Legacy 内建契约

替换字面量与 Iris 一致；不同的是这些 `iris_*` 值由宿主注入（§10）。

| legacy | 展开为 | 需要声明 | 宿主提供 |
| --- | --- | --- | --- |
| `gl_Vertex` | `vec4(iris_Position, 1.0)` | `in vec3 iris_Position;` | 顶点属性 POSITION |
| `gl_Color` | `iris_Color * iris_ColorModulator` | `in vec4 iris_Color;` `uniform vec4 iris_ColorModulator;` | COLOR 属性 + `ColorModulator` |
| `gl_Normal` | `iris_Normal` | `in vec3 iris_Normal;` | NORMAL 属性（可缺） |
| `gl_MultiTexCoord0` | `vec4(iris_UV0, 0.0, 1.0)` | `in vec2 iris_UV0;` | UV0 属性 |
| `gl_MultiTexCoord1` | `vec4(iris_UV2, 0.0, 1.0)` | `in ivec2 iris_UV2;` | UV2 属性 |
| `gl_MultiTexCoord2` | 改名为 `gl_MultiTexCoord1` | — | — |
| `gl_MultiTexCoord3` | `mc_midTexCoord` | `in vec4 mc_midTexCoord;` | 属性，缺省 `vec4(0.0)` |
| `gl_TextureMatrix[0]` | `iris_TextureMat` | `uniform mat4 iris_TextureMat;` | `TextureMat` |
| `gl_TextureMatrix[1]` | `iris_LightmapTextureMatrix` | `uniform mat4 iris_LightmapTextureMatrix;` | 宿主可算 |
| `gl_ModelViewMatrix` | `iris_ModelViewMat * iris_ModelOffsetMatrix()`（转换器注入该辅助函数，由 `iris_ModelOffset` 构造平移矩阵） | `uniform mat4 iris_ModelViewMat;` `uniform vec3 iris_ModelOffset;` | `ModelViewMat` + 每 draw 偏移；合成类程序必须把偏移设成 0 |
| `gl_ModelViewProjectionMatrix` | `iris_ProjMat * iris_ModelViewMat * iris_ModelOffsetMatrix()` | 同上一行 | 组合 |
| `gl_ProjectionMatrix` | `iris_ProjMat` | `uniform mat4 iris_ProjMat;` | `ProjMat` |
| `gl_NormalMatrix` | `iris_NormalMat` | `uniform mat3 iris_NormalMat;` | `transpose(inverse(mat3(ModelViewMat)))` |
| `gl_ModelViewMatrixInverse` / `gl_ProjectionMatrixInverse` | `iris_ModelViewMatInverse` / `iris_ProjMatInverse` | `uniform mat4 iris_ModelViewMatInverse;` `uniform mat4 iris_ProjMatInverse;` | 宿主提供；缺失时报错，不要瞎补 |
| `ftransform()` | `iris_ProjMat * iris_ModelViewMat * iris_ModelOffsetMatrix() * vec4(iris_Position, 1.0)`（不注入函数，直接内联） | `in vec3 iris_Position;` `uniform mat4 iris_ModelViewMat;` `uniform mat4 iris_ProjMat;` `uniform vec3 iris_ModelOffset;` | 同上 |

替换必须按下列子顺序执行，否则二次替换会错：

1. `gl_MultiTexCoord*`、`gl_TextureMatrix*`（带下标的先做）
2. `gl_ModelViewMatrix`（先于 `gl_ModelViewProjectionMatrix` 与 `gl_ModelViewMatrixInverse`，后两者名字以它为前缀）
3. `gl_ModelViewProjectionMatrix`、`gl_ProjectionMatrix`
4. `gl_NormalMatrix`
5. `gl_Vertex`、`gl_Color`、`gl_Normal`
6. `ftransform`（最后展开，其函数体引用前面已替换的名字）

`gl_ModelViewMatrix` 必须乘 `ModelOffset`：vanilla 的 `ModelViewMat` 不含区块偏移，偏移在每次 draw 的 `ModelOffset` 里；不乘会出现地形挤在原点。转换器统一展开成 `iris_ModelViewMat * iris_ModelOffsetMatrix()`，所以合成类程序（顶点本来就是裁剪空间 quad）必须把 `ModelOffset` 设成 0，而不是指望转换器分类。

`gl_FragCoord`、`gl_FrontFacing`、`gl_PointCoord`、`gl_VertexID`、`gl_InstanceID`、`gl_Layer`、`gl_PrimitiveID`、`gl_WorkGroupID`、`gl_LocalInvocationID` 原样保留，naga 与 DXC 都支持。

## 7. 纹理函数族

只做名字替换，不重排参数（参数重排需要类型信息，猜错比报错更糟）。

| 原 | 目标 |
| --- | --- |
| `texture2D` / `texture2DProj` / `texture2DBias` | `texture` / `textureProj` / `texture`（带 bias） |
| `texture2DLod` / `texture2DGrad` | `textureLod` / `textureGrad` |
| `texture3D*`、`textureCube*` | 对应 4.x 名 |
| `texelFetch2D` / `texelFetch3D` / `textureSize2D` | `texelFetch` / `texelFetch` / `textureSize` |
| `shadow2D` / `shadow2DProj` / `shadow2DLod` | `texture` / `textureProj` / `textureLod`，目标类型 `sampler2DShadow` |

`texture(sampler2DShadow, vec3)` 返回 `float`；源码把它当 `vec4` 用时必须显式构造 `vec4(...)`，并在诊断里记一条。

## 8. MRT 契约

`gl_FragData[i]` 里的 `i` 是**列表下标**，不是 colortex 编号。

```glsl
/* DRAWBUFFERS:0364 */           // 槽 0..3 → colortex 0,3,6,4
gl_FragData[0] = color;          // → iris_FragData0
```

改写规则：

- 为每个被写入的槽注入 `layout(location = <槽号>) out vec4 iris_FragData<槽号>;`。
- `gl_FragData[i]` 改成 `iris_FragData<i>`。
- 映射表（槽 → colortex）写入 manifest 的 `mrt.targets`，宿主据此建 MRT 视图；清单缺这张表时该程序不可用。

## 9. 转换器当前能力

以下能力已由转换器（或它的方言层）提供，前端不需要重复实现：

- include 展开、对象式与函数式宏（含反斜杠续行）、`#version 330` → 450 core（`--include`、`--defines-file`）。
- 松散 uniform 收进 std140 块（`--uniform-block <Name>`）。
- 组合采样器拆分、比较采样器、纹理形状矩阵、buffer texture 降级。
- 显式 `layout(location)` 补全、顶点属性顺序覆盖（`--vertex-attribute`）、`gl_VertexIndex` 有符号化。
- 旧式采样函数族（`texture2D`/`texture3D`/`textureCube`/`texture2DProj`/`*Grad`/`*Lod`）与 `shadow2D` 改写、`gl_PointSize` 丢弃。
- `--legacy-builtins`：固定管线内建（含 `gl_Color * iris_ColorModulator`、`iris_ModelOffsetMatrix()`、逆矩阵）、`gl_Fog` 字段、`gl_VertexID`/`gl_InstanceID`、Iris 兼容的 `projectionMatrix`/`modelViewMatrix`/`normalMatrix` 与 `va*` 属性声明、GLSL 1.20 的 `attribute`、`bool` uniform 降级为 `int` + 全局 `bool`。
- 多声明符接口块（`flat out vec3 a, b;`）拆成逐名声明，显式 `layout(location)` 先保留、自动编号跳过已占用位置。
- `gl_FragData[i]` → MRT 输出（槽映射进 manifest 的 `mrt.targets`，`DRAWBUFFERS`/`RENDERTARGETS` 取最后一次与更靠后者）。
- Y 翻转（`--flip-y`）、纹理坐标翻转（`--flip-texcoord`）、入口签名与 cbuffer 布局自检（`verify`）。

## 10. 宿主必须提供的值

- `DynamicTransforms` 的 `ModelViewMat`、`ModelOffset`、`ProjMat`、`TextureMat`、`ColorModulator`。
- `iris_LightmapTextureMatrix`、`iris_NormalMat`（可由上面算出）、以及按需的逆矩阵。
- `MC_GLSL_VERSION` 必须报 `330`（与 Iris 一致）：报 `450` 会让包打开没有归一化过的 4.x 分支。
- MRT 槽到 colortex 的映射、`colortexNFormat`/`colortexNClear*` 的渲染目标配置。
- `shaders.properties` 的 `program.*`、`profile.*`、`screen*`、`sliders`、`blend.*`、`alphaTest.*`、`scale.*`、`size.buffer.*`、`texture.*`、`image.*`、`uniform.*`、`variable.*` 键族的解析结果。

### 10.1 naga 侧补丁

转换器依赖 `naga-for-hlsl` 的六处改动，升级 naga 时必须保留：

- `back/hlsl` 的 `Options::static_samplers`：每个 sampler 输出成独立的 `SamplerState ... : register(sN)`，不再走 sampler heap（宿主的根签名只有静态采样器）。
- `back/hlsl` 的 `SampleLevel::Zero`：非比较采样输出 `SampleLevel(sample, uv, 0.0)`；HLSL 没有 `SampleLevelZero`，写成它会让所有顶点采样编译失败。
- `front/glsl/functions.rs` 的入口函数：`add_entry_point` 必须把全局上下文的 `local_variables` 一并交给入口函数。全局初始化器里的三元表达式会在全局上下文创建一个临时局部变量；丢弃它会让整个着色器报 `InvalidHandle(LocalVariable 0)`（未修时整个光影包批次在此报错，补丁后该类清零）。
- `valid/function.rs` 的未访问表达式检查：GLSL 前端会在条件分支里留下一些没有语句引用的表达式（例如 swizzle 复合赋值重复读取自身），校验器原本直接报 `UnvisitedExpression`。死表达式不进入代码生成，因此改为不报错；已访问的表达式仍按原规则校验。
- `back/hlsl` 的向量条件选择：`mix(…)` 这类操作会产生向量条件的 `Select`，HLSL 的 `?:` 只接受标量条件；条件类型是向量时输出 `select(reject, accept, condition)`。`vector_select.fsh` 快照钉住这个形态。
- `back/hlsl` 的向量 `abs`：`MathFunction::Abs` 原本生成 `val >= 0 ? val : asint(-asuint(val))`，向量时 DXC 直接拒绝（`condition for short-circuiting ternary operator must be scalar`）；分量数大于 1 时改出 `select(asint(-asuint(val)), val, val >= 0)`，标量仍用 `?:`。3 个 `shadowcomp.csh` 的 `abs(int3)` 就踩在这条。

## 11. 验收

- `.vsh/.fsh/.csh` 入口文件都能产出归一化 GLSL；`image*` uniform 转成 `RWTexture*` + `u<n>`（宿主还不能绑定 UAV，管线在 primary 路径上跳过），未知程序有诊断而不是崩溃。
- `DRAWBUFFERS` / `RENDERTARGETS` 解析结果与 Iris 一致（取最后一次、位置更靠后者优先）。
- `gl_FragData` 改写后 MRT 槽与 colortex 的映射可在清单里查到。
- 语料编译率按入口文件统计：基线是本文件 §2 的 219 个入口文件，当前为 **195 compiled / 24 failed**（24 个全部是外部宿主程序的匹配符号：Colorwheel 12、Distant Horizons 12）（`--uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json`）。
- 3 个 `shadowcomp.csh` 依赖加载器给 compute 的特性宏（`IRIS_FEATURE_CUSTOM_IMAGES`、`COLORED_LIGHTING`），`tests\fixtures\pack-compute-defines.json` 就是这份集合；不带它时这 3 个报 `Unknown variable: voxel_sampler`。
- 用 `--skip dh_ --skip clrwl_` 排除外部宿主程序（各 12 个）后是 **183 compiled / 0 failed / 183**：其余入口文件全部可转换。
- 归一化结果交给转换器后，`verify` 通过（资源寄存器、入口签名、cbuffer 偏移、UAV 组）：当前 183 个 in-scope 入口文件 183 verified / 0 failed。



