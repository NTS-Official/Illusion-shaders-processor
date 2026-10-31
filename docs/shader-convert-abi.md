# shader-convert C ABI

转换库以 C ABI 形式交付，**release 产物是 DLL**；`shader-convert` 命令行只用于开发与测试（`cli\` 是独立 workspace 成员，`cargo build --release` 不会构建它）。

## 1. 产物与加载

| 产物 | 路径 | 用途 |
| --- | --- | --- |
| `illusion_shader.dll` | `target\release\illusion_shader.dll` | 发布物；宿主在启动时加载 |
| `illusion_shader.dll.lib` | 同上目录 | MSVC 链接用导入库（C/C++ 宿主用得到，JVM 侧不需要） |
| `shader-convert.exe` | `target\debug\shader-convert.exe` | 仅开发：`cargo build -p shader-convert-cli` 或 `cargo run -p shader-convert-cli -- ...` |

`Cargo.toml` 里 lib 声明为 `crate-type = ["cdylib", "rlib"]`、`name = "illusion_shader"`：`cdylib` 产出 DLL，`rlib` 只用于让 CLI 与单测链接同一份代码。因此 `cargo build`（debug）同样会产 DLL，这是预期的；**exe 只有在显式构建 `shader-convert-cli` 时才会出现**。

不要在 release profile 里打开 `panic = "abort"`：ABI 层用 `catch_unwind` 把 panic 转成错误响应，abort 会让宿主进程直接终止。

## 2. 导出函数

```c
uint32_t illusion_shader_abi_version(void);
int32_t  illusion_shader_convert(const char *request_json, char **out_response_json);
void     illusion_shader_free(char *string);
```

- `illusion_shader_abi_version`：返回 ABI 版本，当前为 `1`。宿主加载后应先比对，不一致就拒绝转换。
- `illusion_shader_convert`：`request_json` 是 UTF-8 的 NUL 结尾字符串；成功返回 `0`，失败返回 `1`，两种情况下 `*out_response_json` 都被写成由本库分配的响应字符串。返回 `2` 表示参数本身不可用（空指针、非 UTF-8、内嵌 NUL），此时 `*out_response_json` 不被写入。
- `illusion_shader_free`：归还 `illusion_shader_convert` 写出的字符串。传 `nullptr` 是合法的空操作；**不要对同一个指针调用两次**。

响应字符串归本库所有，宿主必须复制出需要的内容后立刻调用 `illusion_shader_free`；库内部不使用任何全局可变状态，多个线程可以并发调用 `illusion_shader_convert`（每次调用独立分配响应）。

## 3. 请求 schema

```json
{
  "path": "core/terrain.vsh",
  "source": "#version 330\n...",
  "stage": "vertex",
  "entry": "main",
  "include_roots": ["D:\\...\\assets\\minecraft\\shaders"],
  "defines": { "MULTIDRAW_TERRAIN": null, "OIT_COEFF_COUNT": "8" },
  "shader_model": "6_0",
  "space": 0,
  "register_layout": "host",
  "bindings": { "DynamicTransforms": 1, "Sampler0": 2 },
  "vertex_attributes": ["Position", "UV0", "Color"],
  "vertex_outputs": [{ "location": 0, "hlsl_type": "float3" }, { "location": 1, "hlsl_type": "float2" }],
  "flip_y": false,
  "flip_texcoord": "texCoord",
  "uniform_block": "CommonUniforms",
  "legacy_builtins": true
}
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `path` | 是 | 着色器路径。用于命名、选择 stage（当 `stage` 缺省时）以及相对 `#include` 的锚点；文件不必真的存在，前提是给了 `source` |
| `source` | 否 | GLSL 文本。缺省时读取 `path` 指向的文件 |
| `stage` | 否 | `vertex` / `fragment` / `compute`（也接受 `vert`、`frag`、`vs`、`ps`、`pixel`、`comp`、`cs`）；缺省按扩展名推断（`.vsh`/`.vert` → vertex，`.fsh`/`.frag` → fragment，`.comp`/`.csh` → compute，`.mesh`/`.task` 不自动识别） |
| `entry` | 否 | 入口函数名，默认 `main` |
| `include_roots` | 否 | `#include <minecraft:x>` 的搜索根，按顺序尝试 `<root>/include/x`、`<root>/x`、`<root>/shaders/include/x` |
| `defines` | 否 | 预处理宏。值为 `null` 表示只定义不赋值（等价于 `-D NAME`），字符串表示赋值（等价于 `-D NAME=VALUE`） |
| `shader_model` | 否 | `6_0` .. `6_9`，默认 `6_0` |
| `space` | 否 | 所有资源寄存器所在的 DX12 register space，默认 `0` |
| `register_layout` | 否 | `spirv`（默认，四类资源各自从 0 计数）或 `host`（宿主根签名按一条资源一个寄存器的顺序编号：cbuffer 在前、texture 接着、sampler 复用所属 texture 的寄存器、storage 在最后） |
| `bindings` | 否 | `name -> register` 覆盖表。名字对 cbuffer 是**结构体名**（`DynamicTransforms`，不带 `constants_` 前缀），对 texture 是变量名（`Sampler0`），split 出来的 sampler 沿用所属 texture 的名字；未列出的资源按布局顺序编号 |
| `vertex_attributes` | 否 | 顶点输入的名字顺序，宿主按顶点格式元素顺序传（如 `["Position", "UV0", "Color"]`）。转换器据此编号 `LOC0..`，**覆盖 shader 里已写的 `layout(location = N)`** —— 原版 shader 的 location 是按其常量布局声明的，与宿主的格式元素顺序无关 |
| `vertex_outputs` | 否 | 顶点阶段把交给片段阶段的接口，元素是 `{"location": 0, "hlsl_type": "float2", "interpolation": null}`。给出时转换器按 `location` 顺序补齐片段入口缺少的输入（补成未被使用的 `dxr_pad<location>`，用 `discard` 守卫让它留在签名里），因为 D3D12 按 (semantic, index) 的寄存器与分量掩码做链接，不看名字；宿主传 `stage_io.outputs` 即可 |
| `flip_y` | 否 | 默认 `false`。为 `true` 时在每条 `gl_Position` 整值赋值后插入 `gl_Position.y = -gl_Position.y;`。宿主 `flipY` 管线必须传它，并**不要**再对返回的 HLSL 打 Y 翻转补丁（会翻两次） |
| `flip_texcoord` | 否 | 片元输入的名字（如 `texCoord`）。给出时在 `main` 开头插入 `vecN <name>Flipped = vecN(<name>.x, 1.0 - <name>.y, …);` 并把 `main` 体内对该输入的读取改成 `Flipped` 副本。宿主 `lightmap` 管线传 `texCoord`，传了之后**不要**再插 `texCoord.y = 1.0 - texCoord.y;` 补丁 |
| `uniform_block` | 否 | 块名（如 `CommonUniforms`）。给出时把所有全局作用域的松散 `uniform` 收进该 `std140` 块（初始化器丢弃并记 warning），供只认 Iris uniform 契约的光影包使用 |
| `legacy_builtins` | 否 | 默认 `false`。为 `true` 时改写光影包仍在用的固定管线内建（`gl_Vertex`/`gl_Color`/`gl_Normal`/`gl_MultiTexCoord*`/`gl_TextureMatrix*`/`gl_ModelViewMatrix*`/`gl_NormalMatrix`/`ftransform()`/`attribute`/`gl_Fog.*`/`gl_VertexID`/`gl_InstanceID`）并补兼容声明（`projectionMatrix`、`modelViewMatrix`、`normalMatrix`、`va*`），同时把 `bool` uniform 降级为 `int` |

`defines` 必须由宿主按 pipeline 提供：原版在 `RenderPipelines` 里逐 pipeline 注入（例如 `MULTIDRAW_TERRAIN`、`OIT_COEFF_COUNT=8`、`PORTAL_LAYERS=15`），转换器不猜。

请求里出现未列出的字段会被拒绝（`kind: "request"`，`deny_unknown_fields`）：曾经因为转换器少接一个 `bindings` 字段，宿主的实参被静默忽略，结果是实机 PSO 创建失败（`0x80070057`）而不是编译报错。

## 4. 响应 schema

成功（返回 `0`）：

```json
{
  "ok": true,
  "hlsl": "struct Globals { float4 Color; }; ...",
  "manifest": { "schema": "illusion.shader-manifest/1", "...": "..." },
  "warnings": ["lowered buffer texture `CloudFaces` to a readonly storage buffer"]
}
```

失败（返回 `1`）：

```json
{ "ok": false, "kind": "glsl-frontend", "message": "GLSL frontend rejected the shader: shaders\world0\gbuffers_terrain.fsh:57: Expected Semicolon, found RightBrace" }
```

`message` 在位置能对上时以 `文件:行号: ` 开头（`#include` 里的错误指向被包含的文件），校验失败还会把 `source()` 链拼在后面；宿主可以原样展示给玩家，不需要自己再补文件名。

`kind` 取值：

| kind | 含义 |
| --- | --- |
| `request` | 请求 JSON 不合法（缺字段、类型不对） |
| `preprocess` | `#include` 缺失/成环、条件指令不配对、表达式不可求值、未闭合注释、`#define` 的参数表未闭合 |
| `dialect` | GLSL 归一化失败（不支持的不透明类型、buffer texture 用法超出降级规则等） |
| `glsl-frontend` | naga GLSL 前端拒绝该着色器 |
| `validation` | naga 校验失败（IR 不合法，通常意味着方言层改写有缺口） |
| `hlsl-backend` | naga HLSL 后端拒绝（例如 `gl_PointSize`） |
| `missing-entry-point` | `entry` 指定的函数不存在 |
| `io` | 读 `path` 或写产物失败 |
| `panic` | 库内部 panic，已被 `catch_unwind` 转成响应 |

`manifest` 的字段与语义见 `glsl_hlsl_conversion_contract.md` §5；宿主只按 manifest 装配根签名，不解析 HLSL。

## 5. JVM 侧调用草图

宿主只允许用 FFM（`Linker.nativeLinker().downcallHandle(...)`），因此 ABI 刻意只收发 `char *`：

```kotlin
val linker = Linker.nativeLinker()
val lookup = linker.defaultLookup()
val convert = linker.downcallHandle(
    lookup.find("illusion_shader_convert").orElseThrow(),
    FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS),
)
val free = linker.downcallHandle(
    lookup.find("illusion_shader_free").orElseThrow(),
    FunctionDescriptor.ofVoid(ADDRESS),
)

Arena.ofConfined().use { arena ->
    val request = arena.allocateFrom(requestJson)          // UTF-8 + NUL
    val out = arena.allocate(ADDRESS)
    val code = convert.invokeExact(request, out) as Int
    val response = (out.get(ADDRESS, 0)).reinterpret(Long.MAX_VALUE).getString(0)
    free.invokeExact(out.get(ADDRESS, 0))
    // code 0 -> response 是成功 JSON；code 1 -> 错误 JSON；code 2 -> 参数问题
}
```

这里只是接口示意。当前宿主实现在 `Illusion\src\main\kotlin\dev\nts\expdxr\d3d12\render\shader\IllusionShaderBridge.kt`（同样的 FFM 调用），并按 `glsl_hlsl_conversion_contract.md` §7 完成根签名、静态采样器与装配；转换器本身不依赖 `Illusion`。

## 6. 版本策略

`ABI_VERSION` 只在请求/响应 schema 发生不兼容变化时增加。新增可选字段、新增 `kind` 取值、`manifest` 增加字段都属于兼容变化，不升版本；宿主读不认识的字段应当忽略。

同一规则适用于另外两份 JSON：`illusion.shader-manifest/1` 与 `illusion.shader-verify/1`（`audit` 写的是 `illusion.shader-audit/1`）在 v1 内只增字段，不删不改名，读方按字段名取用、忽略不认识的键；删字段或改语义必须升 schema 版本号。

