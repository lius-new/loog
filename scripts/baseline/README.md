# 回归检查与性能采样

使用 Node.js 20+；CI 使用 Node 24。Rust 版本由 `rust-toolchain.toml` 固定。
运行器使用共享的 CMake/Skia 依赖准备模块，不需要 PowerShell。
构建前置条件见 [构建说明](../build/README.md)。

## 运行方式

从项目根目录执行。默认只采集全部性能组，每组先预热一次，再采集五个正式样本：

```sh
node scripts/baseline/run.mjs --offline --samples 5
node scripts/baseline/run.mjs --performance editor,ui --samples 3
node scripts/baseline/run.mjs --performance file,search,terminal --samples 5
```

`--offline` 要求 Rust、Cargo 和托管构建依赖已经缓存；缺失时失败，不回退联网。
`--skia-archive <path>` 可指定符合锁定版本、目标和 SHA-256 的 Skia 归档。
`--samples` 接受 1～20。`--output-directory <path>` 指定新的空目录；默认使用
`target/baseline/<UTC 时间戳>`。拒绝非空目录，避免混入不同源码的结果。

回归检查、Git 配置隔离复测和 Windows 原生终端检查是独立选项，可以组合：

```sh
node scripts/baseline/run.mjs --checks --performance none --offline
node scripts/baseline/run.mjs --isolated --performance none --offline
node scripts/baseline/run.mjs --native-smoke --performance none --offline
node scripts/baseline/run.mjs --checks --isolated --native-smoke --performance all --offline
```

- `--checks` 执行架构检查及其四组自测、baseline 工具自测、格式检查、Clippy、
  workspace debug/release 测试和 release Loog 构建。
- `--isolated` 额外执行一次 workspace debug 测试，子进程使用合成 Git 配置，
  禁用系统/用户 Git 配置并固定 LF；不修改用户配置。
- `--native-smoke` 仅限 Windows，逐项运行四个精确指定的 ignored 终端测试，
  覆盖 ConPTY shell 往返、launcher 空闲/程序检测、Git Bash 进程替换和程序名清理。
  本机需要可用的 PowerShell、Cmd 和 Git Bash。
- 性能组和原生终端检查始终使用隔离 Git 配置。所有 PATH、CMake、Skia 和 Git
  环境仅传给子进程。检查失败后继续收集其他检查；性能组的某个样本失败后停止该组。
  编译失败、空匹配、缺失日志/指标/样本或中断均导致最终非零退出。

`--checks` 是本地完整回归入口，不替代 CI：CI 还执行语言包/LSP 发布工具测试、
WASM runtime 测试、跨平台打包及 Markdown 原生/UI 证据检查。
GUI 启动检查需要另外执行。

## 性能组

| 组 | 场景 | 必需指标数/样本 |
| --- | --- | --- |
| `language-service` | 8 MiB 文档增量编辑规划（独立性能门槛） | 1 |
| `editor` | 1/10 MiB 文档输入、粘贴、撤销、缩进和历史内存 | 10 |
| `ui` | 大文件交互（ASCII/Unicode/多行）及 100/500 标签页指针更新 | 9 + 2 |
| `file` | 1/10/64 MiB 缓存文件读取及文档准备 | 6 |
| `search` | 100 文件扫描；取消延迟仅在实际观察到取消时记录 | 1，另有 1 项可缺测 |
| `terminal` | ANSI/Unicode 解析、屏幕内容提取及 1/8/32 会话生产路径快照/帧构建 | 2 + 12 |

使用 `all` 选择全部，`none` 只执行显式检查模式；逗号分隔可组合用途组。
精确 Rust 入口和指标清单位于 `cases.mjs`。每次性能/原生检查必须实际通过一个
指定名称的测试，零匹配不能视为通过；普通 workspace 的空 doctest 仍合法。

只对同机、同工具链、相同输入与窗口条件比较性能。预热样本 0 不参与统计。
报告展示正式样本数、中位数和范围，不自动判定跨运行的性能回归。
搜索若在取消请求之前已经结束，取消延迟显示为缺测，不编造零延迟。
终端历史 `terminal_screen_snapshot_100` 只测 `screen.contents()`；生产路径另测
屏幕快照、纯模型快照、帧投影和 UI 帧构建。UiSession 测量不等于原生窗口帧率、
输入延迟或 IME 验收。

编辑器真实排版/高亮连续性、Git 大仓库面板等额外 ignored 测试尚未纳入默认采样；
语言包条件和输出协议应在接入前明确。不要全局执行所有 ignored 测试。

大文档编辑的正确性仍由普通 workspace 测试验证；1.5 秒性能门槛仅在
显式采样时运行，使用 release 构建、独立进程和单测试线程：

~~~sh
node scripts/baseline/run.mjs --performance language-service --samples 3 --offline
~~~

## 隔离 GUI 启动检查（Windows）

启动观察结束后，在启动总期限内等待被测子进程的主窗口就绪，再请求正常关闭。
启动和关闭使用独立期限。回归测试注入窗口创建/关闭延迟，并验证窗口始终不出现、
拒绝关闭、错误退出和取消时仍会失败且清理被测子进程：

~~~sh
node --test scripts/baseline/startup-smoke.test.mjs
~~~

先构建并准备运行时资源，然后启动真实 Loog。每次使用新的空输出目录：

```sh
node scripts/build/cargo.mjs build --release --locked --offline
node scripts/build/stage-resources.mjs target/release
node scripts/baseline/startup-smoke.mjs --output-directory target/baseline/startup-check
```

可用 `--executable <path>` 指定其他 Loog.exe。APPDATA、LOCALAPPDATA 和工作目录
都位于测试目录，工作区包含中文名和 emoji 内容的文件；启动设置 `windowsHide`。
观察存活四秒后，通过本地 Rust 辅助程序向该进程的顶层窗口发送 `WM_CLOSE`，
最多等待十秒正常退出，并核验退出码和保存的工作区路径。
辅助程序先核对 PID 和可执行文件路径，不向其他应用广播关闭消息。
只有失败清理时才强制结束本次创建的子进程。

`close-window.rs` 用 `rustc` 单独编译到本次输出目录，没有 npm 或 Cargo 依赖。
结果始终记录在 `startup-smoke.json`（前置参数/目录验证失败除外），编译和进程日志
也留在该目录。正常关闭及会话保存不能用强制结束替代。
该检查不代替原生 IME、视觉和系统焦点的人工验收。

## 报告与验证

```sh
node scripts/baseline/report.mjs <output-directory>
node --test scripts/baseline/test-results.test.mjs scripts/baseline/report.test.mjs scripts/baseline/run.test.mjs scripts/baseline/startup-smoke.test.mjs
```

| 产物 | 内容 |
| --- | --- |
| `environment.json` / `working-tree.txt` | 源码版本、工作区状态、工具链、CPU/内存/OS、依赖和采样计划 |
| `checks.json` / `logs/` | 每个计划命令、退出码、耗时及原始输出；中断/未执行项保留为空退出码 |
| `measurements.json` / `measurements.md` | 完整性、检查状态、按 crate/套件区分的测试结果及性能统计 |
| `performance.csv` | 可导出的正式样本统计 |
| `test-inventory.json` | 带 crate/目标/日志来源的实际测试清单 |
| `clippy-diagnostics.json` | Clippy 错误位置 |

报告综合日志与 `checks.json` 退出码；前面套件成功不能掩盖后续编译失败。
缺失预热、正式样本或必需指标均标记为不完整并返回非零。
历史日志缺少退出码时明确标记为未核验；仍兼容旧性能日志名称、文本和 JSON 标记。
workspace 验证 crate 复用生产规则，其通过数量不代表新增独立行为覆盖。
硬件使用 Node OS 元数据，不再采集 Windows CIM 专有字段。

源码盘点和旧阶段依赖候选扫描已移除，不再生成 `inventory.json`。
`tests/fixtures` 中的会话、设置和快捷键旧格式夹具仍由 Rust 兼容性测试读取，应保留。
Windows 工具自测使用隐藏的合成 GUI，验证正常关闭、会话错误、早退、非零退出、
超时清理和进程身份检查，不会启动用户的 Loog。
