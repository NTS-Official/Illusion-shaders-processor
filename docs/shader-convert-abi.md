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
  "space": 0
}
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `path` | 是 | 着色器路径。用于命名、选择 stage（当 `stage` 缺省时）以及相对 `#include` 的锚点；文件不必真的存在，前提是给了 `source` |
| `source` | 否 | GLSL 文本。缺省时读取 `path` 指向的文件 |
| `stage` | 否 | `vertex` / `fragment` / `compute`（也接受 `vert`、`frag`、`vs`、`ps`、`pixel`、`comp`、`cs`）；缺省按扩展名推断（`.vsh`/`.vert` → vertex，`.fsh`/`.frag` → fragment，`.comp` → compute） |
| `entry` | 否 | 入口函数名，默认 `main` |
| `include_roots` | 否 | `#include <minecraft:x>` 的搜索根，按顺序尝试 `<root>/include/x`、`<root>/x`、`<root>/shaders/include/x` |
| `defines` | 否 | 预处理宏。值为 `null` 表示只定义不赋值（等价于 `-D NAME`），字符串表示赋值（等价于 `-D NAME=VALUE`） |
| `shader_model` | 否 | `6_0` .. `6_9`，默认 `6_0` |
| `space` | 否 | 所有资源寄存器所在的 DX12 register space，默认 `0` |

`defines` 必须由宿主按 pipeline 提供：原版在 `RenderPipelines` 里逐 pipeline 注入（例如 `MULTIDRAW_TERRAIN`、`OIT_COEFF_COUNT=8`、`PORTAL_LAYERS=15`），转换器不猜。

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
{ "ok": false, "kind": "glsl-frontend", "message": "GLSL frontend rejected the shader:\n..." }
```

`kind` 取值：

| kind | 含义 |
| --- | --- |
| `request` | 请求 JSON 不合法（缺字段、类型不对） |
| `preprocess` | `#include` 缺失/成环、条件指令不配对、表达式不可求值 |
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

这里只是接口示意：接入宿主时还要按 `glsl_hlsl_conversion_contract.md` §7 完成根签名、静态采样器与反射校验，本期不改 `Illusion` / `MCDR` 源码。

## 6. 版本策略

`ABI_VERSION` 只在请求/响应 schema 发生不兼容变化时增加。新增可选字段、新增 `kind` 取值、`manifest` 增加字段都属于兼容变化，不升版本；宿主读不认识的字段应当忽略。
