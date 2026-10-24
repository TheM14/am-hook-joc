# am-hook-joc（简体中文）

[English](README.md) | 简体中文

为 [am-hook](https://github.com/itouakirai/am-hook) 实现 E-AC-3 JOC（Dolby
Atmos）的对象解码与立体声渲染，纯 Rust 编写并编译为 WebAssembly。

播放器继续使用 am-hook 自带的 EC-3 WASM 解码器得到核心 PCM，从同一批同步帧中读取
JOC 元数据，重建 15 个对象与 LFE，并在同一个 Worker 内渲染为立体声。

## 功能

* 解析 JOC / EMDF / ID14 / ID11（OAMD），包括 bed 方案与对象位置。
* 15 个对象与 LFE 的重建：64 频带 QMF 分析与合成、矩阵插值、LFE 延迟、Ls/Rs 相位偏移。
* **Speaker 2.0**：等功率声像渲染到两只扬声器，带地面/高度衰减。
* **Binaural**：77 频带混合滤波器组、5 阶球谐场、6 个镜像声源、共享 FDN 房间，
  使用编译好的 `.jochrtf` HRTF 场；SADIE II 的 D1、D2、H3–H20 可在设置里选择。
* 输出衰减（默认 **0 dB**）；发生削波时建议 -4 dB 或更低，播放条会提示。
* Seek 带 64 同步帧预卷；JOC 开启时专辑/队列优先选择 Atmos；设置页支持中英文。
* 全部在浏览器内完成：音频不会上传，解码与渲染都在 Web Worker，主线程只负责调度
  AudioBuffer。

## 环境要求

* Rust，且安装 `wasm32-unknown-unknown` target
* Node.js 20+（用于通过 `npx` 调用 `wasm-opt`，以及浏览器测试）
* 一份干净的、与补丁对应的 am-hook 上游版本工作副本；播放时还需要它平时使用的后端（wrapper-lite）

## 构建

```sh
git clone https://github.com/itouakirai/am-hook.git
cd am-hook && git checkout <upstream-tag>

# 在本仓库中执行：
bash scripts/apply.sh ../am-hook --tag <upstream-tag>   # 应用对应补丁
bash scripts/build.sh ../am-hook                        # 构建 joc.wasm 和 am-hook
```

## HRTF 来源

双耳渲染使用约克大学公开的
[SADIE II 数据库](https://www.york.ac.uk/sadie-project/database.html)编译的场
（Apache-2.0）。构建会带上 **D1、D2、H3–H20** 共 20 组测量（`.jochrtf` 在
`hrtf/` 目录），在**设置 → JOC 对象渲染**里切换，默认 D2。SADIE 网站上有每组
受试者的说明，用户可对照自己的头型、耳型挑选最接近的一组。

这些场由 `tools/compile_hrtf.py` 从 SOFA 编译（最小相位化、延迟放进
`Data.Delay`、保留 SOFA 元数据），`scripts/build.sh` 负责安装到打补丁后的工作副本。

## 运行

```sh
../am-hook/target/release/am-hook --listen 127.0.0.1:8888 \
    --wrapper-url http://127.0.0.1:12340
```

打开 <http://127.0.0.1:8888>，进入**设置 → JOC 对象渲染**，选择 *Speaker 2.0* 或
*Binaural*，然后播放 Dolby Atmos 曲目。设置页顶部有直接跳到该栏目的入口。

## 仓库结构

```
patches/                 每个上游版本一份补丁
hrtf/                    编译好的 SADIE II HRTF 场
joc-wasm/                Rust workspace
  crates/joc-core/       码流、DSP 与流式状态（本机与 wasm 共用）
  crates/joc-wasm/       浏览器 Worker 使用的 C ABI
  crates/joc-cli/        本机驱动
scripts/                 apply / build 等辅助脚本
tests/                   测试工具
tools/                   表生成器、C++ 对比导出工具
```

## 许可证

整体按 am-hook 的 AGPL-3.0-only 分发：补丁与浏览器集成属于 am-hook 的衍生作品。
`joc-wasm/` 下的 Rust 渲染实现同时以 MIT 许可提供（`joc-wasm/LICENSE`）。
第三方组件见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
