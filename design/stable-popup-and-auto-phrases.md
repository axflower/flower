# 可见候选稳定刷新与完整逐段选词记忆

## 目标与已确认事实

用户报告候选显示框随音节变化闪烁，以及词库外“新加卷”通过逐字选择提交后，
下次输入`xinjiajuan`仍无完整词候选。0.4.18中可见popup.update会show_at旧caret，
生产发布随后show_at新caret；show_at无条件hide，因此单次更新可能两轮隐藏/显示。
普通发布只兑现PromotionEvent，不建立新词；旧LearningEvent计数也不会创建可查词条。

沿用完全离线、非激活/非置顶、UIless、隐私和身份检查、普通候选每次前移一步，
AI/拼写建议不学习；不从已提交正文、聊天历史、窗口标题或未转换raw推断新词。

## 决策一：稳定刷新，不改变候选布局规则

优先修复已证的hide/show与重复布局，不引入未必要的固定窗口宽度或帧缓冲系统。
现有裁剪、绘制失败拒绝命中、DPI与屏幕工作区限制必须保持。

`CandidatePopupWindow::update_snapshot(snapshot)`新增只更新模型、清理旧待处理动作、
撤销旧呈现命中身份的入口，不显示、不隐藏、不布局。普通/AI发布在有效caret与
UIless/焦点等条件通过后，update_snapshot再show_at本次caret，各一次。
原`update(snapshot)`保留可见自动刷新语义供既有独立消费者；内部复用同一赋值逻辑，
不得忽略revision或保留过期presented帧来做视觉去重。

show_at对已可见且同显示器的普通刷新保持可见，按当前/目标DPI与工作区一次算出
最终布局；仅在位置/尺寸变化或首次显示时调用必要的SetWindowPos，保留NOACTIVATE。
同屏不先做BASE_DPI临时布局，不因音节/AI状态变化SW_HIDE。
跨屏可以保留现有隐藏测量/迁移流程，必须继续使用目标DPI而非猜源屏DPI；
失败、真正失焦/无候选/外部拒显时仍允许并必须隐藏。一次性全部跨屏无隐藏不是本轮目标。
整帧绘制仍填满客户区，WM_ERASEBKGND已被处理；可使用不请求额外擦除的失效，
不能仅改InvalidateRect参数而不修hide/show。相同AI状态继续去重。

验收用自有HWND连续快照/音节更新，记录该窗口真实可见性/geometry/paint消息，
同屏稳定更新无hide/show翻转、最多一次必要几何提交；新快照身份立即撤销旧命中，
成功绘制后才恢复新命中。覆盖布局改变、AI状态、真正隐藏、工作区限界与现有绘制失败测试。
不截图真实桌面或向用户窗口注入按键，不将该证据外推所有DWM/目标显示器视觉体验。

## 决策二：自动词条独立存储，保留旧计数和手工词典

不复用旧learned计数来创建词，避免旧记录迁移后意外改变候选。
UserDictionary schema升级到3，新增`auto_phrases: Vec<AutoPhrase>`，
`AutoPhrase { pinyin: String, text: String }`为带deny_unknown_fields的持久化记录。
字段缺失默认空；旧schema1/2非破坏读取为3，不在load时重写；未知版本/损坏文件拒绝。
旧entries/domains/learned/rankings与显式关闭值/generation保持。所有结构字面量相应更新。

自动词条限定2–32基本汉字、一字一完整规范拼音且每音节含元音，规范拼音去分隔符
不超过128字母，读音来自所选候选，不采用实际简拼raw或显示标记。
同规范读音/文字去重，最近更新移到末尾，上限4096，超出淘汰最旧自动词条；
原文件16MiB、合计100000记录上限包含新字段，非法批次整体拒绝，不部分写入。
自动词条使用固定16384的个人词库权重，不声称统计词频，不更改基础词库或手工权重。

新增共享纯函数`remember_auto_phrases(&mut Vec<AutoPhrase>, &[LearningEvent])`
验证完整批次后更新有界集合，供内存和Store复用；`auto_phrases_tsv(&[AutoPhrase])`
只序列化此小词表。UserDictionary公开`base_lexicon_tsv()`保持手工/启用专业词库快照，
`auto_phrase_tsv()`返回自动词条，`lexicon_tsv()`返回两者合并、同词取最大权重的可用快照。
不从旧learned构造条目。顶层导出采用可用快照；专业词库导出范围保持。

Store新增`commit_if_generation(generation, promotions, phrases)`，在原锁/重读/原子发布内
检查adaptive_ranking_enabled、generation及批次上限，分别应用原一步调序和自动词条，
同一事务保存。Worker新增`submit_commit(&[PromotionEvent], &[LearningEvent])`有界非阻塞命令，
两组数据合计不超过原MAX_BATCH。原计数、promote和worker接口保留兼容。
clear_learning清除旧计数、rankings及auto_phrases，推进generation防旧队列复活；
关闭本地记忆停止新增，不删除已保存自动词，清空才移除。AI开关不控制普通记忆。
保存失败保留原文件、报告现有固定错误，不回滚已成功的正文提交；内存可用不冒充耐久保存。

## 决策三：只从明确完成的逐段选择生成词条

PinyinSession新增独立`pending_phrase_learning()`缓冲，使用已有LearningEvent的pinyin/text，
不能混用旧逐边计数。一次组合中有至少两段普通候选显式选择，最终raw完全消耗并提交时，
将所有确认段文字及规范reading连接，符合上述2–32字且当前基础/自动词典无相同规范词对，
才产生一个新词事件。一次直接选择整句、部分未完成、literal raw、取消/退格后的未提交状态、
英文原拼提交、AI/拼写直接提交均不产生。逐段确认仍不立即保存，也不改变已有一步排序事件。
预测test_key仅在克隆产生状态，不能对真实事件或存储产生影响。已知词不重造。

基本汉字标点追加发生在同一完整提交后时，只记录已转换的词本身，不含标点；
有剩余raw的字面提交不记录已选前缀。数据只来自本次用户选词，不读宿主周边正文。

EngineRouter转发pending_phrase_learning。生产成功编辑发布提取新词与旧promotion，
沿同一个scope/兼容开关、焦点/token/epoch/revision/context generation和一次claim门禁，
受adaptive_ranking_enabled控制；将两种事件一并送submit_commit，AI/拼写无事件。

## 决策四：小型自动词索引，同激活立即可用

不在每个按键/新词提交重建大型基础词库索引，也不从按键回调读取磁盘。
Activation缓存auto_phrases与`Option<Arc<Lexicon>>`的小型自动词索引，
激活时从base_lexicon_tsv加载手工层，从auto_phrase_tsv单独构造此索引。
成功安全提交新词后，使用共享remember逻辑更新有界集合，仅集合变化时构建小索引一次。
持久化由worker异步完成。下一次新输入/空闲context同步新索引；正在组词不悄悄换词。

PinyinSession/EngineRouter新增`set_auto_phrase_lexicon(Option<Arc<Lexicon>>)`，仅空闲时替换。
刷新时从小索引生成完整exact/简拼/混拼候选，再合并原基础解码；同text/reading/consumed去重。
新自动词是普通候选，规范读音和真实raw消费保持；已有显式个人rankings仍在合并后应用。
本轮支持自动词的独立完整输入，不要求将新词进一步作为基础长句图中的任意内层边。
新词可下次一次选择，跨context及Store重载/新激活仍可查；跨进程即时磁盘同步不新增。

## 验证与边界

用实际builtin确认“新加卷”此前无exact，逐段选择并成功提交，下一同raw完整普通候选可达；
隔离worker drain/Store重载、新session/跨context同样可达，并验证规范读音/一次提交。
覆盖其他词组、部分词而非逐字、简拼转全读音、取消/未完raw/Shift/AI/拼写/敏感/失焦/关闭记忆，
以及已有词不重复、schema1/2迁移、清空generation、并发合并、上限和失败原文件不变。
复用仍有效的排序/模型证据，集中补新增接口消费者与原生提交，未变化模型不机械重跑239推理。
新测试版完整包与旧归档保护仍按原分发门禁；不自动安装或操作真实用户词典。
