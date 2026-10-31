# Illusion shaders extension

把 Minecraft 原版与光影包的 GLSL 转换成 HLSL（DXC 可直接编译），并以 C ABI 的 DLL 形态供 `Illusion` 宿主调用。

- 发布产物：`target\release\illusion_shader.dll`（`cargo build --release`，只产 DLL）。
- 开发工具：`shader-convert` CLI（`cli\` 包，`cargo run -p shader-convert-cli -- …`），只在开发与测试时构建。
- 宿主接入：`..\Illusion` 通过 FFM 加载 DLL 并调用 `illusion_shader_convert`，契约见 [docs/shader-convert-abi.md](docs/shader-convert-abi.md) 与 [docs/glsl_hlsl_conversion_contract.md](docs/glsl_hlsl_conversion_contract.md)。
- 仓库位置：本目录是 `NTS-Official/Illusion-shaders-processor` 的独立克隆（唯一源码）。`..\MCDR\external\gl2hl` 是同一仓库的 submodule，索引记录在 `fb7be74`、工作树检出到 `4f39ce2`，两者都早于本目录的当前改动；`MCDR` 的构建与 `_port` 下的检查脚本只读本目录，不要从 submodule 构建 DLL。

## 快速开始

```powershell
cd D:\Projects\MinecraftMod\DXRMC\Illusion-shaders-extension
$env:RUST_MIN_STACK = '33554432'

cargo build --release                     # -> target\release\illusion_shader.dll
cargo test --workspace                    # 109 条测试：单测 + 快照/确定性/诊断/残缺输入/命令行

# 原版语料：转换 + 离线校验（verify 需要 DXC）
$shaders = '..\_research\mc263-src\assets\minecraft\shaders'
cargo run --release -p shader-convert-cli -- audit  $shaders --defines-file tests\fixtures\corpus-defines.json
cargo run --release -p shader-convert-cli -- verify $shaders --shader-model 6_0

# 光影包语料：外部宿主程序（Colorwheel / DH）用 --skip 排除，compute 需要加载器特性宏
$pack = 'input\ComplementaryUnbound_r5.8.1\shaders'
cargo run --release -p shader-convert-cli -- audit  $pack --include $pack --uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json --skip dh_ --skip clrwl_
cargo run --release -p shader-convert-cli -- verify $pack --include $pack --uniform-block CommonUniforms --legacy-builtins --defines-file tests\fixtures\pack-compute-defines.json --skip dh_ --skip clrwl_ --shader-model 6_0
```

单文件转换：`cargo run -p shader-convert-cli -- convert <file> --out out`，产物是 `<name>.hlsl` + `<name>.manifest.json`。

## 目录

| 路径 | 内容 |
| --- | --- |
| `src\` | 库：`preprocess`（include 与宏）、`dialect`（GLSL 方言改写）、`compile`（naga 前端 → HLSL）、`manifest`（资源与布局清单）、`verify`（DXC 闭环）、`report`（JSON 报告）、`abi`（C ABI） |
| `cli\` | 开发用 CLI：`convert` / `normalize` / `audit` / `verify` |
| `tests\` | 集成测试（`golden.rs` 快照、`deterministic.rs` 确定性、`diagnostics.rs` 错误位置、`robustness.rs` 残缺输入）与夹具 |
| `docs\` | 契约与验证文档，入口是 [docs/README.md](docs/README.md) |
| `..\naga-for-hlsl\` | 本仓库依赖的 naga fork（含 6 处针对本链路的补丁，见 [docs/shader_pack_frontend.md](docs/shader_pack_frontend.md) §10.1） |

## 状态

| 目标 | 结果 |
| --- | --- |
| 原版 26.3（72 个 shader） | 72/72 可转换，`verify` 6.0 与 6.9 均 72/72 |
| 光影包 Complementary r5.8.1（219 个入口） | 195 可转换；剩下 24 个全是外部宿主程序（Colorwheel 12、Distant Horizons 12）。排除它们后 183/183 可转换，补上 `tests\fixtures\pack-compute-defines.json`（加载器给 compute 的特性宏）后 `verify` 6.0 与 6.9 均 183/183，其中包括 3 个 compute 的 UAV 组 |
| Illusion 宿主（开发客户端） | 221 条管线用转换器出图（含 12 条地形的 meshlet 变体，AS 剔除开启），无 PSO 失败；带 storage image 的管线按 `storage image` 跳过（转换器产物已可用，宿主绑定层还没有 UAV 槽位） |
| 测试 | 109 条：95 个单测 + 4 条命令行用例 + 17 份 HLSL/manifest 快照 + 1 条转换确定性 + 3 条诊断位置用例 + 4 条残缺输入用例 |

## 外部输入

`input\`（第三方光影包）与 `ref_shader_loader\`（Iris 参考克隆）只在本机存在、不入库，原因见 [docs/README.md](docs/README.md) 的「外部输入」。`verify` 还需要 DXC（`--dxc <path>` → `PATH` → `VULKAN_SDK\Bin\dxc.exe`）。

## 许可

本仓库以 LGPL-2.1 发布（[LICENSE](LICENSE)）。依赖的 naga fork 保留上游 MIT / Apache-2.0 许可；`input\` 与 `ref_shader_loader\` 不随本仓库分发。









