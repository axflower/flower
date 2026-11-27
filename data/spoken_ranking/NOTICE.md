# SUBTLEX-CH 字幕词语统计

Copyright © 2010 Qing Cai and Marc Brysbaert.

Cai Q, Brysbaert M (2010). *SUBTLEX-CH: Chinese Word and Character Frequencies
Based on Film Subtitles*. PLoS ONE 5(6), e10729.
https://doi.org/10.1371/journal.pone.0010729

原始来源为文章的补充材料 Files S1，DOI `10.1371/journal.pone.0010729.s002`。
原文和 Crossref 记录的许可为 Creative Commons Attribution 4.0 International。
本目录随附许可全文。https://creativecommons.org/licenses/by/4.0/

- 原始下载 https://journals.plos.org/plosone/article/file?id=10.1371/journal.pone.0010729.s002&type=supplementary
- 原始 ZIP SHA-256 `cced9cb382914b93956a24fd06c10de709d351ff4501ac94c9ab136004421f07`
- 原始 `SUBTLEX-CH-WF` SHA-256 `086536450b1f77d0c7ff3ac0fc8375897162ace807d3167bec48b4c493434077`
- 派生表 SHA-256 `a8f63e844690609baad348f6503758e4449944d883c19e68e6d5ca5574676dc7`

`word-diversity.bin` 是 Flower 的派生数据。它从 99,121 条原始词形中保留了 50,328 条，
仅选取两个至八个基本汉字且至少出现在两个字幕上下文的词形，保留 `W-CD` 原始计数，
并按长度分组写成排序的 UTF-8 定长记录。`W-CD` 统计包含该词的上下文数量；原始语料
来自 6,243 个字幕上下文，总计约 3,355 万词。数据属于字幕语域，会受年份、题材和分词影响。

源码仓库保留补充材料原始 ZIP。运行包携带派生表、归属和许可，DLL 内嵌同一份派生表。
`python scripts/build_spoken_ranking.py --check` 可离线复现并校验完整派生表。
