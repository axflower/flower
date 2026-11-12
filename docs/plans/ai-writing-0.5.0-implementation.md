# Flower 0.5.0 在线帮写与润色实施计划

最新用户变更：独立帮写窗口＋设置中的润色参数＋原文旁非激活候选浮层；
当前契约见 `design/ai-writing-and-inline-polish-0.5.0.md`，下文按此修订。

目标：落实已确认的在线 BYOK、仅智谱 GLM-4.7-Flash 路线。
产品、隐私与行为契约以 `design/ai-writing-and-inline-polish-0.5.0.md` 为唯一依据；
本文仅记录实施顺序与验收归属，不扩展首版范围。

## 1. 在线调用基础

- 新建 `src/writing/mod.rs`：独立任务类型、500/800 字边界、固定模型与提示模板，
  静态安全错误分类，密钥与宿主身份不进入任务对象。
- 新建 `src/writing/stream.rs`：有界 SSE/UTF-8 分块、正常完成标记、拒绝/截断。
- 新建 `src/writing/credentials.rs`：当前用户 Windows 凭据管理器，
  默认不读真实凭据进行测试，禁止明文落入配置/日志。
- 新建 `src/writing/transport.rs`：固定 HTTPS 的异步 WinHTTP，禁止重定向，
  取消/超时后的句柄与回调生命周期闭合。
- 新建 `src/writing/worker.rs`：有界最新快照、单任务身份、取消和 join。
- 定向入口：`cargo test --offline --lib writing::`。必须命中新用例，
  包含每个 UTF-8 字节切分点、截断、超限、取消和密钥格式，不能调用真实 API。

## 2. 密钥配置与手动面板

- `src/platform/writing_ui.rs`：改为独立帮写窗口、卡片美化、流式预览、
  受保护连接配置、首次外发告知、显式复制/插入，以及本地可滚动使用帮助。
- `src/platform/online_settings_ui.rs`：通过主设置入口配置启用/授权、风格、字形及密钥；
  `Settings.online_polish` 缺省不启用，普通设置保存不得覆盖新在线参数。
- 在 `src/platform/mod.rs` 导出平台模块；在 `src/bin/flower-settings.rs`
  仅保留 `--writing`，撤销旧 `--polish` 手动工作窗口，保持GUI无控制台与严格参数校验。
- 纯状态测试覆盖失败保留旧结果、未完成不可应用、应用至多一次、停止/关闭晚结果。
  自有窗口原生夹具只检查控件与生命周期，不访问用户凭据、剪贴板或网络。
- 定向入口：`cargo test --offline --bin flower-settings` 以及新增面板状态测试。

## 3. 快捷键和宿主安全集成

- 在 `src/platform/windows.rs` 增加精确命令判断、OnTest 纯预测、
  首按/重复控制、线程所有的面板与 DLL 生命周期登记。
- 新增独立捕获/应用编辑会话和编辑观察器，不放宽已有纠错票据、焦点门禁。
  未知输入范围失败关闭，安全目标不可证明时仅手动/复制。
- `src/platform/polish_ui.rs`承载不抢焦点的原文旁候选浮层，Ctrl+2仅在明确启用、
  安全非空选区及可靠绑定后请求生成。预览不改正文，点击使用时原焦点保持路径
  单独核验；不复用帮写窗口的恢复焦点门禁。
- `docs/glm-4.7-flash-setup.txt`为内置帮助唯一正文，
  `writing::help::SETUP_GUIDE`供帮写及设置复用，不自动联网。
- 定向入口：新增隔离 TSF 夹具，以及受影响的
  `keyboard_symbols_settings`、`engine_router` 测试。
  不注册/启用输入法，不注入全局键，不操作真实用户宿主。

## 4. 整合和交付

- 对共享接口、取消清理、密钥保护与宿主写回实施独立审查；
  修复后仅补受影响证据，不惯性重复全部检查。
- 集中执行格式、严格 Clippy 与必要的原生构建，记录准确测试数和未覆盖项。
- 公开文档同步实际实现，不把未进行的真实模型质量、网络延迟或真实宿主验收标为通过。
- 本次不自动安装/注册/启用，不读写真实用户配置，不自动提交 Git，
  不覆盖旧版 `dist` 制品。
