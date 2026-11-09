# Flower 0.4.18 固定纠错测试集与实测结果

保留原219条固定数据，追加20条组合词误拼与正确控制；原金标未改。两个JSONL文件包含全部239条用例，CSV可用表格软件打开。

共167条误输入正例，132条能完整提交目标，35条未命中（79.0%）。正确候选可选择不等于自动改写，未承诺任意误输入全部还原。

| 分组 | 用例 | 正例 | 完整目标提交 | 漏报 |
| --- | ---: | ---: | ---: | ---: |
| baseline_219 | 219 | 151 | 116 | 35 |
| followup_20 | 20 | 16 | 16 | 0 |

来源命中数（可重叠）：普通完整候选6、词典拼写122、AI4。非金标AI建议1条，词表不支持等模型错误3条；原始响应均保留。

| 控制 | 用例 | AI建议 | 带可选拼写提议 | 实际AI来源等于金标 |
| --- | ---: | ---: | ---: | ---: |
| unchanged | 43 | 0 | 1 | 32 |
| abstain | 29 | 0 | 8 | 0 |

正确控制的原解码/来源差异、非金标AI建议和额外词典备选分别报告。abstain要求不发布AI纠正；词典备选也列出，不能把可选建议当作已经强制修改。超长拒绝不将截断前缀当作完整输入。

四条用户回归均经真实builtin输入、完整候选选择提交且无学习/个人排序事件验证。部分候选剩余raw不算成功；AI请求取真实普通候选，不用金标伪造来源。

组合建议限两个词（一段精确、一段单编辑）；合法整词或两个合法词覆盖时不强猜。多处错误、三段以上组合、同音歧义及词库外内容仍可能漏报。微信普通学习兼容属于独立原生范围与持久化验证，不能用此纠错集结果代替；本报告仅开发机i7-10700，不等于用户目标CPU/真实微信现场。

## 按错误类型

| 类型 | 正例 | 命中 | 漏报 |
| --- | ---: | ---: | ---: |
| deletion | 30 | 21 | 9 |
| insertion | 33 | 24 | 9 |
| neighbor_substitution | 1 | 1 | 0 |
| repetition | 28 | 20 | 8 |
| sentence | 4 | 4 | 0 |
| substitution | 29 | 20 | 9 |
| transposition | 42 | 42 | 0 |

## 全部239条

| ID | 类型 | 输入 | 目标 | 期待 | 完整目标/控制 | 首项 | AI响应 | 拼写提议 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| correction_001 | control/edited_but_legal_alternative | quexi | — | abstain | 通过 | 缺席 | — | — |
| correction_002 | word/deletion/final/rime | xuex | 学习 | correct | 通过 | 学校 | — | — |
| correction_003 | word/insertion/middle | xuqexi | 学习 | correct | 通过 | 需求恶习 | — | 学习 / 血洗 / 谑戏 / 需可惜 |
| correction_004 | word/repetition/middle | xueexi | 学习 | correct | 通过 | 学恶习 | — | 学习 / 血洗 / 谑戏 |
| correction_005 | word/transposition/within_syllable/initial | uxexi | 学习 | correct | 通过 | 学习 | — | 学习 / 血洗 / 谑戏 |
| correction_006 | word/transposition/across_syllables | xuxei | 学习 | correct | 通过 | 许西欸 | — | 学习 / 许巍 / 许飞 / 许西 |
| correction_007 | word/substitution/initial/onset | qiannao | 电脑 | correct | 通过 | 前闹 | — | 电脑 / 前导 / 钱包 / 潜逃 |
| correction_008 | word/deletion/final/rime | dianna | 电脑 | correct | 通过 | 地安娜 | — | 电脑 / 电压 / 典雅 / 戴安娜 |
| correction_009 | word/insertion/middle | diaqnnao | 电脑 | correct | 通过 | 地啊青年闹 | — | 电脑 / 嗲气恼 / 堤岸闹 / 堤岸脑 |
| correction_010 | word/repetition/middle | diannnao | 电脑 | correct | 通过 | 点嗯闹 | — | 电脑 / 点难熬 / 电难熬 / 殿难熬 |
| correction_011 | word/transposition/within_syllable/initial | idannao | 电脑 | correct | 通过 | 电脑 | — | 电脑 / 一旦闹 / 一旦脑 / 一旦恼 |
| correction_012 | word/substitution/initial/onset | qongzuo | 工作 | correct | 通过 | 求偶那个做 | — | 工作 / 请坐 / 动作 / 用作 |
| correction_013 | control/edited_but_legal_alternative | gongzu | — | abstain | 通过 | 弓足 | — | — |
| correction_014 | word/insertion/middle | gonqgzuo | 工作 | correct | 通过 | 干呕年轻工作 | — | 工作 / 弓足哦 / 弓足噢 / 弓足喔 |
| correction_015 | word/repetition/middle | gonggzuo | 工作 | correct | 通过 | 共工作 | — | 工作 / 公国 / 功过 / 贡国 |
| correction_016 | word/transposition/within_syllable/initial | ogngzuo | 工作 | correct | 通过 | 哦国内工作 | — | 工作 / 哦工作 / 噢工作 / 喔工作 |
| correction_017 | word/transposition/across_syllables | gonzguo | 工作 | correct | 通过 | 干呕女子过 | — | 工作 / 公国 / 功过 / 贡国 |
| correction_018 | word/substitution/initial/onset | qengyou | 朋友 | correct | 通过 | 黔鳄那个有 | — | 朋友 / 孟优 / 盟友 / 清幽 |
| correction_019 | word/deletion/final/rime | pengyo | 朋友 | correct | 通过 | 彭养鸥 | — | 朋友 / 蓬勃 / 彭勃 / 彭博 |
| correction_020 | word/insertion/middle | penqgyou | 朋友 | correct | 通过 | 喷全国有 | — | 朋友 / 喷汽油 / 盆汽油 / 喷气有 |
| correction_021 | word/repetition/middle | penggyou | 朋友 | correct | 通过 | 捧国有 | — | 朋友 / 捧固有 / 碰固有 / 喷固有 |
| correction_022 | word/transposition/within_syllable/initial | epngyou | 朋友 | correct | 通过 | 鄂偏牛国有 | — | 朋友 / 鄂朋友 / 俄朋友 / 恶朋友 |
| correction_023 | word/transposition/across_syllables | penygou | 朋友 | correct | 通过 | 喷一个藕 | — | 朋友 / 盆友 / 喷遗欧 / 盆遗欧 |
| correction_024 | control/edited_but_legal_alternative | qingtian | — | abstain | 通过 | 晴天 | — | — |
| correction_025 | word/deletion/final/rime | mingtia | 明天 | correct | 通过 | 命题啊 | — | 明天 / 命题 / 名家 / 名下 |
| correction_026 | word/insertion/middle | mingqtian | 明天 | correct | 通过 | 明确提案 | — | 明天 / 冥钱 / 名签 / 名额提案 |
| correction_027 | word/repetition/middle | mingttian | 明天 | correct | 通过 | 明天提案 | — | 明天 / 名额提案 / 民歌提案 / 名额天 |
| correction_028 | word/transposition/within_syllable/initial | imngtian | 明天 | correct | 通过 | 明天 | — | 明天 / 命题按 / 命题安 / 命题暗 |
| correction_029 | word/transposition/across_syllables | mintgian | 明天 | correct | 通过 | 民 | — | 明天 / 命题按 / 命题安 / 命题暗 |
| correction_030 | control/edited_but_legal_alternative | qianqi | — | abstain | 通过 | 前期 | — | — |
| correction_031 | word/deletion/final/rime | tianq | 天气 | correct | 通过 | 天气 | — | 天鹅 / 提案 |
| correction_032 | word/insertion/middle | tiaqnqi | 天气 | correct | 通过 | 体啊青年其 | — | 天气 / 天齐 / 田七 / 体安琪 |
| correction_033 | word/repetition/middle | tiannqi | 天气 | correct | 通过 | 提案嗯其 | — | 天气 / 天齐 / 田七 / 甜腻 |
| correction_034 | word/transposition/within_syllable/initial | itanqi | 天气 | correct | 通过 | 天气 | — | 天气 / 天齐 / 谈起 / 叹气 |
| correction_035 | word/transposition/across_syllables | tiaqni | 天气 | correct | 通过 | 体啊亲昵 | — | 天气 / 天齐 / 田七 / 甜腻 |
| correction_036 | word/substitution/initial/onset | quaile | 快乐 | correct | 通过 | 去爱了 | — | 快乐 / 坏了 / 取代了 / 取代乐 |
| correction_037 | word/deletion/final/rime | kuail | 快乐 | correct | 通过 | 快乐 | — | 酷爱 / 苦艾 |
| correction_038 | word/insertion/middle | kuaqile | 快乐 | correct | 通过 | 跨其了 | — | 快乐 / 跨企业 / 垮企业 / 夸企业 |
| correction_039 | word/repetition/middle | kuaiile | 快乐 | correct | 通过 | 酷爱 | — | 快乐 / 会计了 / 会计乐 / 会计肋 |
| correction_040 | word/transposition/within_syllable/initial | ukaile | 快乐 | correct | 通过 | 快乐 | — | 快乐 / 武凯了 / 徐凯了 / 武凯乐 |
| correction_041 | word/transposition/across_syllables | kualie | 快乐 | correct | 通过 | 跨厉鹗 | — | 快乐 / 酷烈 / 扩列 / 跨界 |
| correction_042 | word/substitution/initial/onset | qangzhu | 帮助 | correct | 通过 | 请安关注 | — | 帮助 / 庆祝 / 挡住 / 杨朱 |
| correction_043 | word/deletion/final/rime | bangzh | 帮助 | correct | 通过 | 帮之后 | — | 帮助 / 膀子 / 梆子 / 帮子 |
| correction_044 | word/insertion/middle | banqgzhu | 帮助 | correct | 通过 | 阪泉关注 | — | 帮助 / 绑住 / 办驱逐 / 半驱逐 |
| correction_045 | word/repetition/middle | banggzhu | 帮助 | correct | 通过 | 帮关注 | — | 帮助 / 绑住 / 半个住 / 半个主 |
| correction_046 | word/transposition/within_syllable/initial | abngzhu | 帮助 | correct | 通过 | 啊不能关注 | — | 帮助 / 绑住 / 啊帮助 / 阿帮助 |
| correction_047 | word/transposition/across_syllables | banzghu | 帮助 | correct | 通过 | 办中国湖 | — | 帮助 / 斑竹 / 班主 / 版筑 |
| correction_048 | word/substitution/initial/onset | qhouji | 手机 | correct | 通过 | 气候及 | — | 手机 / 收集 / 喉肌 / 首级 |
| correction_049 | word/deletion/final/rime | shouj | 手机 | correct | 通过 | 手机 | — | — |
| correction_050 | word/insertion/middle | shoquji | 手机 | correct | 通过 | 社会哦去及 | — | 手机 / 收集 / 首级 / 手肌 |
| correction_051 | word/repetition/middle | shouuji | 手机 | correct | 通过 | 受 | — | 手机 / 收集 / 首级 / 手肌 |
| correction_052 | word/transposition/within_syllable/initial | hsouji | 手机 | correct | 通过 | 还是藕及 | — | 手机 / 收集 / 搜集 / 喉肌 |
| correction_053 | word/transposition/across_syllables | shojui | 手机 | correct | 通过 | 手机 | — | 手机 / 收集 / 首级 / 手肌 |
| correction_054 | word/substitution/initial/onset | qiexie | 谢谢 | correct | 通过 | 且些 | — | 谢谢 / 器械 / 球鞋 / 切切 |
| correction_055 | control/edited_but_legal_alternative | xiexi | — | abstain | 通过 | 歇息 | — | — |
| correction_056 | word/insertion/middle | xieqxie | 谢谢 | correct | 通过 | 些倾斜 | — | 谢谢 / 泄泻 |
| correction_057 | word/repetition/middle | xiexxie | 谢谢 | correct | 通过 | 些谢谢 | — | 谢谢 / 泄泻 / 斜溪蟹 / 歇溪蟹 |
| correction_058 | word/transposition/within_syllable/initial | ixexie | 谢谢 | correct | 通过 | 谢谢 | — | 谢谢 / 泄泻 / 歇息鄂 / 歇息俄 |
| correction_059 | word/transposition/across_syllables | xixeie | 谢谢 | correct | 通过 | 溪蟹欸鄂 | — | 谢谢 / 溪蟹 / 泄泻 / 西北鄂 |
| correction_060 | word/substitution/initial/onset | qaoshi | 老师 | correct | 通过 | 请安哦是 | — | 老师 / 考试 / 要是 / 道士 |
| correction_061 | word/deletion/final/rime | laosh | 老师 | correct | 通过 | 老社会 | — | 老师 / 老实 / 老鼠 / 老视 |
| correction_062 | word/insertion/middle | laoqshi | 老师 | correct | 通过 | 老其实 | — | 老师 / 老实 / 老视 / 老式 |
| correction_063 | word/repetition/middle | laosshi | 老师 | correct | 通过 | 老师是 | — | 老师 / 老实 / 老视 / 老式 |
| correction_064 | word/transposition/within_syllable/initial | aloshi | 老师 | correct | 通过 | 啊咯是 | — | 老师 / 老实 / 老视 / 老式 |
| correction_065 | word/transposition/across_syllables | lasohi | 老师 | correct | 通过 | 拉 | — | 老师 / 老实 / 老视 / 老式 |
| correction_066 | control/edited_but_legal_alternative | quesheng | — | abstain | 通过 | 缺省 | — | — |
| correction_067 | word/deletion/final/rime | xueshen | 学生 | correct | 通过 | 学甚 | — | 学生 / 雪山 / 许慎 / 血沉 |
| correction_068 | word/insertion/middle | xuesqheng | 学生 | correct | 通过 | 学生权衡 | — | 学生 / 血色横 / 血色哼 / 血色亨 |
| correction_069 | word/repetition/middle | xueshheng | 学生 | correct | 通过 | 学社会横 | — | 学生 / 学术横 / 学术哼 / 学术亨 |
| correction_070 | word/transposition/within_syllable/initial | uxesheng | 学生 | correct | 通过 | 学生 | — | 学生 |
| correction_071 | word/transposition/across_syllables | xuseheng | 学生 | correct | 通过 | 需色横 | — | 学生 / 嘘声 / 须生 / 无色横 |
| correction_072 | word/substitution/initial/onset | qushuguan | 图书馆 | correct | 通过 | 去树冠 | — | 图书馆 |
| correction_073 | word/deletion/final/rime | tushugua | 图书馆 | correct | 通过 | 图书挂 | — | 图书馆 |
| correction_074 | word/insertion/middle | tushquguan | 图书馆 | correct | 通过 | 土司回去官 | — | 图书馆 / 图书官 / 图书管 / 图书关 |
| correction_075 | word/repetition/middle | tushuuguan | 图书馆 | correct | 通过 | 图书 | — | 图书馆 / 图书不管 / 图书无关 / 图书武官 |
| correction_076 | word/transposition/within_syllable/initial | utshuguan | 图书馆 | correct | 通过 | 图书馆 | — | 图书馆 / 图书官 / 图书管 / 图书关 |
| correction_077 | word/transposition/across_syllables | tsuhuguan | 图书馆 | correct | 通过 | 投诉湖官 | — | 图书馆 / 图书官 / 图书管 / 图书关 |
| correction_078 | word/substitution/initial/onset | qisuanji | 计算机 | correct | 通过 | 起诉氨基 | — | 计算机 / 乙酸基 |
| correction_079 | word/deletion/final/rime | jisuanj | 计算机 | correct | 通过 | 计算机 | — | 计算 / 肌酸 / 及苏安 / 即苏安 |
| correction_080 | word/insertion/middle | jisuqanji | 计算机 | correct | 通过 | 激素请安及 | — | 计算机 / 激素三级 / 激素感激 / 激素反击 |
| correction_081 | word/repetition/middle | jisuaanji | 计算机 | correct | 通过 | 激素啊氨基 | — | 计算机 / 激素氨基 / 计划氨基 / 激素三级 |
| correction_082 | word/transposition/within_syllable/initial | ijsuanji | 计算机 | correct | 通过 | 计算机 | — | 计算机 / 激素氨基 / 计算及 / 计算即 |
| correction_083 | word/transposition/across_syllables | jsiuanji | 计算机 | correct | 通过 | 计算机 | — | 计算机 / 激素氨基 / 计算及 / 计算即 |
| correction_084 | word/substitution/initial/onset | qianyingyuan | 电影院 | correct | 通过 | 倩影玉安 | — | 电影院 / 变应原 |
| correction_085 | word/deletion/final/rime | dianyingyua | 电影院 | correct | 通过 | 电影与啊 | — | 电影院 / 电影与 / 点英语 / 电英语 |
| correction_086 | word/insertion/middle | dianyiqngyuan | 电影院 | correct | 通过 | 点一起那个玉安 | — | 电影院 / 电影玉安 / 电影元 / 电影原 |
| correction_087 | word/repetition/middle | dianyinngyuan | 电影院 | correct | 通过 | 电音嗯玉安 | — | 电影院 / 电影玉安 / 电影元 / 电影原 |
| correction_088 | word/transposition/within_syllable/initial | idanyingyuan | 电影院 | correct | 通过 | 电影院 | — | 电影院 / 电影玉安 / 电影元 / 电影原 |
| correction_089 | word/transposition/across_syllables | diayningyuan | 电影院 | correct | 通过 | 地啊哟宁愿 | — | 电影院 / 电影玉安 / 电影元 / 电影原 |
| correction_090 | word/substitution/initial/onset | qangongshi | 办公室 | correct | 通过 | 请安公式 | — | 办公室 / 南宫市 / 办公是 / 办公时 |
| correction_091 | word/deletion/final/rime | bangongsh | 办公室 | correct | 通过 | 办公社会 | — | 办公室 / 办公司 / 半公司 / 板公司 |
| correction_092 | word/insertion/middle | bangoqngshi | 办公室 | correct | 通过 | 帮哦青年故事 | — | 办公室 / 办公式 / 半公式 / 板公式 |
| correction_093 | word/repetition/middle | bangonngshi | 办公室 | correct | 通过 | 帮哦嗯嗯是 | — | 办公室 / 办公式 / 半公式 / 板公式 |
| correction_094 | word/transposition/within_syllable/initial | abngongshi | 办公室 | correct | 通过 | 啊不能公式 | — | 办公室 / 啊办公室 / 阿办公室 / 嗄办公室 |
| correction_095 | word/transposition/across_syllables | bagnongshi | 办公室 | correct | 通过 | 罢工农事 | — | 办公室 / 罢工是 / 罢工时 / 罢工使 |
| correction_096 | word/substitution/initial/onset | qixingche | 自行车 | correct | 通过 | 启兴车 | — | 自行车 |
| correction_097 | word/deletion/final/rime | zixingch | 自行车 | correct | 通过 | 自行此后 | — | 自行车 / 自行刺 / 子行刺 / 字行刺 |
| correction_098 | word/insertion/middle | zixiqngche | 自行车 | correct | 通过 | 仔细青年贯彻 | — | 自行车 / 仔细清澈 / 自行扯 / 自行撤 |
| correction_099 | word/repetition/middle | zixinngche | 自行车 | correct | 通过 | 自信嗯车 | — | 自行车 / 自行扯 / 自行撤 / 自行掣 |
| correction_100 | word/transposition/within_syllable/initial | izxingche | 自行车 | correct | 通过 | 自行车 | — | 自行车 / 自行扯 / 自行撤 / 自行掣 |
| correction_101 | word/transposition/across_syllables | zxiingche | 自行车 | correct | 通过 | 自行车 | — | 自行车 / 自行扯 / 自行撤 / 自行掣 |
| correction_102 | word/substitution/initial/onset | qingqitian | 星期天 | correct | 通过 | 氢气提案 | — | 星期天 |
| correction_103 | word/deletion/final/rime | xingqitia | 星期天 | correct | 通过 | 行气体啊 | — | 星期天 / 行气体 / 行其他 / 型其他 |
| correction_104 | word/insertion/middle | xingqqitian | 星期天 | correct | 通过 | 兴趣气体按 | — | 星期天 / 兴起提案 / 星期提案 / 兴起天 |
| correction_105 | word/repetition/middle | xingqiitian | 星期天 | correct | 通过 | 兴起 | — | 星期天 / 兴起几天 / 星期几天 / 兴起李田 |
| correction_106 | word/transposition/within_syllable/initial | ixngqitian | 星期天 | correct | 通过 | 星期天 | — | 星期天 / 兴起提案 / 星期提案 / 兴起天 |
| correction_107 | word/transposition/across_syllables | xinqgitian | 星期天 | correct | 通过 | 新 | — | 星期天 / 兴起提案 / 星期提案 / 新启提案 |
| correction_108 | sentence/substitution/middle/preserve_prefix_suffix | xianzaiqainalishangban | 现在在哪里上班 | correct | 未命中/来源差异 | 现在亲爱那里上班 | — | — |
| correction_109 | sentence/deletion/middle/preserve_prefix_suffix | xianzaiainalishangban | 现在在哪里上班 | correct | 未命中/来源差异 | 现在爱那里上班 | 现在在那里上班 | — |
| correction_110 | sentence/repetition/final | xianzaizainalishangbann | 现在在哪里上班 | correct | 未命中/来源差异 | 现在在哪里上班嗯 | — | — |
| correction_111 | sentence/insertion/initial | qxianzaizainalishangban | 现在在哪里上班 | correct | 未命中/来源差异 | 期限在在哪里上班 | — | — |
| correction_112 | sentence/substitution/middle/preserve_prefix_suffix | womingqianqutushuguan | 我明天去图书馆 | correct | 未命中/来源差异 | 我名浅龋图书馆 | — | — |
| correction_113 | sentence/deletion/middle/preserve_prefix_suffix | womingianqutushuguan | 我明天去图书馆 | correct | 未命中/来源差异 | 我 | — | — |
| correction_114 | sentence/repetition/final | womingtianqutushuguann | 我明天去图书馆 | correct | 未命中/来源差异 | 我明天去图书馆嗯 | — | — |
| correction_115 | sentence/insertion/initial | qwomingtianqutushuguan | 我明天去图书馆 | correct | 未命中/来源差异 | 请问哦明天去图书馆 | — | — |
| correction_116 | sentence/substitution/middle/preserve_prefix_suffix | jintianqetianqihenhao | 今天的天气很好 | correct | 未命中/来源差异 | 今天黔鳄天气很好 | — | — |
| correction_117 | sentence/deletion/middle/preserve_prefix_suffix | jintianetianqihenhao | 今天的天气很好 | correct | 未命中/来源差异 | 今天鄂天气很好 | — | 今天天气很好 |
| correction_118 | sentence/repetition/final | jintiandetianqihenhaoo | 今天的天气很好 | correct | 未命中/来源差异 | 金天德天气很好哦 | — | 金天德天气很好 |
| correction_119 | sentence/insertion/initial | qjintiandetianqihenhao | 今天的天气很好 | correct | 未命中/来源差异 | 请进提案的天气很好 | — | — |
| correction_120 | sentence/substitution/middle/preserve_prefix_suffix | qingbangqodakaidiannao | 请帮我打开电脑 | correct | 未命中/来源差异 | 清棒球哦打开电脑 | — | — |
| correction_121 | sentence/deletion/middle/preserve_prefix_suffix | qingbangodakaidiannao | 请帮我打开电脑 | correct | 未命中/来源差异 | 清帮哦打开电脑 | — | 清帮打开电脑 |
| correction_122 | sentence/repetition/final | qingbangwodakaidiannaoo | 请帮我打开电脑 | correct | 未命中/来源差异 | 清帮我打开电脑哦 | — | 清帮我打开电脑 |
| correction_123 | sentence/insertion/initial | qqingbangwodakaidiannao | 请帮我打开电脑 | correct | 未命中/来源差异 | 轻轻帮我打开电脑 | — | — |
| correction_124 | sentence/substitution/middle/preserve_prefix_suffix | laoshiqhengzaibangongshi | 老师正在办公室 | correct | 未命中/来源差异 | 老师权衡在办公室 | — | — |
| correction_125 | sentence/deletion/middle/preserve_prefix_suffix | laoshihengzaibangongshi | 老师正在办公室 | correct | 未命中/来源差异 | 老师横在办公室 | — | — |
| correction_126 | sentence/repetition/final | laoshizhengzaibangongshii | 老师正在办公室 | correct | 未命中/来源差异 | 老师 | — | — |
| correction_127 | sentence/insertion/initial | qlaoshizhengzaibangongshi | 老师正在办公室 | correct | 未命中/来源差异 | 勤劳是正在办公室 | — | — |
| correction_128 | sentence/substitution/middle/preserve_prefix_suffix | womenqiqixuexizhongwen | 我们一起学习中文 | correct | 未命中/来源差异 | 我们漆器学习中文 | — | — |
| correction_129 | sentence/deletion/middle/preserve_prefix_suffix | womeniqixuexizhongwen | 我们一起学习中文 | correct | 未命中/来源差异 | 我么你其学习中文 | — | — |
| correction_130 | sentence/repetition/final | womenyiqixuexizhongwenn | 我们一起学习中文 | correct | 未命中/来源差异 | 我们一起学习中文嗯 | — | — |
| correction_131 | sentence/insertion/initial | qwomenyiqixuexizhongwen | 我们一起学习中文 | correct | 未命中/来源差异 | 请问哦们一起学习中文 | — | — |
| correction_132 | sentence/substitution/middle/preserve_prefix_suffix | pengyouqingtianlaiwojia | 朋友明天来我家 | correct | 未命中/来源差异 | 朋友晴天来沃甲 | — | — |
| correction_133 | sentence/deletion/middle/preserve_prefix_suffix | pengyouingtianlaiwojia | 朋友明天来我家 | correct | 未命中/来源差异 | 朋友 | — | — |
| correction_134 | sentence/repetition/final | pengyoumingtianlaiwojiaa | 朋友明天来我家 | correct | 未命中/来源差异 | 朋友明天来沃甲啊 | — | 朋友明天来沃甲 |
| correction_135 | sentence/insertion/initial | qpengyoumingtianlaiwojia | 朋友明天来我家 | correct | 未命中/来源差异 | 亲朋有明天来沃甲 | — | — |
| correction_136 | sentence/substitution/middle/preserve_prefix_suffix | woxiangqaiyibuxinshouji | 我想买一部新手机 | correct | 未命中/来源差异 | 我想起爱一不新手机 | — | — |
| correction_137 | sentence/deletion/middle/preserve_prefix_suffix | woxiangaiyibuxinshouji | 我想买一部新手机 | correct | 未命中/来源差异 | 我相爱异步新手机 | — | — |
| correction_138 | sentence/repetition/final | woxiangmaiyibuxinshoujii | 我想买一部新手机 | correct | 未命中/来源差异 | 我 | — | — |
| correction_139 | sentence/insertion/initial | qwoxiangmaiyibuxinshouji | 我想买一部新手机 | correct | 未命中/来源差异 | 请问哦西安购买一不新手机 | — | — |
| correction_140 | idiom/insertion/middle | xuerunishquixingzhou | 学如逆水行舟 | correct | 未命中/来源差异 | 学 | — | — |
| correction_141 | idiom/deletion/final | xuerunishuixingzho | 学如逆水行舟 | correct | 未命中/来源差异 | 学如你水性之后哦 | — | — |
| correction_142 | idiom/substitution/initial | querunishuixingzhou | 学如逆水行舟 | correct | 未命中/来源差异 | 却如逆水行舟 | — | 屈辱逆水行舟 / 切入逆水行舟 / 确乎逆水行舟 / 却步逆水行舟 |
| correction_143 | idiom/insertion/middle | yifanfqengshun | 一帆风顺 | correct | 通过 | 伊凡父亲恩高顺 | — | 一帆风顺 / 伊凡丰顺 / 疑犯丰顺 |
| correction_144 | idiom/deletion/final | yifanfengshu | 一帆风顺 | correct | 通过 | 伊凡枫树 | — | 一帆风顺 |
| correction_145 | idiom/substitution/initial | qifanfengshun | 一帆风顺 | correct | 通过 | 其范锋顺 | — | 一帆风顺 / 前锋顺 / 前锋瞬 / 前锋舜 |
| correction_146 | idiom/insertion/middle | huasheqtianzu | 画蛇添足 | correct | 通过 | 话社区提案足 | — | 画蛇添足 |
| correction_147 | idiom/deletion/final | huashetianz | 画蛇添足 | correct | 通过 | 画蛇添足 | — | 话社田 / 花社田 / 化社田 / 画社田 |
| correction_148 | idiom/substitution/initial | quashetianzu | 画蛇添足 | correct | 通过 | 去啊社田足 | — | 画蛇添足 / 去社田足 |
| correction_149 | idiom/insertion/middle | wangyaqngbulao | 亡羊补牢 | correct | 通过 | 往雅琦那个捕捞 | — | 亡羊补牢 / 汪洋捕捞 / 望洋捕捞 |
| correction_150 | idiom/deletion/final | wangyangbula | 亡羊补牢 | correct | 通过 | 汪洋不拉 | — | 亡羊补牢 / 汪洋不了 / 汪洋不利 / 汪洋不怕 |
| correction_151 | idiom/substitution/initial | qangyangbulao | 亡羊补牢 | correct | 通过 | 请安供养捕捞 | — | 亡羊补牢 / 杨阳捕捞 / 杨扬捕捞 / 汤羊捕捞 |
| correction_152 | user/neighbor_substitution/final/rime | xuexo | 学习 | correct | 通过 | 学校哦 | — | 学习 / 血泊 / 血洗 / 削波 |
| correction_153 | user/insertion/middle/preserve_syllables | xueerunishuixingzhou | 学如逆水行舟 | correct | 通过 | 学鄂如逆水行舟 | — | 学如逆水行舟 / 雪耳逆水行舟 |
| correction_154 | regression/sentence/first_syllable | xuanzaizainalishangban | 现在在哪里上班 | correct | 通过 | 选在在哪里上班 | 现在在哪里上班 | — |
| correction_155 | regression/sentence/first_syllable | xainzaizainalishangban | 现在在哪里上班 | correct | 通过 | 喜爱内在在哪里上班 | 现在在哪里上班 | — |
| correction_156 | regression/sentence/first_syllable | xinzaizainalishangban | 现在在哪里上班 | correct | 通过 | 新在在哪里上班 | 现在在哪里上班 | — |
| correction_157 | regression/sentence/first_syllable | xiaanzaizainalishangban | 现在在哪里上班 | correct | 通过 | 下岸在在哪里上班 | 现在在哪里上班 | — |
| correction_158 | control/correct_full_pinyin | xuexi | 学习 | unchanged | 通过 | 学习 | — | — |
| correction_159 | control/correct_full_pinyin | diannao | 电脑 | unchanged | 通过 | 电脑 | — | — |
| correction_160 | control/correct_full_pinyin | gongzuo | 工作 | unchanged | 通过 | 工作 | — | — |
| correction_161 | control/correct_full_pinyin | pengyou | 朋友 | unchanged | 通过 | 朋友 | — | — |
| correction_162 | control/correct_full_pinyin | mingtian | 明天 | unchanged | 通过 | 明天 | — | — |
| correction_163 | control/correct_full_pinyin | tianqi | 天气 | unchanged | 通过 | 天气 | — | — |
| correction_164 | control/correct_full_pinyin | kuaile | 快乐 | unchanged | 通过 | 快乐 | — | — |
| correction_165 | control/correct_full_pinyin | bangzhu | 帮助 | unchanged | 通过 | 帮助 | — | — |
| correction_166 | control/correct_full_pinyin | shouji | 手机 | unchanged | 通过 | 手机 | — | — |
| correction_167 | control/correct_full_pinyin | xiexie | 谢谢 | unchanged | 通过 | 谢谢 | — | — |
| correction_168 | control/correct_full_pinyin | laoshi | 老师 | unchanged | 通过 | 老师 | — | — |
| correction_169 | control/correct_full_pinyin | xuesheng | 学生 | unchanged | 通过 | 学生 | — | — |
| correction_170 | control/correct_full_pinyin | tushuguan | 图书馆 | unchanged | 通过 | 图书馆 | — | — |
| correction_171 | control/correct_full_pinyin | jisuanji | 计算机 | unchanged | 通过 | 计算机 | — | — |
| correction_172 | control/correct_full_pinyin | dianyingyuan | 电影院 | unchanged | 通过 | 电影院 | — | — |
| correction_173 | control/correct_full_pinyin | bangongshi | 办公室 | unchanged | 通过 | 办公室 | — | — |
| correction_174 | control/correct_full_pinyin | zixingche | 自行车 | unchanged | 通过 | 自行车 | — | — |
| correction_175 | control/correct_full_pinyin | xingqitian | 星期天 | unchanged | 通过 | 星期天 | — | — |
| correction_176 | control/correct_full_pinyin | xianzaizainalishangban | 现在在哪里上班 | unchanged | 通过 | 现在在哪里上班 | — | — |
| correction_177 | control/correct_full_pinyin | womingtianqutushuguan | 我明天去图书馆 | unchanged | 通过 | 我明天去图书馆 | — | — |
| correction_178 | control/correct_full_pinyin | jintiandetianqihenhao | 今天的天气很好 | unchanged | 未命中/来源差异 | 金天德天气很好 | — | — |
| correction_179 | control/correct_full_pinyin | qingbangwodakaidiannao | 请帮我打开电脑 | unchanged | 未命中/来源差异 | 清帮我打开电脑 | — | — |
| correction_180 | control/correct_full_pinyin | laoshizhengzaibangongshi | 老师正在办公室 | unchanged | 通过 | 老师正在办公室 | — | — |
| correction_181 | control/correct_full_pinyin | womenyiqixuexizhongwen | 我们一起学习中文 | unchanged | 通过 | 我们一起学习中文 | — | — |
| correction_182 | control/correct_full_pinyin | pengyoumingtianlaiwojia | 朋友明天来我家 | unchanged | 未命中/来源差异 | 朋友明天来沃甲 | — | — |
| correction_183 | control/correct_full_pinyin | woxiangmaiyibuxinshouji | 我想买一部新手机 | unchanged | 未命中/来源差异 | 我向卖艺不惜嗯手机 | — | — |
| correction_184 | control/correct_full_pinyin | xuerunishuixingzhou | 学如逆水行舟 | unchanged | 通过 | 学如逆水行舟 | — | 削弱逆水行舟 / 学府逆水行舟 / 薛埠逆水行舟 / 学徒逆水行舟 |
| correction_185 | control/correct_full_pinyin | yifanfengshun | 一帆风顺 | unchanged | 通过 | 一帆风顺 | — | — |
| correction_186 | control/correct_full_pinyin | huashetianzu | 画蛇添足 | unchanged | 通过 | 画蛇添足 | — | — |
| correction_187 | control/correct_full_pinyin | wangyangbulao | 亡羊补牢 | unchanged | 通过 | 亡羊补牢 | — | — |
| correction_188 | control/legal_alternative/selection_semantics | tamen | 他们 | unchanged | 通过 | 他们 | — | — |
| correction_189 | control/legal_alternative/selection_semantics | tamen | 她们 | unchanged | 未命中/来源差异 | 他们 | — | — |
| correction_190 | control/legal_alternative/selection_semantics | renyuan | 人员 | unchanged | 通过 | 人员 | — | — |
| correction_191 | control/legal_alternative/selection_semantics | renyuan | 人缘 | unchanged | 未命中/来源差异 | 人员 | — | — |
| correction_192 | control/legal_alternative/selection_semantics | shishi | 事实 | unchanged | 未命中/来源差异 | 实施 | — | — |
| correction_193 | control/legal_alternative/selection_semantics | shishi | 试试 | unchanged | 未命中/来源差异 | 实施 | — | — |
| correction_194 | control/legal_alternative/selection_semantics | quanli | 权利 | unchanged | 未命中/来源差异 | 权力 | — | — |
| correction_195 | control/legal_alternative/selection_semantics | quanli | 全力 | unchanged | 未命中/来源差异 | 权力 | — | — |
| correction_196 | control/legal_alternative/selection_semantics | xian | 先 | unchanged | 未命中/来源差异 | 西安 | — | — |
| correction_197 | control/legal_alternative/selection_semantics | xian | 西安 | unchanged | 通过 | 西安 | — | — |
| correction_198 | control/ambiguous_or_valid_initial_mixed | nh | — | abstain | 通过 | 你好 | — | — |
| correction_199 | control/ambiguous_or_valid_initial_mixed | xx | — | abstain | 通过 | 学校 | — | — |
| correction_200 | control/ambiguous_or_valid_initial_mixed | jsj | — | abstain | 通过 | 计算机 | — | — |
| correction_201 | control/ambiguous_or_valid_initial_mixed | tshuguan | — | abstain | 通过 | 图书馆 | — | 树冠 / 书馆 |
| correction_202 | control/ambiguous_or_valid_initial_mixed | jisuanj | — | abstain | 通过 | 计算机 | — | 计算 / 肌酸 / 及苏安 / 即苏安 |
| correction_203 | control/ambiguous_or_valid_initial_mixed | xuex | — | abstain | 通过 | 学校 | — | — |
| correction_204 | control/ambiguous_or_valid_initial_mixed | renyuan | — | abstain | 通过 | 人员 | — | — |
| correction_205 | control/empty | — | — | abstain | 通过 | — | — | — |
| correction_206 | control/digits | 123456 | — | abstain | 通过 | — | — | — |
| correction_207 | control/digits | nihao123 | — | abstain | 通过 | — | — | — |
| correction_208 | control/random | qzxvjqz | — | abstain | 通过 | — | — | — |
| correction_209 | control/digits | 000 | — | abstain | 通过 | — | — | — |
| correction_210 | control/random | !!! | — | abstain | 通过 | — | — | — |
| correction_211 | control/overlong | aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa…（512字母） | — | abstain | 通过 | — | — | — |
| correction_212 | control/multiunsupported/two_insertions | qqxuexi | — | abstain | 通过 | 全球学习 | — | 气血西 / 气血系 / 气血喜 / 气血浠 |
| correction_213 | control/multiunsupported/two_insertions | qqdiannao | — | abstain | 通过 | 全球电脑 | — | 起点闹 / 起点脑 / 起点恼 / 起点瑙 |
| correction_214 | control/multiunsupported/two_insertions | qqgongzuo | — | abstain | 通过 | 全球工作 | — | 气功做 / 气功作 / 气功坐 / 气功左 |
| correction_215 | control/multiunsupported/two_insertions | qqpengyou | — | abstain | 通过 | 全球朋友 | — | — |
| correction_216 | control/multiunsupported/two_insertions | qqmingtian | — | abstain | 通过 | 全球明天 | — | 取名提案 / 齐名提案 / 取名天 / 取名田 |
| correction_217 | control/multiunsupported/two_insertions | qqtianqi | — | abstain | 通过 | 全球天气 | — | 气体安琪 / 气体暗器 / 气体安岐 / 躯体安琪 |
| correction_218 | control/multiunsupported/two_insertions | qqkuaile | — | abstain | 通过 | 全球快乐 | — | 区块了 / 区块乐 / 区块肋 / 区块叻 |
| correction_219 | control/multiunsupported/two_insertions | qqbangzhu | — | abstain | 通过 | 全球帮助 | — | — |
| followup_001 | composed/transposition/first_word | jintainzaoshang | 今天早上 | correct | 通过 | 金台嗯早上 | — | 今天早上 / 锦田早上 / 金台早上 / 金坛早上 |
| followup_002 | composed/transposition/second_word | jintianzaosahng | 今天早上 | correct | 通过 | 今天早撒哼 | — | 今天早上 / 锦田早上 |
| followup_003 | composed/transposition/final | jintianzaoshagn | 今天早上 | correct | 通过 | 今天早杀国内 | — | 今天早上 / 锦田早上 / 今天造山 / 锦田造山 |
| followup_004 | composed/deletion/second_word | jintianzaohang | 今天早上 | correct | 通过 | 今天早杭 | — | 今天早上 / 今天导航 / 锦田早上 / 今天枣阳 |
| followup_005 | composed/substitution/second_word | jintianzqoshang | 今天早上 | correct | 通过 | 今天之前哦上 | — | 今天早上 / 锦田早上 / 今天左上 / 今天坐商 |
| followup_006 | composed/insertion/final | jintianzaoshangq | 今天早上 | correct | 通过 | 今天早上去 | — | 今天早上 / 锦田早上 |
| followup_007 | composed/repetition/second_word | jintianzaooshang | 今天早上 | correct | 通过 | 今天早哦上 | — | 今天早上 / 锦田早上 |
| followup_008 | composed/transposition/first_word | jinitanzaoshang | 今天早上 | correct | 通过 | 及泥炭早上 | — | 今天早上 / 几年早上 / 纪念早上 / 锦田早上 |
| followup_009 | composed/repetition/first_word | jinntianzaoshang | 今天早上 | correct | 通过 | 进嗯提案早上 | — | 今天早上 / 今年早上 / 锦田早上 / 近年早上 |
| followup_010 | composed/deletion/final | jintianzaoshag | 今天早上 | correct | 通过 | 今天早杀割 | — | 今天早上 / 锦田早上 / 今天造山 / 锦田造山 |
| followup_011 | composed/transposition/other_phrase | mingtainzaoshang | 明天早上 | correct | 通过 | 名太嗯早上 | — | 明天早上 |
| followup_012 | composed/substitution/other_phrase | womenxuexo | 我们学习 | correct | 通过 | 我们学校哦 | — | 我们学习 / 我们血泊 / 我们血洗 / 我们削波 |
| followup_013 | composed/substitution/other_phrase | gongzuoxuexo | 工作学习 | correct | 通过 | 工作学校哦 | — | 工作学习 / 工作血泊 / 工作血洗 / 工作削波 |
| followup_014 | composed/insertion/other_phrase | womengongzuoo | 我们工作 | correct | 通过 | 我们工作哦 | — | 我们工作 |
| followup_015 | composed/transposition/other_phrase | xuexigongzou | 学习工作 | correct | 通过 | 学习共走 | — | 学习工作 / 学习公主 / 学习共有 / 学习公有 |
| followup_016 | composed/transposition/other_phrase | shangbangongzou | 上班工作 | correct | 通过 | 上班共走 | — | 上班工作 / 上班公主 / 上坂工作 / 上班共有 |
| followup_017 | control/composed/correct | jintianzaoshang | 今天早上 | unchanged | 通过 | 今天早上 | — | — |
| followup_018 | control/composed/correct | womenxuexi | 我们学习 | unchanged | 通过 | 我们学习 | — | — |
| followup_019 | control/composed/two_transpositions | jintainzaosahng | — | abstain | 通过 | 金台嗯早撒哼 | — | — |
| followup_020 | control/composed/legal_alternative | jintianzaocan | 今天早餐 | unchanged | 通过 | 今天早餐 | — | — |
