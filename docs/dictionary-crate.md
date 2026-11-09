# GitHub词库接入

## 当前接入状态

词库已更名为`flower-dict 0.2.0`，仓库为`https://github.com/axflower/flower-dict.git`。Flower已通过正常Cargo解析更新锁文件，固定实际Git提交`d01e9ac5f6309e05344017ebce4680cf537854a9`。保留完整词库，不拆分多个crate，不再要求先发布到crates.io。

```toml
flower-dict = { git = "https://github.com/axflower/flower-dict.git", branch = "main", version = "=0.2.0" }
```

Rust引用使用`flower_dict`。Cargo.toml限定包版本和分支，Cargo.lock绑定实际提交；正常构建没有本地path或patch覆盖。已核对下载源的83个文件：77个逐字节一致，6个仅有文本换行差异；全部词表与许可正文逐字节一致。后续更换提交时也须验证真实来源，不能用本地验证副本代替Git联调。

真实Git依赖下已通过`cargo check --locked --offline --all-targets`和新增词条消费测试：“人工智能与大数据”可通过全拼、首字母简拼和混拼选词提交。Python资产解析器也已使用实际Cargo metadata验证来源。此前完整本地预检在相同源码/数据下有效，来源切换后补上述集成检查，不重复执行无关模型或桌面验收。

## 完整数据与消费者边界

独立词库维护数据、来源通知、覆盖报告及离线生成器，保持零第三方Rust运行依赖。0.2.0共有19张公开拼音表、17张默认表、2,088,650个默认拼音词对和2,168,043个不同词目。自动读音与既有读音、非拼音文本及待补项分别标记；消费者不应把完整词目收录当作全部可靠拼音标注。

Flower读取`DEFAULT_TSV`等静态接口，负责索引、全拼/简拼/混拼解码、候选排序、学习及TSF。Python验包器通过Cargo metadata中Flower根manifest与直接依赖package ID定位`flower-dict 0.2.0`，支持依赖别名，拒绝缺失、歧义、错误版本或错误根身份。它不猜兄弟目录、不扫描Cargo缓存、不读取真实用户词典。

## 远端同步及验证

1. 在GitHub将原词库仓库更名为`flower-dict`，并同步更改后的包名、示例和测试。不得仅修改仓库URL而保留旧Cargo包名。
2. 在Flower根目录正常运行Cargo解析，更新Cargo.lock；确认元数据中的包名为`flower-dict`、版本为`0.2.0`，source来自目标GitHub仓库并包含实际提交。
3. 运行受影响的构建、词典/引擎测试和Python消费者检查，确认没有本地覆盖。依赖已经获取后再用`--locked --offline`复验。
4. 只有实际远端内容与lock来源验证后，才报告GitHub接入完成。提交与推送由维护者核对最终差异后执行，不自动改用户Git索引或读取凭据。

```powershell
cargo metadata --format-version 1
cargo check --locked --all-targets
cargo test --locked --lib
python -m unittest scripts.test_package.DictionaryResolutionTests
```

GitHub依赖不经过crates.io的10MiB单包上传通道；完整数据的下载量、编译产物和索引内存仍需按实际规模评估。无需额外运行时下载数据。

## 历史接入与分发

0.1.0以旧名`flower-dictionary`发布于crates.io；当时registry校验值为`2b7e0eed72e32737ed72ddc83d57b69128f141d71d932edd210d4d9e6b9fc634`，默认词对201,109。该历史证据不证明本轮Git迁移已经完成，也不作为当前包名或来源要求。

数据和代码仍遵守各自现有通知，具体许可状态见词库NOTICE及完整正文。改名和使用Git不会改变数据权利或原有分发门禁。Windows发行ZIP及安装器需要独立验收，本次源码接入不隐含已交付新的安装包。
