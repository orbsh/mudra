# ADR: 扩展协议——mudra 以 stdio 承载扩展子进程（Induction 线契约）

> **Languages:** [English](ADR-extension-protocol.md)（主文档） · [中文](ADR-extension-protocol.zh-CN.md)

日期：2026-10-01（v2，同日）　状态：**Accepted；事件日志切片已实现
（2026-10-01）**——ns 8 `Event` collection（SCHEMA 双语）、lifecycle 编排
发射（page_open / page_close / tag_set）、`POST /events {cursor, limit}`
只读回放窗口。扩展宿主（stdio spawn/handshake/扇出）已于 2026-10-10 实现
（`243fca4`——手写 BGI 形状的帧、双面 live smoke 通过），2026-10-13 起改
立在独立的 `induction-protocol` / `induction-host` crates 上：帧、带预算的
会话 I/O、PDEATHSIG spawn 与臂分派器都归库——mudra 直接驱动共享的
`route_frame` 核（effector 的进程载体跑同一个核）；事件拉取与动词面留在
mudra。拦截（`Hot`）面已于 2026-10-13 落地（见 §4）：`mudra:before_navigate`
判决（allow / deny / redirect）、宿主分配调用 id、超时放行预算，闸门挂
`/open` 与 `/nav` 的 `goto`。事件日志与钩子集同日拿到控制台视图（`events` /
`hooks` 两个切换面；日志按 `{tail: N}` 读，响应始终带回 `head`）。

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

为什么是 Induction 而不是 LSP：LSP 式能力协商解决的是**两个独立演化的对等体**
的协调问题——没有任何编辑器或语言服务器能要求对方实现完整规范，能力清
单是唯一求交集的机制。mudra 扩展是单向的：宿主提供能力，扩展自行决定用
不用；线词汇表是静态的、双方天然知道（同机、同版纪律，decode 失败本身就是
版本错配信号——不需要一个协商面预先消除它）。这正是 aura 已经命名并落地
的形状——**BGI**（Booth Gateway Interface），FastCGI 血统的进程桥：类型
化 host 帧、逐 target 静态应答语义（`Tier::Hot/Cold`）、每语言一个包装器
作为可移植面——后抽取为独立的 **Induction** 协议 crates（`~/world/induction`）。
mudra 经这些 crates 采用该契约，不发明第二种线格式。

## 决策

### 1. 扩展是管道上的子进程（induction 形态）

在 `config.kdl` 声明（`extensions` 组：名字 → 可执行文件路径，或
`{ cmd; idle_ttl; resident }`——只是路由元数据：跑哪些进程，与它们监听什么
无关）。mudrad 负责 spawn 与守护：`PDEATHSIG`、读 `/proc` state 探僵尸、带
退避的重启——既有 spawn 纪律整体复用。扩展永不绑端口；它唯一的 socket
就是管道。

### 1b. 留存由投递预算决定

本文第一版说「常驻子进程」，而且真的是这个意思：`supervise` 开机读配置、
把每条声明都拉起来，然后一直挂着。那是 FastCGI 的条件反射，对一个一小时
才被问一次的 guest 是错的——induction 的形状是**按需冷起 + 留存窗口**
（aura 的 `lifecycle.idle_ttl` + evictor），mudra 现在也是：

- **开机什么都不起**，除非声明里写了 `resident true`。
- 有东西要投递（一行事件、一次判决调用）才起：`ensure` 负责 spawn、握手、
  注册——**注册之后**调用方才能写它想得到回答的那一行，因为 session 在握手
  时就把 cursor 拍下来了，先写行会被当成历史跳过。
- 每投递一帧 `last_activity` 就前进；超过 `idle_ttl` 秒 session 自己退掉，
  下一次投递再冷起。默认 60 秒，`0` = 永不回收。
- **例外，且不可配置：** 声明了 Hot 事件的 session 永不回收。200ms 的判决
  预算付不起 spawn + 握手，被回收的 Hot guest 只会让闸门一直静默放行。
  留存由预算裁决，声明只表达 guest 想要什么。

这次换回来的冷起成本实测：`python3` 解释器 16ms（20 次中位），而下面那个
guest 一次 `rbw list` 是 138ms——spawn 相对真正的工作是噪声。

### 2. 运输：Induction 帧，仅 json-lines codec

帧词汇表归 `induction-protocol`（`call` / `result` / `host` / `host_reply`，
加 `iterate` 各臂——为何空载落地见「后果」）；库把它记录为 aura ADR-0035 §3
的抽取——aura 是名字的来处，库是它的家。codec：**仅 json-lines**
——CBOR 是契约给自带 codec 的载体留的声明式升级路径，本机单宿主面用
不上。坏 discriminator 在 decode 层失败，不是跑到一半炸。

### 3. 握手：hello 携带 interface_schema

```
mudra → { "type": "initialize", "protocol": 1, … }
ext   → { "type": "hello", "name": "adfree", "schema": { "on": ["mudra:page_open", …] } }
```

事件订阅声明在**脚本的 interface_schema** 里（契约继承的声明面——做法起于
aura ADR-0016 `@cron`、ADR-0026 §4），**不写在配置里**：作者本
来就知道代码在监听什么，为改一个订阅去反复编辑部署配置是配置绝不该承载
的摩擦。进程外的 binary 无法像上传脚本那样被宿主内省，所以 Induction
guest 把 schema 作为会话数据带出来——注册时刻在管道上的表达。daemon 只向声明过
的 kind 扇出——不广播、无隐藏 dispatch。命名是自由字面字符串；schema 是
静态声明，不是协商。

### 4. 投递 = 声明的 `Tier`，不是新词汇

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

**已实现（mudra 侧，2026-10-13）。** 调用帧走 `mudra:before_navigate`，
args = `{url, ctx}`；应答 = `{"verdict":"allow"|"deny"|"redirect","url"?}`。
调用 id 由宿主分配，取自与事件日志行 id **不相交**的空间（guest 应答一个
已投递事件，永远不会被误配成判决）；等待有宿主侧预算（200 ms——hello
schema 里 `Tier`/deadline 的位置是预留，但还没有任何一方携带它）；多个声明
扩展按注册序（名字序）取**第一个非 allow**。`deny` 以
`{"blocked":true,"by":扩展名,"url":…}` 回复调用方且不执行动词；`redirect`
改写请求的 url。闸门挂在 8899 的两个导航**入口**——`/open`（链接点击
beacon、控制台、CLI）与 `/nav` 的 `cmd: goto`——刻意**不挂** `run_verb`：
guest 自己的 `host:invoke` 不能被它正在执行的那个判决拦住。所有失败面
（无声明会话、预算到期、通道断、队列满、应答不可解析）一律放行。

### 4b. 事件可以带回答（ask 面）

§4 说一个处理器要么 Hot（判决）要么 Cold（投递即完），没有既此又彼的帧。
后来发现需要第三种形状，而它不需要新词汇：**日志行号就是调用 id**。调用方
写一行事件，声明了该 kind 的 guest 回 `{id: <行号>, result}`，mudrad 把值
交给等在这个 id 上的调用方。

- `POST /fill_password {url, ctx?, user_hint?}` → mudrad 先冷起候选 guest，
  写一行 `mudra:fill_password`，等 `ASK_BUDGET`（15 秒），返回
  `{ok, event_id, answer}`。它不是 Controller 动词：等的这 15 秒里不持
  store 锁（否则唯一的 store 互斥被停 15 秒），所以它挂在 `run_verb` 之前的
  HTTP 外壳上。
- 调用方通常是地址栏（`b`、`:fill-password`），拿到值后用原型上的 setter
  加一对 `input`/`change` 事件填进聚焦的密码框——裸写 `el.value =` 对任何带
  value tracker 的框架都不可见。
- 失败是 fail-**CLOSED**（与判决相反）：没有 guest 声明、预算内没答、库里有
  多条候选，都变成状态栏上的一行字。迟到的密码没有价值，填错的更糟。
- 日志顺带留一条请求，控制台 `events` 面板能看到每次填充尝试。单写一行事件
  不移动 epoch（日志是面板不可见 collection），所以这条路径自己 bump 一次——
  那个 bump 就是扇出等待的唤醒信号。

**随附 guest：** `scripts/guests/bw_fill.py`——手写 Induction guest（不引客户端
库），经 `rbw` 从 Bitwarden 库取密码。只按 **URI host** 匹配，绝不按条目名
（这个库里 `rbw get github` 会答 "multiple entries found"：有五个）；用页面上
已经填好的用户名收窄，仍多义就报错而不是猜。库锁着就如实报错（`rbw` 没有
可弹提示的终端——stdin 是线路）。

### 5. 效应在 mudra 执行，永不在扩展内

扩展进程是**决策面**（LLM 调用、规则、状态都在进程里）。效应经
`host: {type: "invoke", …}` 帧落回 mudra 动词面——invoke 臂映射到既有动
词目录（8899 面）；注入脚本由 Hook collection 治理、执行落点是既有
`INJECT_JS`。没有扩展直连 CDP，没有扩展开第二个浏览器。

**已实现（mudra 侧，2026-10-13）。** Hook collection = ns 9（`name` /
`matches` / `code` / `enabled` / `updated_at`，按 name 幂等 upsert，不 bump
epoch），治理面 `/hook_set`、`/hook_list`、`/hook_delete`，CLI 侧 `mudra hook
set|ls|rm`，控制台侧 `hooks` 视图（同一组动词，不开第二通道）。执行走同一
根管子：每个 enabled 钩子一份**自己**的带守卫注入源码（`matches` = 逗号分隔
URL 子串，空 = 所有页）——语法错只杀掉那一个钩子；
code 逐字内联（不用 `eval` / `new Function`，严格站点 CSP 禁动态代码）；
写钩子后对运行中实例的活页面做一次重注入（reload 按当前集合重新武装，这也
正是 disable / delete 生效的方式：已经跑起来的代码收不回来）。

**`store` 臂不实现。** 该臂在库里存在是为了 aura 的摊位状态（aura
ADR-0026：每个摊位类型在 realm 引擎里占一个真实 okm ns）；mudra 没有摊位
这回事。扩展状态归扩展自己（自家目录，或经 k10r 自己的 HTTP 存 k10r）——
每份数据只有一个家。事件日志是**宿主**拥有的，属投递基础设施，不是扩展
存储；两者勿混。

**臂集说明（词汇表归库）：** 宿主帧可用的臂集就是 `induction-protocol` 的
`Arm`——共 11 个：`invoke | iterate.start/next/dispose | store | emit |
timer.register | timer.cancel | queue.depth | queue.skip_to_head | schema`。
库把这套臂记录为 aura ctx 对象 CALLABLE 成员的抽取（aura ADR-0043 §5 是名字
的出处，不是本仓的依赖边）。臂字符串是点分方法路径；`emit` 按契约**不带
应答**（fire-and-forget）。mudra 只实现 `invoke`，不需要 storage/timer/queue
各臂（§5 的单一家裁决）；扩展面将来扩容时逐字采用库里这些名字，不造同义
词——全部宿主一个词汇表。注册（`on`/`receives`）没有运行时臂：子进程加载
期装配自己的注册表，由 hello/schema 应答携带（保留 `__sleep`/`__wake` 钩子
走同一个数组）。

### 6. 扩展之间没有直连信道

扩展共享的**只有**事件日志。若分类器的产出要喂给改样式器，分类器把产出
写进它自己的家（扩展自有状态、经反向面进 mudra store、或 k10r），改样式
器在那里读。一条共享基座优于 N 条点对点——「服务太多管不过来」的解药不
是服务之间再加一条总线。

### 7. 信任模型不变

本机 = 信任边界。扩展面不设准入，与 8899 同裁决。跨节点身份若将来需要，
归联邦层，不归这里。

### 8. 可移植性是重点

每语言一个 Induction guest，两个世界通用：对着本契约写的扩展，mudrad 与 aura
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
- `iterate` 各臂空载落地：mudra 今天没有可分发的流式动词。宿主的臂表在触
  及它们时以显式错误值拒答；mudra 侧的 iterate 只在出现自报需求的消费者时落
  地。
- 永久撤回：prism 运输；effector 式后台→浏览器 RPC（被持久意图取代——
  声明的 handler + 钩子）；每扩展一个网络服务；wasm guest 进页面 world
  （CSP，结构性排除）；mudra spawn 或认识 k10r；命名空间路由与自动前缀
  机制；LSP 式能力协商作为**模型**（应答契约是静态+声明，不是对等体间对
  齐）；v1 自造的 fire/intercept 帧 kind 与 `latency` class（被 `Tier`
  吸收）。
