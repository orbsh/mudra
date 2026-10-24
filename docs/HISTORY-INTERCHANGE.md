# 历史交换格式（history interchange）

浏览器、脚本、别的 mudra 之间搬历史记录用的一种文件格式。**格式在这里定义**：导出方把自己
的列映射到下面四个名字上，mudra 永远不需要知道源头的表结构。所以写导出脚本时是脚本向这个
格式靠拢，不是格式去迁就脚本。

## 形态

NDJSON：一行一条记录，UTF-8，行尾 `\n`。

```
{"url":"https://example.com/","title":"Example","visits":12,"last_visit":"2026-10-24T09:27:42.501Z"}
{"url":"https://doc.rust-lang.org/std/","title":"std - Rust","visits":3,"last_visit":"2026-10-23T18:02:11.004Z"}
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `url` | 是 | 记录的**身份**。同一个 url 的第二条记录折叠进已有行，不新增行 |
| `title` | 否 | 可为空串；为空时保留已有行的标题 |
| `visits` | 否 | 访问计数，缺省 1 |
| `last_visit` | 是 | RFC 3339 UTC，毫秒精度（`…T09:27:42.501Z`）。没有时间的行不算历史 |

时间用 RFC 3339 字符串而不是内部的时间戳数字，是为了让文件脱离任何一方的实现里读得懂：
可读、可按文本排序、任何语言的日期库都直接吃；毫秒固定三位，这样同一秒内的两次访问不会
在排序里打平。只接受 UTC（`Z` 或 `+00:00`）——别的时区不做静默换算，宁可当成坏行报出来。

## 容错

- 空行跳过；`#` 开头的行是注释（手写/手改的文件需要）。
- **未知字段忽略**。导出方可以顺手带上 `profile`、`first_visit`、自己的游标之类，不需要
  为它升格式版本。
- 坏行（不是 JSON、没有 url、`last_visit` 解析不了）跳过并计数，不会中断整份文件——一个
  60k 行的导出不该因为一行坏掉而全丢。导入结果里 `invalid` 就是这个数。

## 导入语义：折叠，不是追加

`import` 逐行折叠进历史表，`visits` 和 `last_visit` 都取 **max**：

- 同一份文件导入两次是幂等的（不是把计数翻倍）；
- 一份过期的导出不会把活着的行往回拨（计数和时间都不回退）；
- 文件描述的是"那个世界的状态"，不是一串事件——事件流才该用累加。

## 接口

没有面板命令，就这两个 HTTP 端点（守护进程守着 store，也守着文件系统）：

```
curl -s -X POST -d '{}'                              http://127.0.0.1:8899/export
curl -s -X POST -d '{"path":"~/vivaldi.ndjson"}'     http://127.0.0.1:8899/import
```

- `/export` → `{"ok":true,"path":"…","rows":N}`；`/import` → `{"ok":true,"path":…,"rows":N,"created":N,"updated":N,"invalid":N}`。
- `path` 省略时用默认文件 `<data dir>/history.ndjson`（`~/.local/share/mudra/history.ndjson`），
  导出就地覆盖：导出是历史表的快照，不是追加日志。
- `~`、`~/…` 展开成 home；**相对路径相对 home**（守护进程的 cwd 不是任何人的"这里"）。

## 导出方

`scripts/export_vivaldi_history.py` 是第一个实现：读 Vivaldi 的 `Default/History`
（SQLite：`urls(url,title,visit_count,last_visit_time)`），Chromium 的 1601 纪元微秒换算
成 RFC 3339 UTC 毫秒，输出本格式。它在 Vivaldi 开着的时候也能跑（先拷 `History`/`-wal`/
`-shm` 再读，不需要关浏览器）。

```
python3 scripts/export_vivaldi_history.py --out ~/vivaldi.ndjson
```

## 非目标

- 不携带 page/instance/标签：那些是 mudra 内部的簿记，历史记录没有"哪个窗口打开过它"。
- 不暴露存储层编码（keyspace 前缀、slot、键宽），也不承诺跨 okm 版本的兼容——文件是
  跨程序的中立面，存储是私有的。