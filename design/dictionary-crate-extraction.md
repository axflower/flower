# 独立的可复用拼音词库 crate

## 目标与边界

将当前公开拼音数据与重建来源从 Flower 独立到兄弟仓库 `flower-dictionary`，首次 crate 版本0.1.0，Flower通过精确版本依赖消费相同数据。提供任何输入法可用的纯Rust接口，不依赖Windows、TSF、Flower候选类型、用户路径、网络或推理运行时。

公开词库包括单字、核心词、人工日常词、13分类表、可选专名与既有MVP审计表。项目手工常用词分类清单也随数据维护，但是否采用其人工prior由消费者决定。个人词典、自动记忆词、排名历史、配置及AI模型/模型tokenizer不迁移或发布。

## 接口与行为

- 零第三方运行依赖，Rust2021/MSRV1.75，禁止unsafe；提供`DEFAULT_TSV`（保持原默认表顺序与字节）、`EVERYDAY_TSV`、`COMMON_USAGE_PAIRS`及`TABLES`。
- `Table`包含稳定id、相对path、文本、是否默认使用及许可说明；`entries()`逐行解析为借用的`Entry { pinyin, text, weight }`，错误带行号。默认静态组合可能跨表重复，不隐含候选排序；消费者可按规范拼音/文字取最高权重。全拼/简拼/混拼及窗口/排序仍是Flower引擎职责。
- 为使用非Rust语言的输入法保留UTF-8 TSV、来源/覆盖报告和完整许可；crate内发布运行与验包需要的词表、metadata、许可、小型基线检查集。现有Flower验包还验证jieba原始频次快照，因此明确携带该`dict.txt`和标准库生成器（可供验包调用），保持原排序来源校验，不以删门禁降低依赖。其余大型原始语料、provenance/rejected和生成测试只入新仓库，不进入crate，README说明完整重建需要源码仓库。
- 新仓库是数据的唯一权威；Flower删除已迁移的公开词表与生成入口，不保留两份可编辑数据。Flower验包从Cargo实际锁定依赖的manifest目录读数据，保留已有可信摘要、许可和原生载荷白名单；发布后的普通构建无需兄弟checkout。

## 许可与发布

程序接口代码MIT；混合数据各自继承Unicode-3.0、MIT、CC-BY-SA-4.0及完整归属/修改说明，不把包整体误标为MIT或更改上游许可证。Cargo包使用并存许可表达式并附明确NOTICE；数据的共享相同方式义务不被代码许可覆盖。

先本地path+version依赖联调、测试和cargo package/publish --dry-run，核对crate包清单/压缩体积/无私有路径或数据。用户已提出发布目标，但实际上传仍依赖合法账号权限与认证；不读取或打印token，不绕过权限、不认领他人名称。上传前最终内容固定，实际成功并从registry验证后，Flower切换为纯registry精确依赖且更新lock。无法认证时保留可发布包和明确阻塞，不声称已发布，不以本地path冒充registry可用。

## 验证

迁移前后逐文件SHA与默认词对并集一致；crate在无Windows依赖下测试完整词表解析、数量/有效权重/错误语义、默认选择和可选专名。验证package排除大原料但包含全部include_str、许可及Flower验包输入。Flower跑受影响词库/候选/原生消费者和打包边界，不重做无关视觉或239模型质量任务。公开发布不隐含Git提交/推送，不改用户已有Git索引。
