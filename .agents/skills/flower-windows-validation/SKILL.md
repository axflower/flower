---
name: flower-windows-validation
description: 在 Flower 仓库执行 Windows x64 原生构建、ZIP 验证、COM/TSF 安装和桌面排障时使用；区分可执行检查、当前用户启用和实际输入证据。
---

# Flower Windows 验证

## 在线 BYOK 写作的隔离验证

- 配置保存、活动模型选择、模型目录同步和真实推理成功分别取证，不把任何一项冒充其它。自定义目录并非都可发现：显式GET base/models只在用户点击时发生，不发正文/不自动生成，模型ID从有效目录选择，无法发现时保留高级手填而不猜默认收费模型。连接校验须独立于modelID校验，空模型可以获取目录但不能生成/应用；有界JSON/ID数、超时取消、编辑目标变更和关闭拒晚结果、旧密钥不跨target是必要回归。
- 主配置“保存并使用”需验证凭据操作结果，再持久化当前选择并回读；密钥成功而配置失败明确部分成功，不能说已应用或回滚未知并发设置。用户说“已切换但生成失败”时优先收集服务商基址和固定安全报错，HTTP404/400不能统称Protocol，5xx保留状态数字；不索取密钥或正文。Coding Plan和标准API的入口/密钥/适用工具分开说明，不伪装受支持工具，不把改地址当套餐授权迁移。

- 自定义模型连接要同时冻结基址/协议/模型/凭据槽，确认显示与实际请求同一snapshot；确认、警告和删除确认的消息泵返回后复核窗口归属及原目标，不只重新验证一个后来变化的profile。密钥编辑字段必须匹配已保存profile，目标变化不转发旧key；激活快照与最新润色授权不符时在选区捕获前拒绝。使用注入deleter测试确认中改选择/同ID端点/关闭/解绑均0调用，正常恰1且命中原target。
- Responses的正文增量、part.done、item.done和response.completed应核身份/索引/状态与正文快照；中间声明incomplete或矛盾快照不能被最终completed洗白。首错锁存后push/finish仍失败。允许显式reasoning元数据但绝不拼入正文，reasoning项可占索引0，文本message索引不必为0；使用reasoning0+text1、工具拒绝、错索引和EOF不完整合成流验证，不用真实付费API补协议测试。Chat reasoning_content同样只作为非可见元数据，正常stop+DONE才完整。

- 发送确认的“不再提示”须以供应商/模型scope精确匹配，只有显式发送加勾选才保存；取消、Esc、关闭不落盘。生成前重读偏好，恢复后已打开面板也生效；偏好更新与主设置保存均合并磁盘最新字段，避免旧快照互相覆盖。不可读配置默认确认，坏配置不覆盖，保存失败告知未记住；原生模态泵返回及告警返回后重新核验pending/请求身份。用注入Store和自有HWND验证默认取消、键盘路由、owner恢复及跨实例重读，不以绕过确认的测试入口证明生产发送安全。

- 滚轮卡顿或反弹还要核消息循环的焦点ensure-visible：不能每条timer/paint/wheel后都把旧焦点控件强拉回视口。缓存几何/字体后，滚动只移动控件不重测；用顶部编辑框保持焦点、PostMessage轮滚/滚动条并经过后续timer的真实模态泵断言不反弹，再Tab/ShiftTab验证新焦点可达。同步SendMessage加父窗口获焦的像素回归不能覆盖此交互缺口。
- 滚动残影先核父子绘制边界：父背景全刷、缺WS_CLIPCHILDREN、逐个MoveWindow重绘后又画父层、透明文本保旧像素都应分别取证。可用批量移动加禁像素复制、统一父子重绘，比较连续thumbtrack/滚轮返回后的稳定客户区RGB与完整重绘；排除caret/动画差异，像素等价不外推所有真实桌面/DWM现象。无变化文本/只读/启用状态避免反复Set，流式追加保留用户选区和视口。
- 服务错误按官方业务码区分账号速率、全局模型过载、周期用量与密钥产品类型，不把所有HTTP429都宣称账号高频。重试默认关闭、用户明确开启、无正文且明确可重试码才有限退避；单调总预算跨尝试，等待可取消。有效Retry-After超本地总预算必须停止，不能因解析上限变成0后提前重发；补大整数/超u32/固定头缓冲不足的保守停止计数测试。只读诊断不得包含密钥、正文或provider message。

- 帮写/润色的默认开关以当前权威合同为准：`design/general-online-ai-and-candidate-quotas.md` 增加默认关闭的在线AI总开关（缺字段也关闭），同时约束帮写与润色；润色子开关默认开启及明确 false 保留不构成总授权。配置模型不自动开启总开关；`design/settings-resources-and-live-online-toggle.md`规定显式保存总开关即对已激活浏览器生效，不再用旧activation开关值双重拒绝，但modelscope/隐私/焦点/写回快照仍独立校验。已有面板生成前、确认回调后、启动worker前均重读；润色还需有效 Flower 上下文、主动安全选区与快捷键、有效模型及精确授权快照。模型目标改变保留子开关，但旧 activation 的 scope 不符应在捕获前拒绝。在线参数保存应从注入 Store 的最新值合并单一字段，主设置表单后续保存也不得用旧快照覆盖刚修改的参数。模态配置夹具要走实际保存返回、Enter不误保存/删除、Esc恢复owner和Tab焦点自动滚入视口。
- 非激活候选浮层与独立工作窗口的焦点规则不同：浮层不得抢焦点，点击使用时要求原宿主焦点保持；不能为了兼容浮层删除独立窗口的前台恢复门禁。窗口样式/MA_NOACTIVATE、自有窗口假结果及同步编辑会话分别取证，不把这些单项当真实前台宿主成功写回。
- 服务接入帮助应明确实际产品类型的密钥（例如标准模型 API 与编程套餐可能不通用），正文以单一内嵌来源供帮助/设置复用；打开帮助和保存配置不请求模型。不要为帮助演示获取用户凭据或自动购买套餐，当前免费不等于永久或无限额度。

- 在线帮写与离线纠错分开取证。假 SSE/假结果只证明协议、状态和接线，不证明真实模型质量、网络延迟、配额或供应商隐私。测试不默认读取当前用户凭据；凭据格式用内存样例，原生凭据读写需专用测试目标与明确授权。
- 异步 WinHTTP 的正文、读取缓冲区和回调 context 必须保持到 HANDLE_CLOSING；可用 Open/Connect/OpenRequest 后不 SendRequest 即关闭的真实句柄夹具证明关闭栅栏，不把它外推为真实发送中的所有取消故障。关闭/join 前保留 DLL 登记，禁止为超时释放仍可能被回调使用的内存。
- 取消检查与终态发布需有同一同步/失效规则。用确定性交错阻塞已准备成功结果的邮箱发布，先取消再放行；另测取消已发布未消费的成功快照。只在发布前多读一次原子标记不能消除检查至发布竞态。
- 旧结果恢复只能恢复原任务内容，不能回滚已撤销的选区绑定、来源 generation 或已消费应用资格。分别测试仅改输入后恢复、明确转手动后恢复、重生失败后恢复与应用失败后的重复点击。
- RAII 重入租约只在成功获取时构造；有 Drop 副作用的守卫不能用急切求值的 then_some 创建。持外层租约连续两次获取失败、释放后成功，可捕捉失败临时守卫误释放外层状态。
- 面板安全写回需补同线程自有窗口的来源→面板前台→恢复原焦点/context→同步写入一次夹具；单测 SetText 或绕过前台恢复不够。测试只用隔离文档和假结果，不注全局键、不读剪贴板、不请求真实模型。系统拒绝前台恢复应明确失败，不能跳过成功路径并声称端到端通过。
- 原生面板检查小工作区、高 DPI、焦点导航和配置区可达性；截图只捕自有 HWND 客户区。固定大窗口加缩放可能让密钥/应用按钮出屏，应验证 resize/滚动/焦点自动可见及 WM_DPICHANGED，而不只检初始布局矩形。

## 稳定刷新与逐段自动词增量

- 候选闪烁先检查生产发布是否对旧caret和新caret重复布局、同屏是否无条件hide/show，不把音节更新等同HWND重建。可用自有HWND记录WM_SHOWWINDOW、geometry及paint消息，证明同屏无可见性翻转且至多一次必要布局；保持新revision撤销旧命中、绘制成功才授权、失败/UIless/真正失焦隐藏。消息计数不外推真实DWM视觉或多屏体验，跨屏仍验证目标DPI。
- 自动词与旧学习计数/相邻调序分离；以`design/stable-popup-and-auto-phrases.md`核对完整逐段选择、规范reading、raw耗尽、成功安全发布及独立小索引。用真实builtin例串引擎、生产TSF按键、当前activation下次输入、fresh context、worker drain及隔离Store重载；全拼/简拼/混拼查词与未完/取消/原拼/AI/SP不造词分开断言。不在按键中读盘或重建基础大索引，组词中不偷偷换索引；已有显式排名优先，不把个人词总应首项写成泛化断言。
- schema升级需验证旧版本只读迁移不重写、显式关闭保留、复合事务坏批次原文件不变、总限制和自动词有界淘汰、clear推进generation防队列复活。关闭记忆仅停新增，已存自动词仍可查；清空删除自动词/排名/计数但保留手工与专业词。设置/导出/CLI状态等消费者需同步语义，旧版本降级不兼容须随包说明。
- 混拼普通候选排名回归应区分普通lane与后来插入的可选拼写建议；定位失败先检查实际provenance及raw消费，不把拼写建议当部分普通候选，也不为了测试扩张既有部分简拼规则。未变模型质量证据可按范围复用，新增索引及提交链证据独立记录。

0.4.18普通学习的未声明范围兼容以 `design/composed-spelling-and-learning-compatibility.md` 为准，覆盖下方历史“只扩AI、不扩普通学习”。用户保留每次相邻前移一步；兼容复用既有 `ai_undeclared_scope_compatibility`（默认true、显式false保留），独立于AI总开关，仅S04/S08通过原窗口/焦点/身份/style/密码掩码检查，不扩异常接口或明确敏感。须用真实自定义HWND和生产TSF提交、下一同raw、跨context、worker drain/隔离Store重载验证，仍不等于实际微信现场。其他宿主正常而某宿主不学时先查门禁，勿把保存或步进算法当作已证根因，也不通过按键磁盘扫描刷新解决。

当前0.4.15未声明范围默认兼容以 `design/default-ai-scope-compatibility.md` 为准；用户已明确选择默认开启AI及兼容S04/S08，可关闭兼容恢复严格路径。只允许既定窗口/focus身份及style检查通过的缺声明场景，不扩普通学习，不将Sensitive/Unsupported/损坏接口错误转为安全。下文历史“未知一律拒绝”的AI表述不覆盖此新合同；自定义敏感框未声明且无标准密码标记可能无法识别，须随设置及README告知。发送与发布两次只读检查、原生password mask和自定义控件不发消息的边界仍要分别证明。

先读 `docs/distribution.md`、`docs/windows-validation.md` 和当前 `design/mixed-input-and-installation.md`，分页与调序见 `design/adaptive-candidates.md`，旧版补充见 `design/input-and-installation.md`。历史设计文件若已不存在，不按旧摘要恢复；以当前源码和实际宿主事实为准，不将历史通过记录当成本次版本通过。

## 环境与构建

- 构包前冻结真正的随包说明输入：当前`package.py`将`docs/distribution.md`打包为README，不是根README；补版本、在线外发/费用和测试边界后再启动组包，最终逐字节比较，不能让编译期间追加的文档漏入包。已生成的旧快照不覆盖，另目录重构并只交付已验新版。
- Windows会话目录很深时，为Python夹具选择本Thread scratch下短临时目录名；FileNotFound恰在260字符处先核路径长度与实际源码，不盲改断言或移到未知系统临时目录。只重验受影响失败单例，复用其它有效套件结果。
- 基础Rust PE的版本身份按实际RC与manifest/SHA核对，不假定一定有FileVersion资源；安装器数值版本单独检查。资源探针错误地要求不存在的可选资源不是功能回归，也不能把空元数据伪写为当前版本。

- Windows MSVC 目标需要 Rust、MSVC 工具和 Windows SDK，不需要完整 Visual Studio IDE。先检查现有工具及 `vswhere`，不要重复安装整套工作负载。
- 安装系统依赖须有用户授权；UAC/安装取消不是成功，核对实际文件及注册状态，不盲目重复提权。
- PowerShell 中每次 exec 独立；需要时为该命令补 Cargo PATH，并使用已确认的 Python 路径，避免 WindowsApps 别名。
- 运行适用范围的 Rust 检查和 Python 组包测试。`cargo check` 不能代替原生链接和测试，合成 PE 测试不能作为可运行交付。
- 实际组包使用 `scripts/package.py`，保留仓库正常 Cargo 输出入口。新输出目录使用本 Thread 最新 artifacts 事实，避免覆盖已有 ZIP。

## 包与安装

- Git词库改用`license-file`时，先核对真实Cargo source/版本与完整通知；仅对已审定固定身份、路径和正文摘要做窄支持，manifest保留真实空SPDX及NOASSERTION/本地测试状态，不伪造MIT或泛化接受任意文件。扩充词库须同步构建器与安装器的精确资产白名单、可信大小/SHA和版本化必需记录，旧包携带新资产时也须完整验证；不能信包内自签摘要。载荷上限依据实际展开量有界调整，AI子载荷上限独立保留，勿为新大表向所有历史小fixture复制巨量数据。

- 校验外层 `.zip.sha256`、单顶层结构、包内 `SHA256SUMS`、manifest、三个真实 PE/导出、许可和两个 EXE 的 `--help`。
- 图标由 `build.rs` 调用 Windows SDK `rc.exe` 嵌入。运行 `scripts/verify_windows_icons.ps1 -BinaryRoot <解压目录>`，以资源方式加载三个 PE，对照 `assets/flower.ico` 检查组图标和各尺寸图像；不得执行 DLL 导出来验证资源。
- 品牌名称更新保持 CLSID/profile GUID 不变。旧名称安装应使用旧稳定目录的注册工具先注销，不通过放宽所有权检查或直接覆盖 registry 来改名。
- 机器 `register`/`unregister` 要管理员；普通用户 `enable`/`disable` 只作用于目标当前用户。不要更改默认输入法、重写语言列表或清理其他输入法。
- 从稳定安装目录注册，不从 session scratch、下载目录或 target 注册。冲突时停止，不覆盖未知安装。
- 启用成功不证明已切换焦点；COM 实例创建和 QI 成功不证明 `Activate`、按键接收器或编辑会话成功。
- `Get-WinUserLanguageList` 在 Windows PowerShell 中可返回集合对象；用 `foreach ($lang in (Get-WinUserLanguageList))` 访问成员，避免误把管道投影的 null 当作缺少语言配置。

## EXE 安装器

- 瘦身先统计压缩后的文件贡献，再用同一完整payload隔离比较；展开文件或PE变小不代表最终solid安装器变小。保留模型、词库与许可的逐文件摘要，区分编译耗时、解压字典估计、实测安装和运行性能；没有实际收益时不发布名义上的瘦身包。
- Inno压缩等级的名字不等于所有参数变化；核对实际字典/FastBytes/MatchFinder/Algorithm及安装解压内存成本。Rust profile实验须记录实际rustc参数，不能仅凭环境变量声称cdylib和EXE都启用LTO；固定负载交错比较，进程首次初始化不冒称磁盘冷启动。
- 定制ORT精简需区分非minimal reduced-operator与minimal模型格式限制，原图op列表不能覆盖运行时融合内核。未核验C API、固定模型、图优化与CPU路径前不替换官方固定运行库，也不把未测体积作为可交付收益。

- `scripts/installer.py` 使用 Inno Setup 6 包装已校验的 ZIP，输出 EXE、外部校验文件和不含本机路径的构建清单。可用 `--iscc` 指定编译器；安装目录可能由 Windows 已知文件夹重定向，优先实际卸载记录，不猜工具路径。
- 新增安装器后运行 `scripts/test_installer.py` 的边界测试，并真实编译。编译后只读检查版本资源、图标、签名状态和交付哈希，不把编译成功当成实际安装或卸载通过。
- 当前测试安装器保守拒绝已有注册，不做自动升级。提升进程只做机器注册；用户启用/停用由普通权限开始菜单入口执行。不要把管理员账户当作目标用户。
- 注册失败需保留可能被 COM/TSF 引用的文件并返回失败；卸载需校验文件、路径所有权和占用，失败保留文件。干净虚拟机上的成功、失败和占用验收是独立门槛。
- 新制品校验应保留原 ZIP 的哈希关联与 Inno Setup 引擎许可，不覆写已经交付的旧 ZIP，也不为测试安装器自动改变当前桌面输入法。

## 桌面排障

- 必须记录用户实际宿主和具体现象：英文直通、吞键、已有预编辑但不能提交、卡住/崩溃，不能仅凭“无法输入”推断故障位置。
- 输入法不显示时先只读检查机器 profile、HKCU Enable、排序和用户语言列表；登录会话刷新是用户操作，不自动注销或重启。
- 对照记事本与 Chromium/Electron 宿主，并确认对应进程实际加载的 DLL 路径；不向用户现有文档自动发送按键。
- 切换 DLL 路径前停用目标用户、释放旧宿主并注销旧路径；窗口关闭不等于进程退出。只读检查已加载模块，不强杀用户程序或 Explorer。
- 默认关闭的调试构建可以在构建时显式设置 `FLOWER_TSF_DIAGNOSTIC_FILE` 绝对路径。release 不写日志。仅记录固定阶段、pid、HRESULT，不记录输入或文档内容。
- 独立 TSF 探针必须区分 host client、service client 和已存在接收器。客户端标识无效或重复 advise 的错误不等于真实宿主故障，不能据此改成异步或强行吞键。

## 隔离原生回归与目录安全

- 纯修饰快捷键须验证生产回调链最终创建真实HWND，不以手势状态机或composition拒绝提示计数代替成功唤窗。漏收release后失焦/再进入必须清完整held/releasing且旧任务不能复活；新完整组合可恢复，OnTest仍只预测。私密/空选区可能需要一次只读编辑会话检查scope/selection，正文读取0与会话数0是不同事实，分别计数。
- 候选配额改变时同时验证合法AI显示同文升级：普通完整项被去重移除后仍保留待选正文的长短分类，不能依赖该项仍存在而跳过单字配额，也不能按confirmed显示长度误删短正文歧义。调整页大小后按候选身份恢复高亮；前缀选择测试用Space，Enter对未耗尽raw有原拼提交语义。旧默认页大小迁移应带一次性标记，新保存的显式自定义值不反复强改。
- 新合同已移除候选框下方AI状态行，诊断状态变化不应再引发视觉几何/重绘或命中帧失效；齿轮依据最终客户区而非首行/状态行垂直居中，滚动不移动。设置新增控件后须亲看整窗：只扩大白卡可能使卡片与footer重叠，静态控件可达不证明卡片下圆角/间距正确。

- TSF context身份应查询规范IUnknown；同一ITfContext接口的物理地址不是通用COM身份。QI失败拒绝受影响操作，保留focus/epoch/generation及对象强引用，不能退回零key或去掉焦点门禁。回归同时覆盖同identity不同接口、不同对象和失败查询；OnTest仍只预测。
- 原生预编辑caret用UTF-16计量，必须在同一编辑cookie中验证字符边界和实际移动量；不能按UTF-8字节设置含汉字前缀的选择，也不能落在代理对中间。分别验证begin/replace、内部插入/两种删除、预测不改真实选择和commit/cancel末尾语义。
- 只读ACP scope夹具不能证明Chromium类宿主可写。至少用可写ITextStoreACP串生产按键、preedit和提交读回，独立覆盖锁及选择拒绝；宿主先写后拒选择不等于可回滚。UIless需Ex/UIElement完整协商，不能只登记能力GUID或把隔离直调回调当真实Activate/Advise与具体软件验收。

- 可在 STA 中创建真实 ThreadMgr、DocumentMgr 和无外部 text store 的 Context，直接调用服务按键回调，再通过真实编辑会话读回新建文档。测试不得全局注入按键或触碰用户文档。绕过 Activate/Advise 的测试只证明编辑路径，不证明真实宿主激活。
- StartComposition 必须传入生产使用的非空 composition sink；验证预编辑、提交后光标顺序、Escape 和终止后的干净恢复。模拟宿主强制终止用 ITfContextOwnerCompositionServices::TerminateComposition；发起方自己的 EndComposition 不保证回调同一个终止 sink。
- 自定义机器安装目录不能仅开放向导路径页。用 check-install-path 只读检查本地固定盘、重解析点、owner/DACL、祖先替换权限及实际 payload 树；不要通过增加 users-readexec ACE 假装移除了继承写权限，也不要擅自改 D 盘父目录 ACL。
- 安装前后及卸载前的 helper 必须来自核对哈希的私有临时副本；在执行安装树内 helper 前完成 ACL 检查。原生 SDDL 内存描述符可验证 allow/deny、inherit-only、null DACL 和未知 ACE，不需修改系统 ACL。SDDL 的 DC 是目录服务权限，不是文件系统 FILE_DELETE_CHILD；后者应使用 0x40。

## 新建自定义安装目录

- 当前目录策略契约见 `design/input-and-installation.md`。不要把缺失目标的最近已有祖先当作安装根严格检查；允许无关兄弟创建/属性写入，但拒绝普通目录删除、子项删除及改 DACL/owner。已验证的本地固定卷根不可像普通目录删除，非严格祖先检查仅可忽略根本身 DELETE，不忽略 FILE_DELETE_CHILD 或权限接管；不把此例外应用到子目录。
- `check-install-path` 始终只读；`prepare-install-path` 是显式管理员写操作。后者仅对缺失组件用创建时的受保护 SDDL，新建即可信 owner/管理员SYSTEM全控/Users RX，保留禁止删除共享句柄后复查；不先创建可写目录再收紧，不修复已有对象，不自动改 D 盘根或父目录 ACL。同名竞争失败，不递归清理未知内容。
- 安装器必须在旧注册/目录冲突及运行库门禁后、复制前 prepare/check。只读新路径检查通过不代表管理员创建/向导安装成功；完整许可和源ZIP核验也不等于运行期安装验收。新制品仍不得自动运行。

## 0.4.4 紧凑候选和 Shift 手势

- 当前契约 `design/compact-candidates-and-shift.md`：移除整个页脚及幽灵点击区，不移除引擎分页/键盘翻页。候选高亮使用当前选入字体的真实 GDI 字宽，与实际省略文本共用结果；0.4.5 在词尾加6逻辑像素留白、4逻辑像素圆角，DPI缩放并夹在本 cell 内，整 cell 点击不变。0.4.10改用8×8子采样圆角覆盖率与当次背景色预混合、32位顶向下DIB单次绘制，保持半径/留白/点击；COLORREF的BBGGRR须转换为DIB的RRGGBB，限制缓冲分配并处理绘制失败。离屏像素应检查多级混合色、四角对称、明暗/彩色背景通道、极小尺寸、角外留背景、内部填色、相邻项未被涂色，测量失败/零宽不能因padding画出空背景。纯估算不能代替选字体后的度量，离屏测试不等于实际宿主外观验收。
- 单独 Shift 在真实 down/up 识别；OnTest 纯预测。英文 pending 手势时可预测 TRUE 以接收组合键真实 OnKeyDown，再取消手势并返回 FALSE直通。检查线程键盘快照中的预按普通键/双 Shift/控制键，查询失败拒绝，不只看 Shift 后收到的字母。repeat 不重新建立或刷新手势身份。
- ThreadMgr 文档焦点与 context push/pop 独立 generation 能防切走再回误切；订阅 sink 不持服务，保留线程/卸载计数，guard解除。generation只在新手势建立时保存，不能被repeat刷新。真实 Push/Pop 后插入repeat再release是必要回归；仅比较当前context会漏掉往返。
- 原拼音转换恢复 confirmed.raw+remaining raw，不学习；同步编辑成功且token/epoch/context/generation有效才发布英文模式。无效client、写前stale校验、失焦及纯试探需单独证明，不承诺TSF部分写后失败能回滚宿主文档。
- 安装完成默认启动现在只普通用户 enable 后退出，不再打开设置，取代下方0.4.3启动GUI约定。无参设置入口保持；包核验禁止运行有效启动命令。

## 跨CPU纠错证据

- 先核对实际报告的helper/model/runtime摘要和请求metadata；同字节模型在不同CPU上可能给出不同概率，执行机通过不覆盖目标机。记录候选、source_p、masked概率、margin、gain和调用/耗时后再定修复，不凭None或短耗时猜哪个门禁拒绝，也不把CPU量化饱和当已证事实。
- 对照程序须在同一目标机器执行旧规则和修复候选，正确句与错句同时断言；错误的假设即使在开发机控制组通过也应按目标机日志否决。缺EXE/摘要检查失败属于模型未启动，不当作模型结果；先确认完整解压，安全隔离时保持防护，不改扩展名/关闭防护绕过。
- 0.4.16拼音联合证据以 `design/cpu-robust-spelling-evidence.md` 为准：保留原字不确定度与原CSC，目标最弱概率在[0.90,0.95)时需更强gain≥2；不能把它误写为取消source门禁或把全部阈值降为0.90。数值单测覆盖目标机实际概率不等于新包已在目标CPU运行。

## 可选离线AI与分发验证

- 契约见 `design/offline-ai-correction.md`。0.4.7起默认开启（显式关闭配置保留）、停顿后追加 `✓ AI` 建议、显式选择才提交；原候选立即可用，AI不学习、不改已提交正文、不读取周边宿主文本。发出与发布前分别在真实只读cookie内用 `GetAppProperty` 检查安全scope，未知/敏感拒绝，不能用模型probe替代这些门禁。0.4.10的AI明确文本集合为0/50/52/53/54/55/57/58/68，学习仍仅0/57/58；拒绝原因固定S01–S11。Chromium形态须用真实ITextStoreACP属性host补验，owner测试不能代替ACP桥接；官方示例使用GetAppProperty，不以GetProperty或VT_EMPTY兜底。GetInputScopes即使失败/超限也释放已返回的COM分配指针，失败/超限不构造slice。
- 把推理纯逻辑、固定真实模型、生产IPC、安全TSF接线和真实桌面分开记录。显式ignored真模型单例必须实际命中；假transport只证明接线。冷加载、热推理、IPC整体与250ms去抖分别计时，测峰值工作集，不只报模型磁盘体积。0.4.8将生产IPC与TSF共同有界等待改为5秒以容纳冷加载，不能只延长测试探针或只改传输层；超时/晚到仍须完整拒绝旧票据，待只读cookie也需有界并释放源状态。
- 候选窗固定AI状态行用于区分关闭、组件不可用、输入未达条件、范围受限、检查中、无建议、已建议和超时/编辑会话失败；不含输入、不持久化、不参与候选编号/点击/提交。先读真实状态而不是把无对号都归因模型；浏览器/Electron安全范围未知仍拒绝，不为正例将空VARIANT当默认scope。设置标题版本不证明宿主已换DLL；状态行缺失需核实实际加载版本，不能自动改注册或强杀用户进程。
- 用真实WM_TIMER消息泵驱动隔离TSF异步读会话，分别证明私密范围0请求、None响应状态、超过旧1秒但在新预算内可发布；手动调用idle不能代替timer投递证据。真实AiClient+管道人工1.2秒响应可验证期限与取消join，但不冒充真实模型或浏览器验收。
- 固定模型及ORT必须对照源码内嵌可信manifest，不接受用户bundle自签清单；核对九文件完整大小/摘要、词表特殊ID和许可来源。原生DLL显式绝对路径加载与限制依赖搜索不替代生产安装目录及祖先ACL保护。TSF DLL不引用ORT推理模块；额外运行库仅在AI私有白名单中。
- 源快照、request id、期限、epoch、revision、焦点/context generation都要覆盖；无回包也必须主动失效并清理源文本。真实TSF `SetFocus(None)` 成功不保证平台查询立即无top context，fixture要断言实际状态；保留原动作和预期，不以降低断言掩盖环境差异。后台shutdown需取消完成并join后才释放DLL计数。
- 模型质量用预先固定的正确句和错句分组，保存raw输出与过滤结果，分别报误改、漏报、部分纠正和完整金标命中；不要为演示降低阈值或把小样本当泛化准确率。若整句被一处低置信拟替换连带否决，先以同批真实logits比较逐字保留原字与整句否决，再决定策略；0.4.11保持0.98/4并逐字保留，降至0.95/3出现他/她误改反例。模型直输正例和builtin拼音→实际首候选→AI插入须分别证明，忽略测试必须实际命中。转换仓库无独立许可、训练数据未审计等限制必须随最终包的共同README携带，源码旁说明不足以证明随包告知。
- 0.4.7起只构建内置AI的标准文件名，CLI和实际构建API都必须显式提供资产，不能回退base；内部variant=ai、AI可用记录、九文件固定摘要和安装器source ZIP关联仍须完整验证。旧0.4.6两变体的只读兼容不等于允许新构建生成普通包，历史制品不可覆盖。新依赖许可证若不识别，按实际完整正文增加窄识别与失败用例，不能凭SPDX/文件名绕过；ISC须包括授权、通知保留、无担保及责任排除，原始字节仍随包。
- AI正向验收须从真实内置词库的 `ai_input()` 源文本经固定模型，明确assert非None金标、注入、选择及不学习；允许所有输入None的探针不证明纠错可见。模型纠正文已有普通候选时应按契约复用并标记来源，不能去重后静默丢弃；遍历全部页检查无重复，标记仅在显示路径。普通词条fixture和单独模型probe不能替代这项信号。
- 中文标点需证明无预编辑时的真实TSF插入，而不仅是引擎产生commit。无composition不可访问composition范围；用当前写cookie的InsertTextAtSelection及强类型flags直接插入并移光标。覆盖组词确认再追加、尾部raw保留、引号OnTest预测不改变方向、快捷键/英文直通和原翻页键；不注入全局键或读取用户文档。
- 集成探针从已核验AI包启动自有broker，仅经命名管道发送固定测试句，finally停止并wait自己持有的child，不能杀用户进程。该显式probe可在自有验证目录运行，但不证明生产受保护目录门禁或真实宿主体验。两变体只读检查通过仍不等于签名发行、真实安装/卸载或桌面端到端通过。

## 0.4.3 固定安装、启动与真实学习链路（启动GUI为历史）

- 最新用户决定取消自选目录，EXE 固定 `{commonpf64}\Flower`、隐藏目录页、拒绝 `/DIR` 异根。下文自选目录章节仅为历史/helper能力，不是当前安装器承诺。不修改已有父目录权限或 owner，不把普通应用的 HKCU 安装方案当成已证实的 per-user TSF 注册。
- 完成页默认启动用 `postinstall nowait skipifsilent runasoriginaluser`，不加 `unchecked`；先检查安装/注册成功、安全树和设置程序哈希。启动入口严格解析，提升 token 或查询失败均拒绝；普通用户 enable 后打开设置，不改默认输入法、不声称焦点已切换。nowait GUI 错误必须有固定 MessageBox，不能只有 stderr。包验收不能运行该入口，因为会修改用户启用状态。
- `GUID_PROP_INPUTSCOPE` 应用声明读取使用同一编辑会话的 `GetAppProperty`；`GetProperty` 的文档属性不等价。未知、空、失败及敏感仍拒绝学习，不能为修复正向通路放宽门禁。
- 原生 ITfContextOwner 测试除 CreateContext 外，还须对 context 的 ITfSource 显式 AdviseSink，并在析构解除。用生产按键→成功提交→安全发布→Activation 排名→worker drain→隔离 Store 重载串起前移证据；仅 Store fixture 或 adaptive=false 测试不证明学习生产链。测试目录必须显式隔离，不用真实用户数据。

## 0.4.2 安装与严格单行（历史安装策略）

- 用户明确选择全部载荷放所选目录并保留安全限制；安装器恢复真实 `{app}` 根、版本载荷、注册/快捷方式/卸载器均同根，不再固定 Program Files 第二份，不调用用户副本部署。已有不安全祖先仍拒绝，不将深度当原因，也不改其 owner/ACL。Windows 注册表、快捷方式和用户配置不属于可搬迁程序载荷。
- 严格单行必须与 engine 真分页一起改。session page_ranges 是所有数字、鼠标、Enter、页码真源，不能 popup 少画尾项。共享 engine::paging::candidate_cell_width，560逻辑像素总宽、默认544内容宽，最多min(设置上限,9)；超长项独页省略但提交不截断。预算变化保留绝对高亮并发布新revision，旧票据失效。
- 跨屏不能在发布前拿旧 popup DPI 换算目标工作区。同屏可读取实时DPI；跨屏用无owner/parent、不显示、不激活的内置STATIC隐藏探针，先核对目标monitor与awareness context相同再GetDpiForWindow，所有路径销毁；失败默认预算并由单行布局缩窄。不移动已有popup、不改宿主或线程awareness，不在OnTestKey预演创建资源。
- GetDpiForMonitor 有调用方 awareness 限制，GetScaleFactorForMonitor 也不能未经核实当作精确窗口DPI。不要只替换API名称就宣称修复；纯96/192预算测试应接入真实生产查询链，真实多屏仍另验收。

## 0.4.1 分组件安装与候选窗（历史）

- 深层用户目录与机器 DLL 分离：系统载荷、公共入口及卸载器固定 commonpf64 Flower；自选目录仅固定用户文件副本。不得把关闭祖先 ACL 门禁或改已有父 owner 当作深路径支持。向导明确两种位置，ExecAsOriginalUser 从已核对安全载荷运行 helper，helper 查询 token 失败/提升均拒绝，不回退提升复制。
- install_user_files 的非提升检查、严格 source 检查、目标 guard 不能被 private copy helper 抽取绕过。固定文件 create_new、独占句柄、sync，失败仅通过本次持有句柄标记删除。源/目标 guard 覆盖复制及回滚，卸载不递归清理可替换用户树。
- `isolated_user_copy_` 五项测试默认忽略，必须使用本 Thread 的 `FLOWER_INSTALL_PATH_TEST_SCRATCH` 显式运行 `cargo test --lib isolated_user_copy_ -- --ignored`。只操作独占新建 fixture；提升进程在 I/O 前应明确失败，不以空 return 假通过。假 EXE 只当数据，不执行，不需要真实安装。
- GetTextExt 无布局、clipped 和无效 RECT 是不同情况；有效 caret 可零宽但须正高度。编辑会话内优先文字位置，其次关联 view HWND 的 GUI caret，最后可用有效 viewport 边缘作粗锚，不能称精确 caret。焦点/token/revision 门禁不放宽；popup 普通 Z 序提升且 NOACTIVATE，非 TOPMOST。
- 横向候选布局与绘制/命中同源，不画拼音头部或 comment；页大小可达99，不能沿用32项截断。极小区域整页退化无鼠标区域，保留键盘语义。截图/隔离几何测试均不替代 Edge 实机证据。

## 长候选与原生兼容增量

- 0.4.11长候选按实际PaintFont/GDI字宽在Unicode标量边界换行，窗口内部滚动不改引擎页；必须验证末字实际像素、AI显示标记、滚动命中、状态行隔离和可见刷新工作区限界。SaveDC/IntersectClipRect失败须停止候选绘制并恢复DC；鼠标命中只由成功呈现且快照/DPI/状态/滚动/客户区/窗口几何仍匹配的帧授权，不能重建不可见候选选择区。故障注入与真实HWND生产state验证可覆盖这一分支，无需重跑无关词库/持久化套件。
- 原生AI兼容边界以design/long-candidates-and-ai-compatibility.md为准，需遵守用户已明确授权的隐私取舍；不能把同进程类名或非密码style单独当安全声明。当前实现限定四类声明缺失/失败并组合焦点/context view、PID/TID、原生类、style和类过程密码字符检查；明确敏感仍拒，本地学习不回退。SendMessageTimeout在同消息队列会忽略超时，跨线程超时夹具不能证明同线程生产有界；GetClassLongPtrW/CallWindowProcW只读类过程检查须验证绕过应用子类拦截后仍读取真实掩码，不宣称可抢占任意宿主/系统代码。

## 拼音修复与通用选词记忆

- 短词和额外音节的词典建议以 `design/broad-pinyin-correction.md` 为准，显示“✓ 拼写”，不可将词库召回冒充真实模型纠正。测试先验证真实 raw 消费：同字部分候选留下尾部拼音不算完整命中；同字普通完整候选与词典建议的去重规则不同于已通过模型的 AI 升级。显式选择须完整提交、确认前缀一次、无学习及个人前移，并补退格恢复再选择的交叉路径。
- 广测试集在执行前固定金标，逐项保存进度并单列输入长度拒绝、无完整来源、词库未召回、模型拒绝/词表不支持及非金标建议；正确控制的原解码差异不能混算为模型误改。控制项出现额外可选拼写提议也须报告，不能只报无 AI 响应。原生 fixture 若关闭 learning，只证明提交及排序；不学习事件由引擎断言承担，不能外推持久化无写入。

- 拼音误输验证须分开记录原始raw、真实builtin首候选/规范reading、生成器召回、模型放行、协议proof和最终提交；生成金标在候选中不等于AI已展示。用替换、交换、漏字、多字以及正确自然句对照，保留未达门槛的边界，不为演示降低原CSC阈值。新拼音路径可用独立组合证据，但发布端仍须校验编辑距离、局部音节/字数和未变前后缀，不能借修拼音顺便改其他同音字。
- 原始拼音与reading的IPC扩展保持有界、身份及期限，旧空metadata路径要有兼容测试；真实包内AiClient/自有broker应包含新拼音正例、正确对照和旧CSC，退出只停止自己持有的进程。真实WM_TIMER只读session+fakeResponse证明接线，不能冒充真实模型或用户宿主端到端。隔离原生组合应串起真实按键、实际请求、生产管道/模型及最终提交，并区分快打、前缀响应完成后继续和前缀仍在途继续；旧None结果不得覆盖新完整句。即使该组合通过，用户已确认版本且重开宿主后仍失败，也不能推定旧DLL或宣称修复。可用固定公开句诊断脚本在实机记录后台响应/阶段/耗时、模块路径与磁盘摘要；不读取正文/词典，不把固定metadata等同TSF实际metadata，磁盘hash不等于已加载映像身份，复用后台不终止它，自有进程清理须核实退出。
- 排名故障先判断事件生成、编辑成功、安全门禁、focus/context/claim、激活内同步和worker保存哪个环节断开。数字、候选动作、方向Enter/Space共用成功提交链；同context、跨context、Store重载分开验证。使用真实builtin歧义词且查身份/consumed，不给示例词硬加权；相邻前移与频率默认排序是不同层，AI建议及失败/取消/敏感输入不学习。0.4.12普通学习的类型/native兼容边界以design/pinyin-repair-and-ranking.md为准，历史“仅AI兼容/学习0、57、58”不可当当前规则。
- 默认排名的人工使用prior必须有界且绑定规范词对，不伪称统计词频；保留简拼/混拼既有正例，不能靠扩大beam或预算掩盖退化。文字总频率不等于读音义项频率，多音词有合法次义时应保留来源，不能把罕用读音误报为错字或凭主观改源权重。

## 用户词典发布前进程终止回归

- 可用隔离 `DictionaryStore::new` 指向本 Thread scratch 下词典，父进程持有目标的无 delete-sharing 只读句柄，让真实生产写入停留在原子发布前；不得使用真实用户词典或填满工作磁盘。
- 观察唯一临时文件，并以拒绝 WRITE sharing 的只读打开确认生产写句柄已关闭；结合生产路径 `write_all -> sync_all -> close -> publish` 确定检查点，不用固定 sleep 宣称已进入写入阶段。
- 用测试程序自身的精确 helper 启动子进程，只终止该测试拥有的 child 并 wait/reap；断言失败也须通过 guard 停止并回收子进程。读取 staged JSON 检查新 generation，再强制结束，验证旧目标逐字节不变、可重新加载与管理写入、orphan 不被误恢复。清理仅限本次测试目录。
- `tests/user_dictionary_process.rs` 的 `killed_writer_before_atomic_publish_preserves_old_bytes_and_allows_recovery` 是定向入口。此类证据只覆盖同步完成但尚未发布时的进程结束，不证明写入中途、真实磁盘满、系统崩溃或掉电耐久性；不要为扩大表述而自动执行危险注入。

## 运行期路径与原生设置视觉

- 设置黑框先分清PE控制台子系统与真正cmd进程；GUI入口可用GUI子系统，CLI仅附着已有父控制台且保留重定向stdout/stderr，不AllocConsole；AI/登录静默路径仍不附着。发布验GUI PE子系统及重定向help/错误退出码，不自动启动真实用户GUI作探针。
- 设置响应慢还要检查输入法在设置进程中的自激活：TSF Activate同步构建完整词表/混拼索引或启动离线broker，可使只打开数字设置控件也付出大量CPU/内存。输入资源可延到首次真实拼音字母，OnTest只预测；不因Tab/数字/在线快捷键加载词表。先分类常用词再一次性构建索引，勿构建后立即重建。延迟安装词表时保留各context的引号/标点状态，新增真实首次输入及overlay回归；明确首次输入仍有成本且共享缓存不因窗口关闭自动释放。设置自身词典管理读取可延到首入该分类，保持加载中禁写/错误不覆盖/cancel join。
- 资源比较须用明确自有进程/fixture、实际命中测试及同类阶段，分别记录耗时、工作集和私有字节。未注册TSF的advise参数失败即使发生在重初始化之后，也只能作为该阶段资源线索，不能声称完整激活成功或保证用户桌面内存。零测试、采样遗漏和完整生命周期失败均如实记录。
- 设置响应慢按点击回调、安全检查、spawn、首屏、词典加载分阶段取证；整树ACL检查是元数据成本，不等于哈希103MB模型。可把完整校验移到activation拥有的单worker，owner以非阻塞poll复核token/focus后启动，coalesce、取消/join及DLL存活守卫不可少；关闭AI也要处理完成事件。GUI初次词典读取可在首屏绘制后后台进行，加载/失败期间写控件与handler双重拒绝，保留坏文件，晚结果不作用于复用窗口。模拟延迟证明响应边界，不冒充真实用户耗时；join仍可能等待在途I/O。

- Windows PowerShell 5.1的 `Split-Path`/`Join-Path` 也不能直接消费COM注册里的 `\\?\C:\...`，可能报drive为空。诊断工具须在provider操作前仅适配精确的verbatim本地盘符表示；保留原注册值用于报告，IPC身份仍取文件句柄返回的canonical路径。验证自动注册定位与显式helper两条入口，不能只用普通路径的 `-Helper` 探针替代自动定位；请求列表为空的初始化错误不算后台模型已测。

- 注册canonical路径与GetModuleFileNameW可能保留 `\\?\C:\...`。AI初始化不能把它直接交给仅接受普通盘符的安装CLI语法检查；仅在运行期入口精确适配verbatim本地磁盘形式，再跑原祖先/整树/owner/DACL/reparse/hardlink检查。不要扩大CLI路径接受范围、盲目剥所有前缀或canonicalize后掩盖链接。纯规范化测试和可写fixture仍被拒绝的实际门禁测试分别记录，不把字符串转换当安全证明。
- AI初始化失败应保留固定阶段原因：模块路径、路径形式、目录安全、缺helper/资产、client或timer；不要持久记录输入或本机路径，也不要把helper存在称模型运行就绪。
- Win32设置可用原生卡片/多级字体/owner-draw主次按钮改善视觉，但不引入无真实行为的导航。DictionaryStore和SettingsStore须注入，预览及动作只用自有fixture，窗口截图来自自己HWND而非桌面；字典错误不重建覆盖。UI美化需保留立即保存/表单保存/清空确认的区别。
- BS_OWNERDRAW不能与BS_DEFPUSHBUTTON互斥位硬或。生产键盘循环若先IsDialogMessageW，Enter可能抢先发IDOK，把聚焦关闭/危险动作变成保存；必须验证实际路由优先级、展开下拉Enter/Esc、Tab/ShiftTab/Space，不以静态布局或没有消息循环的截图fixture证明键盘正确。
- visualstyles/PerMonitorV2/asInvoker的应用manifest仅附到flower-settings.exe，不附TSF DLL或改变宿主DPI；按需在GUI启动前InitCommonControlsEx。资源只读映射验证settings包含而其他PE没有这些声明，图标仍单独核验。测试进程截图不等于最终EXE的v6主题或实际多屏验收。
- 快捷方式显示名变更不等于改EXE文件名、参数或权限。启动/停用仍普通用户enable/disable；只改新安装器声明，不自动删除真实用户旧快捷方式或更改注册。

## 登录自启动与UIElement增量

- 以 `design/startup-uiless-and-mixed-composition.md` 核对当前合同。登录项命令应绑定实际current_exe及完整受保护安装，不接受任意调用方路径；Run值的类型、UTF-16内容和唯一终止符均纳入所有权，未知同名值拒覆盖/删除。登录模式连非法参数组合也静默；包核验只运行无副作用负例，纯store成功不冒充真实登录。
- Inno的ExecAsOriginalUser不支持卸载期，UninstallRun也不支持runasoriginaluser；提升卸载的HKCU不是原用户。按既定每用户先关闭/停用流程记录残留边界，不枚举其他hive或误删管理员账户项。
- UIElement协商须覆盖Begin的pbShow和后续Show，拒显同时约束候选、AI状态行、caret刷新和旧鼠标帧；保留隐藏HWND供原timer，不将拒显等价为禁用AI。生命周期COM调用放在RefCell可变借用之外，测试Begin期间停用/重激活及旧元素失效。
- 新能力类别作为独立注册资源验证冲突、补装、写后失败回滚、卸载逆序和installer孤立项检测；keyboard-only旧安装不得因补能力失败被清掉旧资源。只登记真实实现的能力，不用协议隔离通过外推微信/WPS现场。
- 解码排序改变后，先以真实builtin→请求→模型核对原生测试固定source是否过期；若只更新测试预期，保留非None金标、只读发布、显式选择提交及不学习断言，仅重验受影响场景，不改生产迎合旧文本，也无需为cfg(test)变化重打未变运行包。

## 交接

记录源码版本/差异、实际命令、产物哈希、安装绑定、当前用户启用状态、桌面观察及未覆盖项。未签名包或失败的桌面验收不得标为可发布。旧归档保持不可变，并说明其是否包含后续源码修改。日志和探针放当前会话目录，不把会话绝对路径或用户输入沉淀到长期文档/技能。
