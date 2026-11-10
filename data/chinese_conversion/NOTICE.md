# OpenCC 通用简繁转换数据

四份字典按原始字节取自 crates.io 的 `ferrous-opencc 0.4.0` 归档中
`assets/dictionaries/`，其文件头明确指向 OpenCC 与 Apache-2.0。
包内 VCS 身份、每文件长度及 SHA256 见 `manifest.json`。
原始上游为 https://github.com/ByVoid/OpenCC ，OpenCC contributors。
本项目不采用 ferrous-opencc 的 Rust 代码或地区词汇表，不声称这些表是上游最新版本。

本项目以最长词组匹配后字符回退读取原文件，每条映射有多个值时选择第一项。
未修改四份字典内容；应用范围是候选显示和显式选择后的输出字形，
不将其用于改写宿主已有文本。通用繁体不进行台湾或香港术语替换。
完整 Apache License 2.0 见 `LICENSE-APACHE-2.0.txt`。
