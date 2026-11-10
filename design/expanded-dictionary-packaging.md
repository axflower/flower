# 0.4.21 完整词库打包契约

本版继续生成含固定离线 AI 九文件的 Windows x64 测试包，不更改安装、
启用、图标或 TSF 行为。旧制品不可覆盖，旧包的只读验包兼容保留。

## 字典身份与许可

当前字典为 `flower-dict 0.2.0`，Cargo source 必须精确为
`git+https://github.com/axflower/flower-dict.git?branch=main#d01e9ac5f6309e05344017ebce4680cf537854a9`。
该包使用 `license-file = "NOTICE.md"` 而非 SPDX 表达式。仅对此精确身份，
允许缺失 SPDX；声明路径须解析为包根 NOTICE.md，正文须为 2370 字节且 SHA256
为 `689de482c01fb308d0f04b9accc6875452070e2f645907f94f82609d35c82447`。
继续校验并附带 MIT、Unicode-3.0、CC-BY-SA-4.0、pypinyin MIT 的完整正文；
其余依赖仍要求受支持的 SPDX 与对应完整正文，不能泛化接受任意 license-file。

manifest 保留真实 `spdx: null`，另记 `license_file: "NOTICE.md"`、
`license_status: "NOASSERTION"`、`distribution_status: "local-test-only"`。
第三方声明必须展示文件许可及新增数据再分发条件未核定，不能打印 None 或冒称完全 MIT。
本版为本地未签名候选；构建成功不代表已核定公开再分发授权。

## 扩充资产及可信摘要

0.4.21 及以后要求 payload 含以下精确四文件，未知 vocabulary 路径拒绝。
构建器从 Cargo 实际解析的字典根读取，安装器复用相同可信摘要校验，
不信任包内自行声明的摘要，不携带原始 words.txt。

| data/pinyin/vocabulary/ 下文件 | 字节 | SHA256 |
| --- | ---: | --- |
| expanded.tsv | 59302671 | 12669ecc1bda04813d5fdb9bd8196972ccbdca022f1b03050e217ae477043c34 |
| catalog.tsv | 31158596 | 17a0aceedb52fbf2c671073178ba91befdb21927a3309d33f90f6c0f04220e9b |
| coverage.json | 2900 | e9b410e6751f39d69720d67b73e9acf24962ac658e7d219db57c4fe7fc5f9b12 |
| unresolved.txt | 225804 | c3b1dfce035f061f4a8d7c93e8c582e31f4d537fbbbbf99d79dc2acc40ff8ac0 |

manifest 新增 `expanded_vocabulary`，记录上述固定文件身份及 2168043 个不同词目、
2088650 个默认拼音词对、1887541 个自动读音、45313 个待补音、
42662 个非拼音文本、192527 个既有读音词目；
标明自动读音未经逐词人工核对、许可 NOASSERTION 和仅本地测试状态。
安装器将记录与可信资产验证结果精确比较，缺失或篡改失败关闭。
0.4.21 以前不强制新增记录；如果旧包携带扩充资产也必须完整验证。

## 有界载荷

旧 0.4.20 展开约 149 MB；新 DLL 内嵌约 59 MB 扩充拼音，
并附约 91 MB 四文件，因此整 ZIP 的单文件及累计展开上限调整为 512 MiB。
仍拒绝加密、非普通文件、不安全路径及白名单外文件；
AI 私有九文件的 256 MiB 上限不变。最终报告实际展开大小。

## 验收

覆盖正确文件许可、错误包身份/来源/版本/声明路径、截断及改条款、
缺失完整许可正文，扩充资产缺失/未知路径/摘要及清单篡改、
旧包兼容和总载荷上限。再真实 release 构建、验 ZIP 与安装器，
只读验证 PE、版本、图标、摘要及无副作用帮助入口。
不自动安装、注册、启用、修改登录项或测试真实用户文档。
