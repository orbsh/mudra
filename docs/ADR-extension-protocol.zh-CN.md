# ADR: 扩展协议——mudra 以 stdio 承载常驻子进程（BGI 线契约）

> **Languages:** [English](ADR-extension-protocol.md)（主文档） · [中文](ADR-extension-protocol.zh-CN.md)

日期：2026-10-01（v2，同日）　状态：**Accepted；事件日志切片已实现
（2026-10-01）**——ns 8 `Event` collection（SCHEMA 双语）、lifecycle 编排
发射（page_open / page_close / tag_set）、`POST /events {cursor, limit}`
只读回放窗口。扩展宿主（stdio spawn/handshake/扇出）是下一切片，未开始。

**取代**：[ADR-aura-probe-federation](ADR-aura-probe-federation.md)（v3，
proposal）——prism 运输腿与 effector-invoke 腿整体撤回。k10r 集成降级为
扩展的一个**消费示例**；mudra 对 k10r 一无所知。

**v2（2026-10-01）**用 **BGI 线契约**（aura ADR-0035 §3 及其应答契约
Update）取代自造帧词汇表，并撤回 LSP 类比：扩展是插件血统（宿主托管的
摊位），不是两个必须对齐异构实现的对等体之间的桥。v1 的两条投递面塌缩到
BGI 既有的 `Tier` 语义上；v1 的 `latency` class 作为冗余删除。

## 背景

mudra 的控制面是 RPC（`niri msg` 哲学：一切能力外部可见、可脚本化），
功能因此在编译器之外生长。一个事件——网页打开——可能同时有**多个**消费
者（分类、去广告、改样式）。若每个消费者都是一个网络服务，用户就等于在
运营一个服务注册表——管不过来。

k10r 转为独立服务（krystallizer ADR-0007 修订版）改变了这个问题的形状：
mudra → k10r 现在是直连 RPC 边（可查询、可回放），不是事件总线联邦。
prism 为一条本机、单消费者的链路额外引入一个进程依赖和一次帧翻译；且
任何总线上的推送本来就不携带送达保证。联邦运输因此整体去掉，剩下的问题
——一条事件流上的多个本机消费者——答案是**托管它们**，不是给它们发消息。

为什么是 BGI 而不是 LSP：LSP 式能力协商解决的是**两个独立演化的对等体**
的协调问题——没有任何编辑器或语言服务器能要求对方实现完整规范，能力清
单是唯一求交集的机制。mudra 扩展是单向的：宿主提供能力，扩展自行决定用
不用；线词汇表是静态的、双方天然知道（同机、同版纪律，decode 失败本身就是
版本错配信号——不需要一个协商面预先消除它）。这正是 aura 已经命名并落地
的形状——**BGI**（Booth Gateway Interface），FastCGI 血统的常驻桥：类型
化 host 帧、逐 target 静态应答语义（`Tier::Hot/Cold`）、每语言一个包装器
作为可移植面。mudra 采用该契约，不再发明第二种线格式。

## 决策

### 1. 扩展是常驻子进程（bgi 形态）

在 `config.kdl` 声明（`[extensions]` 组：名字 → 可执行文件路径——只是路
由元数据：跑哪些进程，与它们监听什么无关）。mudrad 负责 spawn 与守护：
`PDEATHSIG`、读 `/proc` state 探僵尸、带退避的重启——既有 spawn 纪律整
体复用。扩展永不绑端口；它唯一的 socket 就是管道。

### 2. 运输：BGI 帧，仅 json-lines codec

帧词汇表即 aura ADR-0035 §3（`call` / `result` / `host` / `host_reply`，
加 `iterate` 各臂——为何空载落地见「后果」）。codec：**仅 json-lines**
——CBOR 是 aura 给自带 codec 的载体留的声明式升级路径，本机单宿主面用
不上。坏 discriminator 在 decode 层失败，不是跑到一半炸。

### 3. 握手：hello 携带 interface_schema

```
mudra → { "type": "initialize", "protocol": 1, … }
ext   → { "type": "hello", "name": "adfree", "schema": { "on": ["mudra:page_open", …] } }
```

事件订阅声明在**脚本的 interface_schema** 里（aura 既有的声明面——ADR-
0016 `@cron`、ADR-0026 §4 装饰器派生 schema），**不写在配置里**：作者本
来就知道代码在监听什么，为改一个订阅去反复编辑部署配置是配置绝不该承载
的摩擦。进程外的 binary 无法像上传脚本那样被宿主内省，所以 BGI 垫片把
schema 作为会话数据带出来——注册时刻在管道上的表达。daemon 只向声明过
的 kind 扇出——不广播、无隐藏 dispatch。命名是自由字面字符串；schema 是
静态声明，不是协商。

### 4. 投递 = BGI `Tier`，不是新词汇

`Tier` 在脚本 schema 里随 handler 声明——逐 handler、静态、绝不在运行时
猜测（ADR-0035 Update）：

- **拦截语义 = `Hot` + deadline。** 导航前判决：`{call, event:
  "mudra:before_navigate", args}` 必须在 deadline 内以 `{result}` 应答；
  到期即 failure value（outer-Result 纪律——调用方永不悬挂）。mudra 的
  策略把到期映射为 **fail-open**（导航体验优先）：因判决迟到而拦掉一个
  页面，是比放行更糟的默认。
- **观察语义 = `Cold`。** 事件扇出永不泊住产生方：dispatch 返回
  Pending，handler 之后经 `host` 帧作答，或完全不作答。事件同时落**只追
  加事件日志**（mudra store；ns 槽在实现启动时分配）。崩溃或重启的扩展
  从自己的 cursor 回放——checkpoint 归消费方（krystallizer ADR-0008
  trailing 纪律：只追加日志 = 免费的重跑/滞后能力）。

一个 handler 二选一——同时是 fire-and-forget 又是 request-response 的帧
kind 不存在。

### 5. 效应在 mudra 执行，永不在扩展内

扩展进程是**决策面**（LLM 调用、规则、状态都在进程里）。效应经
`host: {type: "invoke", …}` 帧落回 mudra 动词面——invoke 臂映射到既有动
词目录（8899 面）；注入脚本由 Hook collection 治理、执行落点是既有
`INJECT_JS`。没有扩展直连 CDP，没有扩展开第二个浏览器。

**`store` 臂不实现。** BGI 的存储面是宿主代持的摊位状态（aura ADR-0026：
每个摊位类型在 realm 引擎里占一个真实 okm ns）。扩展状态归扩展自己（自
家目录，或经 k10r 自己的 HTTP 存 k10r）——每份数据只有一个家。事件日志
是**宿主**拥有的，属投递基础设施，不是扩展存储；两者勿混。

### 6. 扩展之间没有直连信道

扩展共享的**只有**事件日志。若分类器的产出要喂给改样式器，分类器把产出
写进它自己的家（扩展自有状态、经反向面进 mudra store、或 k10r），改样式
器在那里读。一条共享基座优于 N 条点对点——「服务太多管不过来」的解药不
是服务之间再加一条总线。

### 7. 信任模型不变

本机 = 信任边界。扩展面不设准入，与 8899 同裁决。跨节点身份若将来需要，
归联邦层，不归这里。

### 8. 可移植性是重点

每语言一个 BGI 垫片，两个世界通用：对着本契约写的扩展，mudrad 与 aura
effector 都能原样托管（k10r 同步扩展是第一个预期两边通吃的消费者）。
mudra 不依赖任何 aura crate——契约就是依赖（如 `aura_alloc` 之于 wasm
guest 的 ABI）。

## 消费示例：k10r 集成

一个扩展声明 `@on mudra:page_open / page_close / tag_set`（Cold），提取
知识（偏好、主题分类）并经 k10r 自己的 HTTP 面存入 k10r。mudra 不含任
何 k10r 专属代码；没有部署 k10r 的用户不运行这个扩展即可。

## 后果

- 8899 仍是人/CLI/面板的控制面；扩展面是第四个机器面（stdio、宿主管理）。
  两者不相通：扩展经管道与 daemon 对话，不打 HTTP 端口。
- 轻量规则类样式可以留在纯 Hook JS（数据进 store，不起进程）——扩展留给
  需要状态、算力或网络的逻辑，不替代 Hook 机制。
- 事件日志 = 新 collection（只追加、cursor 可回放）；ns 槽与键布局在实现
  启动时按 schema 定稿纪律登记进 SCHEMA 双语。
- `iterate` 各臂空载落地：mudra 今天没有可分发的流式动词。垫片在触及时以
  unknown-kind 错误应答；mudra 侧的 iterate 只在出现自报需求的消费者时落
  地。
- 永久撤回：prism 运输；effector 式后台→浏览器 RPC（被持久意图取代——
  声明的 handler + 钩子）；每扩展一个网络服务；wasm guest 进页面 world
  （CSP，结构性排除）；mudra spawn 或认识 k10r；命名空间路由与自动前缀
  机制；LSP 式能力协商作为**模型**（应答契约是静态+声明，不是对等体间对
  齐）；v1 自造的 fire/intercept 帧 kind 与 `latency` class（被 `Tier`
  吸收）。
