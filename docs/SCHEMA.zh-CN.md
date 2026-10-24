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
  - 指称文本反查（`pages_by_fragment`）：精排**不再归调用方**，法则的唯一
    住所就在这里。指称文本 = 剥掉 scheme 的打字面（小写）+ 小写标题；召回
    是查询**词元**的子串包含（containment），且**没有长度地板**——两个字符
    也是一个词元；打分用 BM25，统计量取自语料自身（N/df/avgdl），精确，因为
    做召回的那次扫描同时就是普查。消费方：`/focus {query}`，它现在对该上下文
    里活着的开页做排名，而不是取"碰巧排在前面"的那个 live target。这份文本
    不声明任何索引（取代掉的 `by_text` gram 索引已移除）：召回既然是
    containment，posting list 就再无可剪枝之物；而 gram 窗口自带一个碎片可以
    比它更短的地板。布局注记：被移除索引的条目仍留在它那个 slot 里，成为无人读取
    的死字节——既有 store 照常可用，**不需要重建**。slot 就是声明序（okm ADR-0005），
    所以这个位置以后不能直接声明新索引：要么把旧索引保留为
    `#[ok_index(…, deprecated)]`（slot 占住、写入停止、`prune_deprecated_slots()`
    回收条目），要么按原型期政策删库重建——后者不急。
- (instance, target) 唯一性：sqlite 版用 UNIQUE 索引防多 daemon 竞写。
  Rust 版不需要库层约束——fjall 的文件锁让第二个写者根本打不开 store，
  且 `upsert_target` 的身份次序对已有 target 永不另起新行。

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
  - `page_id` / `tag_id` / `instance_id` / `site_width_id` / `history_id`
    / `event_id` / `hook_id`——每表自增计数器（u64）。okm 无内置序列；单写者模型下 State 计数器行
    无竞争，故计数器胜出，不用 `HighWater` reduce。

### History（ns 7）

- key：`id: u64`（自增，走 `history_id` 计数器）。
- payload：`url`、`title`、`visits`（u64）、`last_at`。
- 访问方法：`by_url` func 索引，对完整 `url`（变长，绝不落 key 段——定宽纪律）：
  **写路径**按精确 url 命中待累加行，无需全扫。排名的读面不声明索引，也不
  可能有——见下。
- **索引是前缀扫描，写路径必须比对 url 文本**：查 `https://x/` 会把所有以它
  开头的更长的 url 一并返回（按 key 序），取第一条命中会把无关地址折进一行
  （`/` 吞掉 `/a`，`/a` 吞掉 `/a/b`）。两个写者（`history_bump`、
  `history_absorb`）都用 `row.url == url` 过滤。不做过滤时按导入规模实测：
  60,122 行里 6,314 行被前缀吞掉；回归门是 `tests/history_test.rs`。
- 批量写：`history_absorb` 折叠一条交换格式记录（见
  `docs/HISTORY-INTERCHANGE.md`），计数与时间取 `max()`——同一份文件重复导入是
  幂等的，过期文件也不会把活着的行往回拨。open 动词仍用 `history_bump`（累加）。
- 语义：每个打开过的地址一行。`/open` 与 `/add` 累加 visits 与 last_at，
  标题标签在打开时从 watcher 同步的页行刷新；open 动词之外没有任何路径
  写 History——访问计数只属于它们。写 history 不 bump epoch：History 对
  面板不可见，open 动词自身的通知已经覆盖这次写。
- 消费面：`POST /history {query, limit}` → `MudraStore::history_search`，
  地址栏的四腿排名，RRF 融合（等权，k=60）：
  1. 指称文本的 BM25——查询词元被包含在剥 scheme 的地址或标题里。URL 的
     噪声在**这里**、在打分里控制，而不是靠把 URL 挡在召回之外：idf 让每个
     地址都有的东西（scheme、常见主机名）几乎不占权重，而 scheme 本身根本
     不在指称文本里；
  2. 子序列相似度（`sim_score`），判据文本同上两半——字母在地址里但不连续的
     查询（"ghb" → github）；
  3. 访问次数、4. 时间。后两条只给召回的集合排序，从不把行放进集合。
  召回是对整个日志的一次扫描，而这次扫描同时就是普查：N/df/avgdl 是语料
  自身的数字，不是计数器的替身、也不是对召回集取的平均。逼出这次扫描的是
  第 2 条腿——子序列不是任何东西的子串，没有索引能答 "ghb"，唯一能答的
  索引（字符集索引）的答法就是召回日志的大部分，那是同一次扫描、只是局部性
  更差。被它取代的两个读索引消失同理：`by_addr`（头部锚定的前缀腿——地址
  整体重新成为文本之后，"URL 当文本"里最没用的那一半）与 `by_title`（只索引
  标题的词法腿），在扫描成为召回之后已无可剪枝之物。布局注记：退役的 slot 里
  条目仍在，但无人读取——既有 store 照常可用，**不需要重建**；slot 即声明序
  （okm ADR-0005），这些位置以后不能直接声明新索引，除非把退役索引留作
  `deprecated` 占位（slot 占住、写入停止、`prune_deprecated_slots()` 回收条目）。日志哪天大到碍事，索引再以剪枝的
  身份回到扫描前面——它不再是召回的定义。
  缺席某条腿的行从该腿拿不到任何贡献，因此两条召回腿都不命中的地址，无论
  访问多少次都不会出现在非空查询里。空查询保留旧规则：全表、按访问次数排序。

### Event（ns 8）

- key：`id: u64`（自增，走 `event_id` 计数器）。
- payload：`kind`（自由字面字符串，`mudra:` 前缀）、`args`（自描述
  JSON 快照）、`at`（u64 墙钟毫秒，与所有 lifecycle 时间戳一样由调用方
  供给）。
- 访问方法：无——回放路径（`events_since`）就是主键序的 cursor 扫描；
  没有任何其他读取方（SCHEMA 无覆盖索引纪律的延伸：连二级访问需求都
  还不存在）。
- 语义：扩展协议观察面中只追加的那一半（ADR-extension-protocol §4）。
  发射编排在 lifecycle 转移里（单一事实源——watcher 与动词两条路径都从
  它流过）：Insert/Revive/URL 变化的 Refresh 发 `mudra:page_open`
  （watcher 重连的 baseline 重放**不是**事件——消费方经 /ctx_pages
  快照追赶）；watcher 的 destroyed 路径、`/close` 动词、`mark_down`
  清扫各发 `mudra:page_close`（尸体永远等不到 destroyed 事件）。
  `mudra:tag_set` 携带变更后的**全量** id 集——单行可重建，绝不发要
  排序的 delta。写不 bump epoch（面板不可见，与 History 同纪律）。
  保留/压实推迟到有实测需求；id 永不复用，构造上单调。
- 消费面：`POST /events {cursor, limit}`——只读回放窗口；checkpoint
  归消费方，daemon 从不推进它。`{tail: N}` 是同一读法的尾部锚定（最近
  N 条，id 升序——`head - N` 即游标，因 id 是稠密计数器而精确），响应
  始终带回 `head`（计数器）。stdio 实时扇出按 cursor 读，控制台日志视图
  按 tail 读。pull 路径自身已完整：at-least-once，崩溃=不推进、重读。

### Hook（ns 9）——按 schema 定稿纪律在实现启动时登记

- key：`id: u64`（自增，走 `hook_id` 计数器）。
- payload：`name`（自由字面字符串——人的手柄，也是 upsert 身份）、`match`
  （逗号分隔的 URL 子串；空=所有页面）、`code`（JS 源码，逐字注入）、
  `enabled`（u8 0/1——okm derive 无 bool）、`updated_at`（u64 墙钟毫秒，
  调用方供给）。
- 访问方法：`name` 上的 `by_name` func 索引——`hook_set` 以它为键做 upsert
  （`create_tag` 的 (parent,name) 幂等规则）；读取走全扫（钩子就一小把，
  没有别的二级需求）。`hook_set` 里**缺省字段保留原值**（只改 code 的一次
  编辑不得顺手清掉 scope——`instance_set_config` 同规则）；`code` 在新建时
  必填，且不能被清空（要停用就 delete 或 disable）。
- 语义：ADR-extension-protocol §5 的**轻量规则路径**——页面侧 JS 作为
  **数据**：不起进程、不走管道。治理面=8899 动词（`/hook_set`、
  `/hook_list`、`/hook_delete`）；执行落点=既有 CDP 注入桥（`INJECT_JS`
  同一根管子）：daemon 为每个 enabled 钩子注入一份
  `Page.addScriptToEvaluateOnNewDocument` 源码，带字面子串守卫，并对已加载
  页面 `Runtime.evaluate` 跑一遍同一份源码。**刻意一钩子一份源码**：语法错
  只杀掉那一个钩子，不会连带废掉整页注入。code 逐字内联——不用 `eval` /
  `new Function`（严格站点的 CSP 禁动态代码，而注入源码本身不受它管）。
  写钩子不 bump epoch（面板不可见，与 History / Event 同纪律）；效应面是
  对运行中实例活页面的一次 best-effort 重注入——装上即生效，无需 reload。
- 边界：钩子 = “不起进程”的那一半。需要状态/算力/网络的逻辑归扩展（宿主管
  托的子进程），它经 `host:invoke` 抵达同一个注入点——Hook collection 不是
  扩展存储的替代品。

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
