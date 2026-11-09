# Flower 0.4.2：完整安装、单行分页与相邻调序

0.4.18普通学习的S04/S08自定义宿主兼容以
`composed-spelling-and-learning-compatibility.md` 为准；每次相邻前移一步保持。

0.4.12的普通文本类型与原生兼容学习门禁以 `pinyin-repair-and-ranking.md` 为准；
相邻交换、成功提交与持久化身份规则继续有效。

0.4.11的长候选显示按 `long-candidates-and-ai-compatibility.md` 自动换行及滚动；
本文件引擎页范围、候选身份和短候选预算仍有效，旧“不换行/省略长句”仅为历史显示规则。

本文件是 0.4.2 当前契约，替代 `input-and-installation.md` 中与之冲突的旧版本条款。
用户明确选择全部放所选目录且保留安全限制、不修改已有父权限；
选词排序本地保存，重启后保留。本轮仍仅递增补丁版本。

## 完整安装

EXE 恢复真实安装目录页，默认 Program Files，可选安全的本地固定盘路径。
DLL、两个程序、数据、许可、说明、版本载荷及卸载器全部在所选根下。
不保留 Program Files 的第二份系统载荷或用户目录副本，公共入口及注册指向所选载荷。
沿用创建时安全 ACL、可信已有祖先、禁止重解析与覆盖旧注册的门禁。
普通用户可替换的已有祖先仍拒绝；明确提示不是目录深度限制。
不改已有父 ACL/owner；选择此策略并不授权安装或修改开发机配置。
0.4.1 用户副本辅助 CLI 可保留兼容，但新安装器不调用它。
系统注册表、开始菜单、Windows 自身缓存与当前用户配置不属于可搬迁的程序载荷；
不承诺这些 Windows 集成数据也全部位于安装目录。

## 单行及真实分页

- 候选只一行，不换行、不画拼音；页脚可以单独一条状态区，但候选不得占第二行。
- 产品宽上限 560 个 96-DPI 逻辑像素，候选内容默认预算 544（扣两侧各8）。
  页大小设置保留为数量上限，实际自适应页最多9项以匹配数字键，长词减少数量。
  一项超长时独占一页，显示裁剪省略，但完整内容及提交身份不截断。
- 引擎引入可选宽预算，固定页模式保留供既有公开 API/fixture 使用；
  Windows 激活默认使用单行预算。计量纯整数、无平台依赖：
  编号与间隔按 ASCII9像素、非ASCII18像素、左右 padding，共用单项估算函数，
  单项宽度上限240逻辑像素；session分组和popup布局必须共用同一估算。
- session维护连续无遗漏 `page_ranges`，页数、当前切片、高亮、Enter/数字/鼠标选择
  均使用范围，不再用 `page*page_size`。测试按键克隆同预算和顺序。
- 平台在发布候选前按当前监视器可用宽和DPI收窄预算，改变预算须重新分页并更新模型
  revision；可保持原绝对高亮候选的所在页，不可让旧鼠标票据仍有效。
- popup只在单行放置全部已分页项；若监视器/DPI在发布后变化，可缩窄单元格并省略，
  不换行、不隐藏后半页。极端小到每项至少1物理像素也放不下时整体隐藏鼠标区域，
  下一次发布重算预算；键盘仍对应原真实页，不修改宿主文档。
- 目标屏与现有popup同屏时可读取其实时DPI；跨屏不能把旧popup DPI当作目标值。
  可在目标屏短暂创建不显示、不激活的独立内置STATIC探针窗，核对其显示器与
  DPI awareness context均匹配，再读取GetDpiForWindow并销毁。不得移动现有popup
  或改变宿主/线程DPI awareness来探测；失败使用默认预算并由单行布局收窄。
  探针仅在成功编辑后的发布路径使用，不进入OnTestKey的只读预演。

## 相邻调序数据接口

引擎纯数据（不依赖Windows或I/O）：

```rust
pub struct RankingCandidate {
    pub pinyin: String, // canonical，显示身份，不是任意输入文本
    pub text: String,
    pub consumed: usize,
}
pub struct RankingOrder {
    pub input: String, // 当前规范化ASCII原始拼音/简拼，作用域不混淆
    pub candidates: Vec<RankingCandidate>,
}
pub struct PromotionEvent {
    pub order: RankingOrder, // 选择时实际已排序的完整候选列表
    pub selected: RankingCandidate,
}
```

三者为 Clone/Debug/PartialEq/Eq、serde Serialize/Deserialize 数据，未知字段拒绝。
保存的输入作用域区分简拼和全拼；`yingshe` 的提升不强行改变另一缩写的排序。
只有完整候选（consumed等于input长度）生成PromotionEvent，部分选择本身不记录；
组合完整候选作为一个身份，不把组成词的旧LearningEvent错误当作一次选择。

调序算法：解码基础列表生成后应用该输入的显式顺序，未知/已不存在身份忽略，
新基础候选保持稳定相对顺序追加；不修改词权重或beam，不生成不存在候选。
成功确认时在当前顺序中仅与直接前驱交换一次；首位不动，不直接跳Top1，
可以越过日常词等基础排序类别。相同提交只能应用一次。
选择时捕获事件，最终成功且仍聚焦的安全发布才兑现，试探、取消、失败、失焦不兑现。
事件快照只含完整身份，部分候选保持基础顺序置后，避免记录未消耗的无解后缀。
作用域更新时移除当前候选集已不存在的旧身份，再追加新身份；不会与不可见前驱交换。
共享纯函数 `ranking_indices` 与 `promote_order` 位于 engine/ranking.rs，
单元格估算位于 engine/paging.rs，平台和持久化不得复制另一套不同算法。

## 本地持久化与兼容

- 新文件 schema2，新字段 `adaptive_ranking_enabled` 默认true（用户已选择本地保存）、
  `rankings: Vec<RankingOrder>` 默认空。旧 schema1读取保留原entries/domains/learned/
  learning_enabled/generation；迁移时新自适应开关true，旧false不被伪称新调序的关闭选择。
  不在只读load时重写旧文件，下次合法更新才保存schema2。未知版本和坏文件仍保留并报错。
- 旧count数据保留，但TSF不再叠加 `count*512` 频次加权代替前移；旧公共接口及测试
  可保留兼容。新的窗口/CLI学习开关管理自适应开关；旧set_learning_enabled同时更新
  两个开关，清空同时清旧counts和新rankings，管理操作推进generation防旧批次复活。
- `promote_if_generation(generation, &[PromotionEvent])` 在锁内重读，检查自适应开关/
  generation。已有同作用域按持久最新顺序前移，缺失则从事件快照初始化后前移；
  这样跨进程更新不丢步，不声称并发时与过时UI位置完全一致。
- 上限：256个输入作用域，每域4096身份、所有rankings总计100000身份，仍合计16MiB
  和既有词条限制。新作用域超256时淘汰最旧域；已有域更新移到末尾。
  超限/非法事件批次整体拒绝，不部分写入、不删除旧文件。
- Worker增加 `submit_promotions(&[PromotionEvent])`，保持有界队列、非阻塞、首次错误后
  不再写、shutdown/join与DLL存活顺序。保存失败不回滚成功文档提交；内存顺序仍可使用，
  明确报告持久化失败，不宣称已耐久保存。

## TSF 即时效果与隐私

激活持有最新排序快照；成功提交的安全发布分支同步前移并更新激活快照，再投递worker。
同一激活内各context在下一次开始新输入前同步排序；正在组词不偷偷换位置。
每次成功事件只兑现一次，重放旧完成回调不得重复推进。
沿用编辑会话内scope白名单0/57/58及焦点/token/epoch/revision门禁；
密码、私密、未知scope无内存或磁盘学习。开关/清空修改重新切换生效，旧代次worker
不能恢复记录；不在按键回调做磁盘load/扫描，不引入网络和遥测。

## 验证

覆盖全部候选无遗漏可翻页、单行宽上限和长词省略、页索引与票据、按键试探不变、
第二项选一次变首项、第五项选一次只变第四、交错选择及重启重放、
schema1非破坏迁移/开关/清空/generation/跨进程合并、敏感与失败发布不提升。
安装器真实构建与ZIP/EXE核验不等于实际安装和桌面通过；不覆盖旧制品。
