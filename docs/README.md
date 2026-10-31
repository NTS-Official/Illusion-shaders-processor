# Illusion shader 转换器文档

本目录是 GLSL → HLSL 转换器（`illusion_shader`）的契约与说明。仓库总览（构建、目录、状态）见 [../README.md](../README.md)。按这个顺序读：

| 文档 | 读者 | 内容 |
| --- | --- | --- |
| [glsl_hlsl_conversion_contract.md](glsl_hlsl_conversion_contract.md) | 宿主/工具开发者 | **输入输出契约**：请求字段、18 条方言改写、HLSL 与 manifest 的约定、宿主接回时必须遵守的 6 条规则 |
| [shader-convert-abi.md](shader-convert-abi.md) | 宿主（JVM/FFM） | C ABI：三个导出函数、JSON schema、错误 kind、内存所有权、DLL 发布方式 |
| [verification.md](verification.md) | 维护者/CI | `verify` 的三层校验（DXC 编译、资源与入口签名、cbuffer 布局）与快照回归 |
| [vanilla_glsl_hlsl_plan.md](vanilla_glsl_hlsl_plan.md) | 维护者 | 当前实现版方案：流水线、组件、里程碑（M1–M4）与下一步 |
| [rt_light_contract.md](rt_light_contract.md) | 渲染侧 | 光线追踪光照契约（与本转换器无依赖，同仓存放） |
| [vanilla_frontend_survey.md](vanilla_frontend_survey.md) | 调研记录 | 原版 shader 前端勘察（历史结论，含语料统计） |
| [shader_pack_frontend.md](shader_pack_frontend.md) | 光影包前端（M4） | 光影包语料基线、预处理顺序、legacy 内建契约、MRT 与宿主必须提供的值 |

## 当前状态

- 原版语料：Minecraft 26.3 的 72 个 shader，**72/72 可转换**（`audit`）。
- 原版校验：`verify` 在 SM 6.0 与 6.9 下均 **72/72**（DXC 编译 + 资源寄存器 + 入口签名 + cbuffer 偏移）。
- 光影包语料：Complementary r5.8.1 的 219 个入口文件（216 个 `.vsh/.fsh` + 3 个 `.csh`）里 **195 可转换**；剩下 24 个全是外部宿主程序的匹配符号（Colorwheel 12、Distant Horizons 12），排除后 **183/183**，`verify` 在 SM 6.0 与 6.9 下均 **183/183**（3 个 compute 的 UAV 组也在内，需要 `tests\fixtures\pack-compute-defines.json` 提供的加载器特性宏）。
- 测试：109 条（95 个单测、4 条命令行用例、17 份 HLSL/manifest 快照、1 条转换确定性、3 条诊断位置用例、4 条残缺输入用例）。
- 宿主：`Illusion` 开发客户端里 shadow 全量转换（442 条 stage）；primary 留空过滤器时 221 条管线切到转换器，地形管线也由转换器产物建出 meshlet 变体（AS 剔除开启），无 PSO 失败；带 storage image 的管线按 `storage image` 跳过（绑定层还没有 UAV 槽位）。

## 外部输入

`input/`（第三方光影包）与 `ref_shader_loader/`（Iris 参考克隆）都只在本机存在、不入库：前者许可证禁止再分发与修改，后者是 LGPL-3.0 / MPL-2.0 的只读参考。缺了它们时 `audit`/`verify` 只能跑原版语料，单测与快照不受影响。

## 常用命令（工作目录 = 本 crate 根）

```powershell
$env:RUST_MIN_STACK = '33554432'
cargo test --workspace                     # 109 条：单测 + 快照/确定性/诊断/残缺输入/命令行
cargo run -p shader-convert-cli -- audit ..\_research\mc263-src\assets\minecraft\shaders --defines-file tests\fixtures\corpus-defines.json
cargo run -p shader-convert-cli -- verify <shader-dir> --shader-model 6_0
cargo build --release                      # 只产 target\release\illusion_shader.dll
```

光影包（需要 `input\`）用同一组开关：

```powershell
$pack = 'input\ComplementaryUnbound_r5.8.1\shaders'
cargo run --release -p shader-convert-cli -- audit  $pack --include $pack --uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json --skip dh_ --skip clrwl_
cargo run --release -p shader-convert-cli -- verify $pack --include $pack --uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json --skip dh_ --skip clrwl_ --shader-model 6_0
```

`verify` 需要 DXC：`--dxc <path>` → `PATH` → `VULKAN_SDK\Bin\dxc.exe`。光影包语料在 debug 构建下会撑爆主线程栈，请用 `--release`。

每个子命令只接受自己那份选项清单：拼错或把别的子命令的选项塞进来会直接报错并打印用法（`` `audit` does not take `--out` ``），不会被静默忽略。










