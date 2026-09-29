# ADR: mudra 接入 aura 联邦——事件外流为正体，动词为效应器

> **Languages:** [English](ADR-aura-probe-federation.md)（主文档） · [中文](ADR-aura-probe-federation.zh-CN.md)

日期：2026-09-29　状态：**proposal（v3 草案待审；接受前不写码）**

> v3 用 **prism 连接面**取代了自造的 HTTP endpoint 运输：事件就是普通的
> `{"ev": "mudra:<kind>"}` 帧，payload 是纯数据，命名空间是自由约定
> （完整字面字符串、按字符串相等订阅）——prism 的 dispatch 里没有命名空间
> 路由规则，任何地方也没有自动前缀机制。

> v1 在接受之前即被取代：它以 aura 侧适配器 booth 为先、把动词当主面，
> 效果是把操作入口搬到 prism、让 mudra 依赖联邦。用户的产品裁决
> （2026-09-29）：用户在 mudra 里操作；mudra 向外**推送**页事件；aura 侧
> booth（gravity，自带 krystallizer）消费事件；动词只是 handler 可拉用的
> 效应器，不是正门。联邦用于分享，不用于操作。v2 落实了这个主次重排，
> v3 把运输定到 prism。

## 背景

mudra 的控制面与 `ADR-rust-fullstack` 一致：单 daemon、单 store（fjall，
单写者）、8899 动词、面板走 epoch 总线。每次 lifecycle 写本来就知道自己
改了什么——store 的 epoch 纪律对真写返回 `Some(new_epoch)`、对 no-op 返回
`None`——daemon 也已经把一个数字扇出给面板，面板以重拉 shaped reads 响应。

这个扇出就是联邦要接的缝。外部消费者（aura 节点上的 booth，比如 gravity）
想要同样的知识——开了什么页、打了什么标、导航去了哪——既不轮询 mudra，
mudra 也不依赖它活着。

产品已定的角色分工（PLAN §11 场景定调，用户 2026-09-29）：

- **mudra 是操作发生的地方**——面板、扩展键位、CLI。它完全单机可用；
  集成是一行配置的 opt-in。
- **事件向外流**：lifecycle 事实产生时，mudrad 把自包含的页事件发到配置
  好的连接面上（§1）。"对方挂了捕获照常"——捕获是 mudra 自己的 store；
  外流是它的视图，绝不是它的源头。
- **处理在 aura 侧**：booth 消费事件（gravity 的核心是循环——一个事件
  一圈，天然适配）；处理写出的记忆落在**那个节点**的 krystallizer。
  记忆数据永不流进 mudra 的 store；booth 里没有 mudra 业务逻辑。
- **动词是效应器面**：处理需要浏览器**做动作**（开相关页、打标、关闭）
  时，booth 调 mudra 动词。一次性薄转发——不是正门。
- **联邦（ADR-0031 显式寻址）用于分享**：比如"把这个页分享到团队"，
  事件（带地址、跨节点）转发到团队节点上的远端 booth。分享的**动作**
  仍然发生在 mudra。

## 决策

### 1. 页事件外流——主集成面

- **触发点：epoch bump，结构化。** 每次 bump epoch 的 lifecycle 写同时
  构造一个事件：`page_opened`（insert/revive）、`page_closed`、
  `page_deleted`、`tags_set`、`tag_created`、`ctx_switched`、
  `page_updated`（标题/URL 导航）。no-op 不发——分界线就是 epoch 纪律
  已经画出的那一条。
- **形状：自描述，不需要回查。** 事件自带 handler 在 mudra **不在线**时
  也能行动的内容：`{kind, page_id, ctx, url, title, tag_ids, epoch, ts}`
  ——时点快照，不是指针。`page_id` 保持不透明跨节点引用的身份；它是
  关联键，不是可解引用的手柄。
- **投递：store 锁外，fire-and-forget。** daemon 的短锁纪律本来就禁止
  跨 await 持 store；发帧与 epoch 帧走同一条"写后锁外"路径。慢的或死的
  接收端绝不能拖住写——尽力发送、超时有界、失败即丢。**补齐靠拉**：
  漏了事件的消费者重读 `/ctx_pages` + `/forest`（store 是事实源；事件是
  扇出视图）。发送端不设队列：持久性归需要它的一方，而只有消费者知道
  自己需要什么。
- **配置：`config.kdl` `[events] prism = "ws://host/ws"`（缺省=关）。**
  默认关闭——零依赖形态是默认，集成是一行配置。本地面板扇出（WS 总线
  上的 epoch 数字）原样不动；这是第二条、结构化的、朝外的面。出站 WS
  客户端不是新依赖（mudrad 本来就对 CDP 说 WS）；单条连接、断线期间丢帧、
  退避重连——面板总线自身的形态反过来朝外。
- **运输：prism 事件帧（prism ADR-0017）。** 出帧 =
  `{"ev": "mudra:page_opened", "args": {…}}`，标准握手（先用
  `?protocol=json` 调试编码；CBOR 是面需要时接收侧的升级）。prism 的回帧
  （`<kind>.result` / `{"ev":"error"}`）mudra **一概忽略**——在一个会应答
  的协议上 fire-and-forget 依然成立：读回帧会把外流变成 RPC 依赖；不读，
  则 prism 整个下线与"未配置"在行为上等价。接收面拿到事件后做什么
  （路由给 gravity、落库、记未达）是 **aura 侧的决定**——本 ADR 刻意不
  规定。
- **命名是自由约定，不是机制。** 发送点写**完整字面字符串**
  （`mudra:page_opened`）；订阅方按字符串相等匹配——与面上今天处理
  点号/冒号名字的方式完全相同。明确拒绝：dispatch 里做命名空间解析
  （为一个观感加路由规则）、`@ns` 装饰器或 interface_schema 字段自动
  前缀（隐形机器——前缀从作者视野里消失，用户会忘线上名不是自己写的
  名字）。真正存在的隔离边界不用于此：realm 是硬分区；靠命名空间分享
  是**同一张面内的约定**，跨应用数据（gravity 订阅 `mudra:*`）因此
  始终可行。kind 清单的唯一事实源：本 ADR 的触发表 + 文档；消费者从
  那里抄名字，不从 mudra 的代码里。
- **args 是纯数据。** 快照是可读 JSON 事实——不是 Accrete 信封、不是
  okm 线字节、不是任何渲染形态。事件的全部契约就是：约定名字之下的
  自包含数据。

### 2. 处理在 aura 侧；记忆各回各家

- booth（gravity）收事件、每事件跑一圈循环；学到什么进它自己的
  krystallizer。除 §4 实现期的 hook collection 外，mudra 的 store 不从
  本 ADR 获得任何新数据。probe 铁律（probe 不持存储，okm ADR-0010 §7）
  是构造性成立的：没有任何东西被放在不该放的地方。
- booth 朝浏览器的出向能力恰好就是动词，经一个**薄转发 handler**调用
  （v1 的适配器，降到它该有的尺寸）：每动词三行
  `invoke("open", args) → POST 8899/open → 原样返回 JSON 值`；无动词
  逻辑、无缓存、无 store。失败是返回的 `{ok:false,err}` **值**（单信封，
  ADR-0036 谱系），不是第二条通道。
- 效应器面是**可选**的：只观察、只记记忆的 handler 永远不碰它。

### 3. 联邦=分享，动作点在 mudra

- "发现一个网页，分享到团队"=mudra 侧动作（`/share` 动词、面板入口）→
  事件/凭证按 ADR-0031 显式寻址转发到团队节点的远端 booth → 该节点的
  gravity 把它摄入团队记忆。显式寻址是特性（remote-booths 规则）；分享
  永远不会变成"从远端节点操作 mudra"。
- v1 的倒转作为反面记档：把 aura 注册表当作操作 mudra 的地方，等于给
  产品开出两个入口、外加一条 daemon 设计与用户工作流都从未要求的依赖边
  （mudra → prism）。v3 的运输让方向诚实：mudra 作为**生产者**在 prism
  面上说话——prism 挂了是一个丢帧，永远不是 mudra 的故障。

### 4. 钩子（与 v1 相同；与本条重排正交）

- 页面钩子（Tampermonkey 级）是**store 新 collection 里的 JS 脚本数据**
  （`Hook { id, name, match, script: Bytes, enabled: u8, added_at }`），
  由动词治理（`/hook_add|remove|list|enable`），导航时经**既有** CDP 桥
  注入（INJECT_JS 那根管子；daemon 侧按 URL 匹配选择）。wasm guest 进
  不了页面 world——与胶囊同源的 CSP 结构性排除（严格站点无
  `wasm-unsafe-eval`）；JS 是唯一能在扩展已运行的所有地方运行的 payload。
- 钩子的回写和任何本机进程一样走 8899 动词——而一个上报"值得记住"的
  钩子天然汇入 §1 **同一条**事件外流（钩子 → 动词 → lifecycle 写 →
  epoch bump → 事件）：没有第二条管线。

## 后果

- mudrad 获得：事件 schema + 发送路径（写后锁外）、`[events]` 配置组、
  `/share` 与 hook 动词（实现期；`Hook` collection 按 SCHEMA.md 双语
  同步规则走）。
- mudra 不失去任何东西：`[events]` 缺省时，现有行为逐字节不变；不新增
  mudra → aura/probe 的依赖边。
- aura 侧（本 ADR 记录落点，各仓自定自己的 ADR）：gravity 长出事件驱动
  入口（一事件=一圈循环）与薄动词转发 handler；未达簿记（如需要）归
  接收节点的面对待（dead-ring 先例）。
- 投递语义刻意选弱（尽力而为 + 拉取补齐）。若未来消费者证明需要保证
  投递，答案是**消费者自己的 store**（把事件 apply 进它自己的 KV），
  不是 mudrad 里的发送端队列——mudra 是浏览器，不是 broker。

## 引用

- mudra：`PLAN.md` §11 开放项（本 ADR 忠实重排的场景定调）、
  `docs/ADR-rust-fullstack.md`（单一控制点）、`docs/WS-SYNC.md`
  （epoch 总线）、`docs/SCHEMA.md`（epoch 纪律，Hook collection 待补）。
- aura：`docs/adr/0013`（联邦优于元数据共识——数据留在家里：与 §2 的
  记忆归属互为镜像）、`0031`（remote booths，显式寻址）、`0032`
  （booth）、`0035`（exec carrier）、`0036`（单信封——失败是值）。
- okm：`docs/adr/0010` §7（probe 不持存储）、`0027`/`0028`（传输无关
  接收；浏览器存储家族）。
- prism：`docs/adr/0017`（连接面——帧形状、身份、本 ADR 以生产者身份
  搭乘的 broadcast 遍历）。
