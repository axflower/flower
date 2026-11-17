# Flower 离线 AI 纠错实验版

0.5.11 候选展示以 [新显示合同](input-commands-case-and-clean-candidates.md) 为准：
纠正尾部统一 `√`，不再显示“AI”；模型、授权与学习边界不变。

0.4.15用户明确选择默认兼容未声明范围的应用，适用安全边界与设置以
`default-ai-scope-compatibility.md` 为准；本页与之冲突的未知范围拒绝条款为历史。

用户已同意优先复用 MacBERT4CSC INT8，快速本地纠错、较小空间占用，
后台生成候选，不自动改已提交文字。本契约基于 `cf0c191410b7043ea88eec16a25b4397ac617276`
上的未提交0.4.5改动，保留圆角/Shift/学习等行为，不回滚已有内容，只递增补丁号。

## 决策与边界

1. **非聊天生成模型**：使用固定 Xenova ONNX INT8 转换制品，完整原句输入、
   逐位置分类；仅处理4–64个基本汉字，首版拒绝混合ASCII/未知token，不猜分词偏移。
   无需云服务、Python、GPU或执行模型仓库代码。
2. **独立推理进程，单用户共享**：复用受保护安装载荷内的 `flower-settings.exe --ai-server`，
   原生CPU ONNX Runtime只在该进程加载。TSF只含小型IPC客户端，不直接初始化ORT。
   一个用户、一个安装版本的 broker 串行有界处理请求；空闲60秒退出，不登录自启。
3. **单一完整分发（0.4.7起）**：只生成内置模型与ORT的ZIP/EXE，外部文件名统一为
   `flower-版本-windows-x64.zip` / `flower-版本-windows-x64-setup.exe`，不带ai后缀；
   内部 `variant=ai` 与固定 `ai/` 文件集继续表达真实载荷身份。
   实际构建必须显式提供可信资产目录，缺失即失败，不静默生成普通包，不运行时下载。
   旧0.4.6制品不修改。新默认设置为开启，用户已明确同意；旧JSON未包含该字段时采用
   新默认，但显式false始终保留，不覆写用户配置。设置变更仍重新切换激活生效。
   缺组件/校验失败不影响普通输入，隐私与显式选择边界不变。
4. **只提建议**：只检查当前未提交的 confirmed 汉字＋第一个完整消费剩余拼音的原候选，
   不读取周边宿主正文，不回溯已提交文本。返回建议必须由用户显式选择。
   AI选择不产生任何 LearningEvent/PromotionEvent，也不携带旧confirmed学习事件。
   AI补入首屏第二项，仅在仍是首页/高亮首项且同一候选快照时进行；
   不替换首项，不抢用户已移动的选择，纠正候选显示 `✓ AI` 标识。
   对号和AI标签仅用于候选显示、测量和分页，绝不写入提交正文。
   若纠正文本已存在于普通完整候选中，移除该普通副本并将同一纠正内容作为第2项
   `✓ AI` 候选展示，不再静默丢弃AI佐证；保留原首项，禁止重复文本，选择仍不学习。

## 固定模型与许可

- 源：`Xenova/macbert4csc-base-chinese`
  revision `7ebfe81cf502576e93c844b95354840b2ecb5c28`，
  `onnx/model_int8.onnx`，103126077字节，
  SHA256 `11fd06011b1381bcb2f5ac1cd9c208d502ce652876c79c2903bcd786e0bfb97b`。
- tokenizer同revision的 `vocab.txt`，21128项，CLS101/SEP102/UNK100；
  输入基本汉字逐字映射，不能把WordPiece通用decode删空格当成字符对齐方案。
- 原权重 `shibing624/macbert4csc-base-chinese` revision
  `615e6e09ef9a69ec487bc7c641ec3a311e2c11b9` 模型卡声明Apache-2.0。
  转换仓库没有独立LICENSE，保留原模型卡、转换说明和Apache全文，明确转换归属和该限制，
  不声称训练数据权利已逐一审核。权重和runtime下载后必须本地复核摘要。
- ORT固定1.20.0 CPU Windows x64，Rust `ort`和`ort-sys`均精确锁定
  `=2.0.0-rc.9`，关闭自动下载/拷贝runtime，显式load-dynamic。
  runtime ZIP、本次选用DLL及其许可、ThirdPartyNotices下载后固定摘要，不混配版本。
  保持项目声明MSRV1.75；如锁定依赖不兼容，先解决/明确报告，不悄悄提高。

## 模型输出策略

- batch1，输入长度≤66（含CLS/SEP）；实际图输入名称/类型和输出shape以ORT加载验证，
  只接受预期 `[1, sequence, 21128]` 浮点logits，非有限数或维度异常拒绝。
- 只允许相同字符数的基本汉字替换；不输出特殊token/WordPiece/字母/空白；
  最多2字且不超过原句25%发生替换。原句相同返回无建议。
- 每个替换位置softmax置信度≥0.98，且候选对原字logit差≥4；保守默认可根据
  固定验证集证据收紧，不为追求演示而无证据放宽。不承诺自动语法润色；
  0.4.12新增的单次拼音编辑候选和独立模型门禁见 `pinyin-repair-and-ranking.md`，
  不降低本段原汉字CSC门槛。
  0.4.11改为逐字保留：不达阈值或top1不是单个基本汉字的位置保留原字，
  不再连带否决同句其他高置信纠正；全局最多2字/25%限制继续在最终建议上执行。
  固定24错句/24次正确对照中，维持0.98/4可从6条完整命中增加到7条并修正2条的部分错字，
  未观察到错误编辑；降为0.95/3会误改两个正确句的“他→她”，因此不采用。
  这些是小集合证据，不外推通用准确率；模型top1本来就是原字时仍可能漏纠。

## 固定资产清单格式

`data/ai/manifest.json` 为构建时钉死的源记录；增强包内 `ai/manifest.json`
必须逐字相同，不能以用户提供的manifest自证可信。格式固定为：

```json
{
  "schema_version": 1,
  "model": {
    "repository": "Xenova/macbert4csc-base-chinese",
    "revision": "7ebfe81cf502576e93c844b95354840b2ecb5c28",
    "license": "Apache-2.0",
    "upstream_repository": "shibing624/macbert4csc-base-chinese",
    "upstream_revision": "615e6e09ef9a69ec487bc7c641ec3a311e2c11b9"
  },
  "runtime": {
    "version": "1.20.0",
    "license": "MIT",
    "archive_url": "https://github.com/microsoft/onnxruntime/releases/download/v1.20.0/onnxruntime-win-x64-1.20.0.zip",
    "archive_sha256": "下载后实测的64位小写十六进制摘要"
  },
  "files": [
    {"path": "model_int8.onnx", "size": 103126077, "sha256": "11fd06011b1381bcb2f5ac1cd9c208d502ce652876c79c2903bcd786e0bfb97b"}
  ]
}
```

上例files仅展示一条结构；正式记录必须恰好包含八个固定文件（不含manifest自身）：
model_int8.onnx、onnxruntime.dll、vocab.txt、LICENSE-model.txt、
LICENSE-onnxruntime.txt、ThirdPartyNotices-onnxruntime.txt、README-model.md、
README-conversion.md。所有大小和摘要实际下载后填写，不保留示例占位。
inference将该源记录编译进worker用于比对，package读取同一源记录，
不额外维护另一套可漂移的词表/DLL摘要。

## 线程、调度和隐私

- 0.4.8将后台连接、冷加载与结果的总等待上限从1秒改为5秒，IPC与TSF使用同一常量。
  这不阻塞按键；所有source/revision/焦点/上下文/两次安全门禁继续严格生效，
  旧结果仍丢弃，不以延时换取陈旧建议。超时不无限重试或绕过隐私范围。
- 候选窗增加不可选的AI状态提示行，不恢复旧翻页页脚/按钮，不参与候选编号/提交/学习。
  状态仅固定枚举：关闭、组件不可用、等待停顿、等待完整候选、范围受限、检查中、
  无纠正建议、已有纠正建议、超时或组件故障、编辑会话失败。
  不含输入、宿主正文或错误原始路径，不写日志/文件；状态行不授予处理敏感字段的权限。
  浏览器/Electron未知范围仍明确提示受限而不默默失败，不把开关勾选或目录存在当运行就绪。
  任何真实输入、失焦或切换后失效时清除旧“已纠正/无建议”状态。

- 新默认开启（显式关闭仍保留），普通候选立即发布。用户停顿250ms后才准备请求；
  每激活只有一个待处理文本、一个最新结果，最多4KiB IPC帧，broker串行处理，
  无无限队列，无文本持久化/日志/缓存。
- 在TSF线程的合法只读edit session中，复用 `GetAppProperty(GUID_PROP_INPUTSCOPE)`。
  0.4.10 的AI允许集合为明确声明的默认0、搜索50、增量搜索52、中文半角53、
  中文全角54、本地文字55、正文57、聊天58、无表情聊天68；这些文本类型均可形成
  既有4–64字中文源句。0.4.10当时只扩充AI集合；0.4.12普通学习范围统一及原生兼容
  以 `pinyin-repair-and-ranking.md` 为准。
  密码31、private61、数字密码63、PIN64/65/66、其他类型、未知/空/失败及混合敏感全部拒绝。
  不更换属性通道、不把VT_EMPTY当默认0、不扩展读取正文范围或缓存前次安全结果。
  拒绝状态以固定枚举区分选区、应用属性、属性读取、未声明、无效值、接口、
  scope读取/空列表/异常列表、敏感和不支持类型；只显示固定原因，不记录输入或路径。
  0.4.11按本轮用户明确选择增加普通原生编辑框兼容与IS_FILE_FILENAME=3；
  完整条件见 `long-candidates-and-ai-compatibility.md` 的“AI输入范围调查”。
  这项用户授权例外只对AI的Property/Value/Undeclared/Empty生效，必须重新验证
  同进程同线程焦点与context view关联、受支持原生类、style和原生类过程密码字符查询；
  不把类名或非密码style单独当许可，也不声称它能补出应用缺失的私密声明。
  明确敏感、不支持、格式/接口/列表异常仍拒绝；0.4.12普通学习复用同一原生组合验证。
  Chromium风格ACP隔离测试与官方读取示例均支持当前GetAppProperty入口；
  该测试通过不能作为真实Edge/编辑器已恢复的证据。
  **检查通过后才构造并交给后台线程文本**，AI同意独立于本地学习开关。
  结果发布前再检查安全范围。后台线程不能持有或使用COM context/edit cookie。
- 推理请求包含单调request_id；客户端有传输期限，应用结果只在派发后5000ms内接受。
  冷加载不阻塞UI，未就绪/过期结果丢弃，之后可在下一次输入重试；
  不声称冷启动也满足热推理时延。
- token、owner thread、foreground、focus epoch、context generation、top context、
  popup revision、源句/原拼音身份及仍存活composition全部一致才可发出/应用结果。
  按键、分段选择、翻页/移动高亮、Shift、取消、提交、失焦/销毁使旧请求失效。
- 弹窗隐藏窗口新增仅用于调度的50ms timer，回调先释放窗口state借用，
  不在WndProc直接写宿主文档。安全检查用异步只读edit session；
  AI注入只更新引擎候选和popup，不修改宿主预编辑正文。
- AI客户端线程stop/join后才释放独立DLL存活计数，连接和读写有界，
  callback使用弱激活引用；窗口销毁取消timer及回调，broker不持有TSF DLL引用。

## IPC与装载安全

- 本机命名管道，名称包含当前用户SID和安装版本/目录身份；
  DACL只允许该用户及SYSTEM，拒绝远程客户端，单实例防重复broker。
  客户端验证server进程映像是本安装的settings EXE且同用户，拒绝未知server。
  输入仅通过管道传输，不放命令行、环境变量或临时文件。
- spawn只用固定绝对EXE路径和固定参数，不通过shell；不接受网络监听/任意helper路径。
  在加载模型前检查固定bundle manifest的大小/hash/许可标识；
  ORT DLL从同一受保护载荷内 `ai/onnxruntime.dll` 显式绝对路径加载，
  不用cwd/PATH/ORT_DYLIB_PATH，限制传递DLL搜索。
  broker拒绝提升token，初始化失败仅退出，不回退提升或云端。
- AI文件仅来自明确构建输入并进入现有包内hash/白名单/总大小边界，
  未签名测试包仍不能证明发布者身份。测试只使用本Thread自有目录，
  不安装/注册/启用真实输入法，不修改已有ACL或真实词典。

## 实施接口和所有权

- `src/ai/mod.rs`：根定义共有 `AiInput { raw, source, reading }`、
  `Request { id, text, raw, reading }`、`Response { id, corrected: Option<String>, reading }`，
  `AI_COMMENT`、长度/帧/超时常量及校正输出验证。
- `EngineRouter::ai_input() -> Option<AiInput>`；
  `apply_ai_candidate(&AiInput, &str) -> Option<CompositionUpdate>`；
  拼音修复用 `apply_ai_spelling_candidate(&AiInput, &str, &str)` 重验raw/reading证明；
  根实施引擎身份、AI整句提交/不学习语义及真实分页。
- `src/ai/ipc.rs`：独立实现本地broker/后台客户端，
  `AiClient::start(helper: PathBuf) -> Result<Self,String>`、
  `submit(Request) -> bool`、`try_result() -> Option<Response>`、`shutdown()`；
  不在submit/try_result做IO或阻塞；broker
  生产 `serve_requests(correct: impl FnMut(&Request)->Result<Response,String>)`；
  保留原 `serve(correct: impl FnMut(&str)->Result<Option<String>,String>)` 兼容文本测试入口。
- `src/ai/inference.rs`：仅由settings二进制包含，负责固定bundle校验、原生模型加载、
  logits过滤及 `run_server`/显式测试推理入口；不引用TSF/真实用户配置。
- 根负责架构文档、共享接口、TSF/timer/CLI及GUI接线；可委派已定契约内
  inference/IPC/离线打包实现，互斥文件不交叉改动。

## 验证和交付

依赖许可证继续按本地实际正文收集，不下载、不以 SPDX 名称或根项目许可证替代。
新增动态加载依赖的 ISC 正文允许没有标题，但必须完整匹配其授权、保留版权和许可通知
条件、无担保及责任排除正文（仅容忍空白差异），并有版权声明；仅标题、截断正文或
仅在 Cargo 中声明 ISC 仍拒绝打包。识别成功不省略原始正文随包分发。
同样允许没有标题的标准 MIT 正文，但新增识别路径必须包含版权声明和完整授权、
通知保留、无担保及责任排除正文，仅容忍空白差异，不凭首句或声明放行。
兼容固定依赖使用的旧 Cargo 双许可表达式 `MIT/Apache-2.0`，仅将这一精确值解析为
`MIT OR Apache-2.0`；其他斜线表达式仍拒绝。保留原声明并收集两份实际正文，
不把兼容解析变成缺失许可证的豁免。

先用真实固定模型验证图签名、代表纠错与正确句不改，记录热P50/P95/冷加载、
峰值工作集、模型及运行库体积；达不到合理速度或误改率则明确阻塞，不用fixture冒充。
定向覆盖AI候选所有选择入口/confirmed恢复/不学习、队列/超时/非法响应、
scope敏感矩阵与实际生产发出前门禁、失焦代次/撤销/重复结果/销毁与退出。
实际运行使用隔离STA/TSF或显式probe，不读用户文档，不自动全局输入。
独立审查安全及异步接线，严格Clippy/格式及受影响分发检查。
仅交付完整模型包，保留固定模型来源/许可/内外摘要；真实桌面及签名发行仍另验收。

## 0.4.7 中文标点行为

中文模式将常用ASCII按键转换为中文标点：逗号/句号/问号/感叹号/分号/冒号对应
`，。？！；：`，圆括号 `（）`、方括号 `【】`、书名号 `《》`，
斜线和反斜线为 `、`，波浪为 `～`，Shift+6为 `……`，Shift+减号为 `——`。
双引号交替 `“”`、非组词中的单引号交替 `‘’`；单引号在组词中仍为拼音分音符。
引号方向在当前引擎会话内独立维护，不读取宿主正文，结束一次组词不重置。
Shift数字先映射实际符号，不能误选数字候选；未定义符号保持宿主处理。
英文模式和Ctrl/Alt/Win快捷键仍原样直通，不能被中文标点接管。

无组词时标点直接作为一次提交；有组词时按标点等同确认当前高亮的完整候选再追加标点，
完整消费不了剩余拼音时提交已有confirmed文本加剩余原拼音，不丢失后半输入。
不把部分候选确认后遗留拼音的中间状态与标点混在一次提交；标点本身不学习。
默认高亮仍是原首项，标点不会自动选择后台AI建议；用户主动高亮AI后再确认才使用该项，
保持AI整句选择不学习。原 `-`/`=` 及对应加号翻页、PageUp/PageDown行为不变。
OnTest只在clone上预测，不能改变引号方向或活跃输入。错误或失焦的TSF提交门禁沿用既有路径。
空闲标点没有活动composition，TSF宿主在合法写会话中直接通过InsertTextAtSelection插入，
不要求先存在拼音预编辑；移动光标与错误传播仍复用既有规则。
