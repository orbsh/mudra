# mudrad 存储 schema（okm over fjall）—— R1 验收规格

Rust 重写的 mudrad 用单一 fjall store 取代 sqlite 文件，经 okm 静态 derive
（`#[derive(DocumentEncode)]` + `Collection`）访问。本文是 PLAN §11 / R1 的
布局契约；代码对照本文评审，即验收关口。

设计前提（`ADR-rust-fullstack.md` 已锁定，本文不重开）：

- 只用静态 typed derive，不用 `okm-dynamic`（mudrad 是 Rust 进程；dynamic
  留给 aura 宿主语言场景）。
- 单一引擎（`fjall`）、单一 store 句柄。
- schema 变更 = 删库重建（原型期政策）。
- key 纪律：定宽二进制段、无分隔符。

## Collection 布局

命名空间（ns）按声明序分配。

### Tag（ns 1）

- key：`id: u32` BE（自增，走下方 `tag_id` 计数器）。
- payload：`parent_id: i32`（`-1` = 根哨兵）、`name`、`alias`、
  标志位 `isolated` / `required` / `hidden` / `deleted`（u8 0/1，okm
  payload 无 bool）、`rank`、`note`。
- 访问方法：
  - `by_parent { fields(parent_id) }`——树钻取主查询。
  - `by_name` func 索引（对 `name`）——裸名寻址（`/tags`、`/tag` API
    按裸节点名寻址，不按路径）。
- R1 全面不建覆盖（`includes`）：原型规模（tag 百级、页千级）下，前缀
  扫描收敛后回表是便宜操作；变长字段进覆盖 value 只换来一段手工解码
  的字节，无收益。

### Page（ns 2）

- key：`id: u64` BE（自增，走 `page_id` 计数器）。
- payload：`instance_id`、`target_id`（CDP 字符串 id）、`url`、`title`、
  `position`、`opened_at`、`closed_at`、`deleted_at`（软删：仅已关闭页
  可删；时间戳 0 = 无，okm payload 无 Option）、`parent_id`（开它的父页，
  CDP openerId）。
- 访问方法：
  - `by_instance { fields(instance_id) }`——列某上下文全部页。
  - `by_parent { fields(parent_id) }`——子树分拣查询。
  - `by_target` func 索引（对 `target_id`）——`/open {tabId}` 与
    `focus_page` 的反查；URL 反查降为 fallback。
  - URL 反查：n-gram 索引（`okm-ngram` 配方——多值 func 索引，
    n-gram → 条目，BM25 精排在调用方）。取代早期的全表扫 + 谓词方案。

### page_tag

Page↔Tag 连接（junction），双端各写一条 entry。跨树多选 = 多行；
树内单选 = app 层约束（与 sqlite 版语义一致）。

`JunctionEncode` 不自己声明 ns——entry 分居 Page ns 2 与 Tag ns 1
（方向由所在 ns 携带，段 0x3 判别符 + `#[ok_junction(1)]`）。
ns 3 随之弃用；按 ADR-0002（ns 永不复用）永久保留为空位。

### Instance（ns 4）

- key：`id: u32`（自增，走 `instance_id` 计数器）。
- payload：`profile`（situation 叶名）、`port`、`pid`、`running`、
  `proxy`、`extensions`。
- 访问方法：`by_profile` func 索引（对 `profile`）。

### SiteWidth（ns 5）

- key：`id: u32`（自增，走 `site_width_id` 计数器）。
- payload：`site`、`proportion`。
- 访问方法：`by_site` func 索引（对 `site`）。变长的 `site` 字符串
  刻意不做 key 段（定宽纪律；哈希进 key 会让 key 从身份变成查询维度
  ——索引才是正解）。

### State（ns 6）

- key：来自封闭枚举的定长 `u8` 槽位；value：该槽位的原文。
  无索引——固定小集合。
- 槽位：
  - `current_context` / `walker_mode` / `op_mod` / `sort` / `dev_mode`——
    同 sqlite `state` 表的键。
  - `epoch: u64`——失效计数器（见下节）。不属于任何页/tag payload，
    它是传输层状态。
  - `page_id` / `tag_id` / `instance_id` / `site_width_id`——每表自增
    计数器（u64）。okm 无内置序列；单写者模型下 State 计数器行无竞争，
    故计数器胜出，不用 `HighWater` reduce。

## epoch 失效信号

mudrad 每完成一次 CDP 驱动的写库（开页 / 页销毁 / 标题更新、tag 变更）
就把 `epoch` 行 +1。面板的数据面是纯 VirtualStorage 帧请求/应答，
所以推送只以提示形态存在：

- mudrad 在面板已开着的那条 WS 上发一帧，只带新 epoch 号、不带数据；
  面板收到即重扫 pages。
- 正确性从不依赖这帧：帧丢了最多延迟面板下一次自发 scan 看到新状态。
  信号是延迟优化，不是可靠性机制。
- epoch 存于 State（而非进程内存），mudrad 重启不回卷；面板拿上次
  见到的 epoch 一比即知要不要重扫。
- epoch 永不混进页/tag payload——数据模型保持与推送机制无关。

## 线协议说明

面板在同一条 WebSocket 上走裸 VirtualStorage 帧
（`VirtualStorageAsync` → `NestStorage::apply(bytes)`）；okm 类型层对象
（`Collection`）全部绑同步 `VirtualStorage`、只活在 mudrad 内部。
面板用纯字节 codec 编解码，并把 mudrad 的 ns 编号与 key 布局当作
本文所定义的契约。
