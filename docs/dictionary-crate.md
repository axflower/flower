# 发布与消费者接入

## 当前接入状态

`flower-dictionary 0.1.0`已由维护者发布到crates.io，Flower使用纯registry精确依赖
`flower-dictionary = "=0.1.0"`，不再指定本地path。
`Cargo.lock`绑定crates.io来源与校验值
`2b7e0eed72e32737ed72ddc83d57b69128f141d71d932edd210d4d9e6b9fc634`。
发布包的源码、词表和许可内容已与本地审定候选逐文件核对；
发布归档与候选归档的整体摘要不同，不将本地候选摘要冒充registry校验值。

## 当前拆分边界

`flower-dictionary`独立维护公开拼音词表、来源许可、覆盖报告和离线生成工具，接口与Windows/TSF/Flower引擎无关。
Flower负责索引、解码、候选布局、排序、用户词和隐私门禁。默认数据没有改变：18张公开表中使用16张，默认并集201109个词对，保留803条人工表达及可选专名边界。

## 发布前检查

在词库仓库执行：

```powershell
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo package --locked --allow-dirty
cargo publish --dry-run --locked --allow-dirty
```

还须在完整源码仓库执行三种离线生成器的`--check`及对应Python测试，检查归档中的完整许可、词表和include目标，确认未带入用户数据或凭据。`--allow-dirty`仅用于显式检查当前工作树；正常发行建议由维护者先审核并提交最终版本，不自动Git写入。

发布账号通过本机终端`cargo login`配置，勿把令牌写入源码、脚本、日志或聊天。登录之后再运行`cargo publish --locked --allow-dirty`；拒绝、超时和未知上传结果须先核对原结果与registry，不能盲目重试。`--dry-run`成功不表示上传或名称归属已验证。

## Flower接入与状态

开发联调阶段可同时带精确版本和本地path；认证或registry发布未完成前，不能删path并声称普通在线构建可用。
成功发布并从registry验证同一版本后，正式依赖为：

```toml
flower-dictionary = "=0.1.0"
```

更新`Cargo.lock`并用Cargo metadata确认解析来源是crates.io，而不是patch、path或邻居checkout。测试真实crate依赖的查词、普通选择和打包器；更换数据版本须同步可信摘要及许可检查，不仅更新版本号。

Python打包器从Flower根包的直接依赖package ID定位词库manifest目录，不猜测本机兄弟目录、不扫描Cargo缓存。发行ZIP保留原词库布局和完整许可，用户无须安装Rust。原公开`data/pinyin`和生成器已迁至独立仓库，Flower内不保留第二份可编辑词库。

## 许可

词库Rust代码MIT；数据按来源分别适用Unicode-3.0、MIT和CC BY-SA 4.0。不得把crate整体描述为纯MIT，也不能通过Flower程序许可证覆盖数据义务。新crate的NOTICE及来源完整许可必须在分发中保留。
