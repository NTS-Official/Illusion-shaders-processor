# 校验：DXC 编译与 reflection 比对

## 1. 目的

转换器的产物是 HLSL 文本 + manifest。manifest 是宿主装配根签名、输入布局与资源视图的唯一依据，所以它必须和 DXC 实际绑定的结果一致，而不是“看起来一致”。

`verify` 做四件事：

1. 用 DXC 按目标 profile 编译生成的 HLSL，编译失败即为失败。
2. 解析 DXIL 汇编里的资源绑定，与 manifest 逐条比对 `space`、`register` 与类型族。
3. 解析 DXIL 容器里的入口签名（`ISG1` 输入、`OSG1` 输出），与 manifest 的 `stage_io` 比对 location、分量数与分量类型。
4. 解析 `-Zi` 汇编里的 `!dx.typeAnnotations`，与 manifest 的 cbuffer `size`/成员 `offset` 比对。

它不检查根签名形状、采样状态与 buffer 的元素宽度（HLSL 里 `ByteAddressBuffer` 的可见宽度由宿主视图决定）。

## 2. 流程

```
convert (GLSL → HLSL + manifest)
  → dxc -T <profile> -E <entry> -Fo <tmp>.dxil -Fc <tmp>.asm <tmp>.hlsl
  → 解析 <tmp>.asm 的 !dx.resources
  → 解析 <tmp>.dxil 的 ISG1 / OSG1 part
  → compare(manifest.resources, reflection) + compare_stage_io(manifest.stage_io, reflection)
```

`!dx.resources = !{!N}`，`!N` 是四个分组的列表，顺序固定为 SRV、UAV、CBV、Sampler；每个分组是条目列表，条目格式为：

```
!{i32 <kind>, %<type>* undef, !"<name>", i32 <space>, i32 <lower bound>, ...}
```

- 第 2 个 `i32` 是 `space`，第 3 个是 `register`（DXIL 里叫 lower bound）。
- 资源名在这一层是空字符串，因此匹配只能按 `(kind, space, register)`，不能按名字。
- 没有资源的着色器没有 `!dx.resources`，按空 reflection 处理。

签名 part（`ISG1` 输入、`OSG1` 输出）的结构：

```
u32 count           参数个数
u32 offset          元素表偏移（当前 DXC 固定为 8）
count × 32 B        元素表，每个元素：
                      [0, nameOffset, semanticIndex, systemValueType,
                       componentType, register, packedMask, minPrecision]
打包的语义名        NUL 结尾
```

- `packedMask` 的低 4 位是写入的分量位，`popcount` 即分量数。
- `componentType`：1 = `uint32`、2 = `int32`、3 = `float32`。
- `register` 不是 location：DXIL 会把多个标量输出并进同一个 register（`LOC0` 用 bit0、`LOC1` 用 bit1…），所以按 `LOC<n>` 的语义索引 `n` 匹配，语义不是 `LOC` 时才退回 register。`-Zi` 还会输出 `!dx.typeAnnotations`：每个类型的首个 `i32` 是字节大小，成员节点里 `i32 3` 后面是偏移，这是 cbuffer 布局的唯一可信来源。
- 旧式 `ISGN`/`OSGN` 是 24 字节元素且首字段就是 nameOffset；解析器会先试 32 字节布局，失败再试 24 字节。

## 3. 判定规则

| 情况 | 判定 | 说明 |
| --- | --- | --- |
| manifest 的资源在 DXIL 同 `(kind, space, register)` 出现，类型族一致 | 通过 | 类型族指 `Texture2D` / `TextureCube` / `ByteAddressBuffer` / `RWTexture3D` / `SamplerState` 这类粗分类；UAV 组也参与（`RWTexture*`、`RWByteAddressBuffer`、`RWStructuredBuffer`） |
| manifest 的资源在 DXIL 里找不到 | 警告，`pruned` 计数 | DXC 把未使用的资源裁掉了，manifest 仍要保留它（宿主按 pipeline 绑定） |
| DXIL 在某个 `(kind, space, register)` 有绑定，manifest 没有声明 | 失败 | 说明转换器的寄存器分配被 DXC 改写或发生了冲突 |
| 同一位置两边类型族不同 | 失败 | 典型是 texture 与 buffer 的寄存器撞车 |
| manifest 的 stage 输入/输出在 `ISG1`/`OSG1` 同 location 出现，分量数与分量类型一致 | 通过 | location 取 `LOC<n>` 的语义索引 `n`；其它语义退回 register |
| cbuffer 成员在 `!dx.typeAnnotations` 里的偏移与 manifest 的 `offset` 一致，且 DXC 尺寸不小于 manifest 的 `size` | 通过 | `-Zi` 输出 typeAnnotations；`size` 是打包下界，naga 的 `_end_pad_N` 会让 DXC 报更大的尺寸。成员按位置比对（过滤掉 naga 自己插入的 `_pad*`/`_end_pad*` 后两侧数量相等），因为 naga 会给重名成员加下划线后缀 |
| cbuffer 成员偏移不同、或 DXC 尺寸小于 manifest 的 `size` | 失败 | 说明 manifest 的打包算法与 HLSL 实际布局不一致 |
| cbuffer 被 DXC 裁掉（没有 typeAnnotations 条目） | 警告 | 未使用的缓冲不会出现在反射里 |
| manifest 声明的 stage 参数 DXC 裁掉（入口点没用到） | 警告，`pruned_io` 计数 | 例如 `block.vsh` 的第 4 个输出 |
| 签名里有 `LOC<n>` 而 manifest 没声明 | 失败 | 说明 manifest 漏了参数，或 location 改写时漏掉了某个 `in`/`out` |
| 分量数 / 分量类型不一致 | 失败 | 例如 `float2` 对上 `float4`（4 个分量） |

退出码是校验失败的着色器数量，与 `audit` 一致。大语料用 release 二进制跑（`cargo run --release -p shader-convert-cli -- verify ...`）：debug 构建在主线程 1 MiB 栈上处理光影包的深表达式会栈溢出。

## 4. 用法

```powershell
cargo run -p shader-convert-cli -- verify <shaders 根> `
    --defines-file tests\fixtures\corpus-defines.json `
    --dxc "C:\Program Files\Khronos Group\Vulkan SDK\Bin\dxc.exe"
```

- `--shader-model 6_0`（默认）或 `6_9`：决定 `-T` 的 profile 后缀。
- `--dxc <path>`：显式指定；不传时按 `PATH`、再按 `VULKAN_SDK\Bin\dxc.exe` 查找。
- `--include` / `-D` / `--defines-file` 与 `convert`、`audit` 相同。
- 光影包语料：加 `--uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json --skip dh_ --skip clrwl_`，与 `audit` 用的是同一组开关；`pack-compute-defines.json` 提供加载器给 compute 的特性宏，缺它时 3 个 `.csh` 报 `Unknown variable: voxel_sampler`。
- `--report <file>`：把结果写成 `illusion.shader-verify/1` JSON（`{ schema, root, dxc, shader_model, total, verified, failed, entries[] }`，每条 `{ shader, ok, bindings, inputs, outputs, pruned, warnings[], findings[], error? }`（`warnings` 省略空数组）），供 CI 读；标准输出不保证字段稳定。`audit --report` 写的是 `illusion.shader-audit/1`。

`pruned` 只出现在 `ok` 行里；需要看具体是哪些资源被裁掉时，用 `convert` 生成 HLSL 后手工跑 DXC。

`ok` 行的格式是 `ok  <相对路径>  bindings=<资源数> io=<输入>/<输出> pruned=<资源裁剪数>[ warnings=<条数>]`：`io` 是 DXIL 签名里的参数个数，`pruned` 只计资源；`warnings` 只在 manifest 带 warning 时出现（例如 `debug_point.vsh` 的 `dropped 1 gl_PointSize assignment(s)`）；`pruned_io` 不打印，出现在失败行的诊断里。

## 5. 当前结果

DXC 1.9.0（Vulkan SDK 1.9(5512)）下：

| 目标 | 结果 |
| --- | --- |
| `--shader-model 6_0` | 72 verified / 0 failed / 72 |
| `--shader-model 6_9` | 72 verified / 0 failed / 72 |
| 光影包 Complementary r5.8.1（`--uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json --skip dh_ --skip clrwl_`），`6_0` | 183 verified / 0 failed / 183 |
| 同上，`--shader-model 6_9` | 183 verified / 0 failed / 183 |

全部 72 个语料通过；光影包侧排除 24 个需要外部宿主程序的入口文件后，183 个全部通过——资源寄存器、入口签名、cbuffer 偏移与 UAV 组都与 DXC 一致。其中 3 个 `shadowcomp.csh` 依赖加载器特性宏，`tests\fixtures\pack-compute-defines.json` 就是那组宏（`IRIS_FEATURE_CUSTOM_IMAGES`、`COLORED_LIGHTING`）。`core\debug_point.vsh` 的 `gl_PointSize` 赋值已由方言层丢弃（D3D12 无点大小，见契约 §3 第 11 条），因此它现在也能编译与校验。

资源绑定与 stage IO 都通过，包括输出打包的顶点着色器（如 `block.vsh` 有 5 个输出、其中 1 个被 DXC 裁掉，`LOC0`–`LOC3` 仍逐项对上）。

## 6. 快照、确定性与诊断回归

`verify` 校验的是“DXC 看到的东西与 manifest 一致”；另一层是**输出本身不要悄悄变**：

- 夹具在 `tests\fixtures\golden\`，共 17 份，每份生成 `<name>.hlsl` 与 `<name>.manifest.json` 两个快照，由 `tests\golden.rs` 逐字节比对。
- 十七份夹具覆盖：宿主属性顺序覆盖声明里的 `layout(location)`（`host_inputs_declared.vsh`）、显式 `bindings` 与 `register_layout = host`（`host_bindings.fsh`）、`--flip-y`、cbuffer 偏移计算、纹理拆分与 cube 构造函数、`isamplerBuffer` 降级、`gl_VertexIndex` 取整、`gl_PointSize` 删除与 warning、`textureSize()` 生成的 `NagaMipDimensions2D` 辅助函数、`--flip-texcoord` 的局部副本改写、组合式比较采样器拆成 `texture2D` + `samplerShadow`（`shadow_sampler.fsh`）、`--uniform-block` 把松散 uniform 收进 std140 块的形态（`uniform_block.fsh`）、旧式采样函数改写（`texture2DLod` → `SampleLevel`、`shadow2D` → `SampleCmp`，`legacy_sampling.fsh`）、`gl_FragData` → `SV_Target` 与 manifest `mrt.targets`（`mrt.fsh`）、固定管线内建与 `attribute` 的展开形态：`gl_Color * iris_ColorModulator`、`iris_ModelOffsetMatrix()` 辅助函数、逆矩阵 uniform（`legacy_builtins.vsh`）、光影包前端组合能力：函数式宏展开、`texture2D` 族改写、`gl_Fog` 字段降级、`bool` uniform 降级、MRT 输出（`pack_fragment.fsh`），以及 `attribute`、`gl_VertexID` 映射、Iris 兼容 `projectionMatrix`/`modelViewMatrix` 与 `vaPosition` 声明（`pack_vertex.vsh`）、向量条件的 `mix` 三元选择输出 `select(...)`（`vector_select.fsh`：没有 naga 的 HLSL 向量选择补丁时 DXC 拒绝 `?:`）、片段输入按顶点阶段输出补齐（`stage_pad.fsh`：只声明 `LOC1`，补齐后写成 `LOC0` 未被使用的 `dxr_pad0`）、storage image 的 UAV 形态（`compute_image.csh`：无格式限定符的 `writeonly uniform image3D` 变成 `RWTexture3D<float4> … : register(u0)`，manifest 记 `storage_images`）。
- 阶段接口：`tests\golden.rs` 的 `a_padded_interface_matches_the_snapshot` 钉住补齐后的 HLSL 形态（`dxr_pad<location>` + `discard` 守卫）；D3D12 的顶点/片段链接按 (semantic, index) 的寄存器与分量掩码比对，不看名字，缺一条输入就是 `CreateGraphicsPipelineState` 的 `#662`。`tests\verify.rs` 那条 `ISG1`/`OSG1` 比对只能在两阶段都转换之后发现问题，不能替代补齐。
- 转换确定性：`tests\deterministic.rs` 把三份夹具（显式 `bindings`、光影包组合、顶点属性顺序）各转两次，断言 HLSL 与 manifest 逐字节相同；它挡的是 `HashMap` 迭代顺序一类的隐性不稳定（宿主缓存产物、CI 直接 diff）。
- 残缺输入：`tests\robustness.rs` 把 25 条残缺 GLSL（空输入、未闭合括号与注释、截断的 `gl_FragData[`、递归宏、残缺的函数式宏参数表、缺失 include、重复的 `layout(location)` 等）逐条喂给编译器，要求返回错误而不是 panic——转换器跑在游戏进程里，panic 会带走渲染线程而不是让单条管线失败；错误 kind 必须登记在 `docs\shader-convert-abi.md`。
- 预处理语义：块注释里的 `#define` / `#if` 是注释文本，不参与宏展开与条件编译；未闭合注释、未闭合的函数式宏参数表都是 `preprocess` 错误，而不是静默保留或静默丢弃宏定义。
- 诊断位置：`tests\diagnostics.rs` 用 `tests\fixtures\diagnostics\` 的四份夹具（语法错误、`#include` 里的类型错误、未闭合注释、被包含的 `lib\typed.glsl`）断言消息里出现 `文件:行号:`，且未闭合注释那条的路径只出现一次——位置来自 `lexer::render_with_lines` 的行映射与预处理的 `origins`，`dxr_pad` 之类的拼接片段按拼接处记账，不会被报成第 1 行。
- 光影包 compute 的加载器宏：`tests\fixtures\pack-compute-defines.json` 只对 `world*/shadowcomp.csh` 生效，由 CLI 用例 `pack_compute_fixture_only_applies_to_its_shaders` 钉住；写进 `default` 会把 `IRIS_FEATURE_CUSTOM_IMAGES` 带进图形着色器，改变它们的产物。
- 有意改动输出（方言改写、manifest 字段、naga 升级）后先看 diff，再用 `$env:UPDATE_GOLDEN='1'; cargo test --test golden` 刷新快照，并把变更写进契约。

## 7. 局限

- 只比对绑定位置与类型族；`ByteAddressBuffer` 的元素宽度与视图格式不在其中（由宿主按 manifest 的 storage 条目决定）。UAV 组同样只比类型族（`RWTexture2D` / `RWTexture3D` / `RWByteAddressBuffer` …）：DXC 的 `RWTexture3D<float4>` 在反射里有元素类型，但真正决定写法的 `format` 来自 manifest，元素宽度不符不会被这条检查抓到。
- 不比对根签名形状与采样状态：静态采样器由根签名生成器按 binding 顺序构造，`verify` 看不到那一层。
- 依赖 `-Fc` 汇编里 `!dx.resources` 的字段顺序与 DXIL 容器里 `ISG1`/`OSG1` 的 part 布局。DXC 升级后如果布局变化，`reflection_from_assembly` / `reflection_from_container` 会先报解析失败而不是静默通过。
- `-Fre` 输出的是 `STAT` 统计块，不是绑定表；需要完整 reflection（含偏移）时应走 `IDxcUtils::CreateReflection`。
- stage IO 只比对标量与向量（`float` / `float2` / `int3` / `uint4`）；矩阵与数组直接跳过（`hlsl_shape` 返回 `None`），`SV_*` 系统语义不参与比对。分量数取自签名的写入掩码 `popcount(mask)`，因此 DXC 把多个标量并进同一 register 时仍按语义索引匹配，不会被寄存器重排误导。







