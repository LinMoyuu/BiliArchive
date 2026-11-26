# Bili Archive v1

统一文档格式为 `bili-archive` / `version: 1`。新归档根目录是设置中的导出目录下的 `archive-v1/`。自动读取及增量写入只接受新版。评论按类型、每稿件一份 JSON 持续自动保存，消息同样自动归档；支持通过账号菜单导入新版 JSON、归档目录或离线资料包；不提供拆解或旧格式转换，不在启动时迁移或删除已有文件。

```text
archive-v1/
  manifest.json
  <account_uid>/comments/<author_uid>/
    videos/<oid>.json
    dynamics/<oid>.json
    reposts/<oid>.json
    articles/<oid>.json
    audio/<oid>.json
  <account_uid>/messages/
    replies.json
    mentions.json
    likes.json
    private/user_<uid>.json
    private/group_<id>.json
  <account_uid>/reports/<run_id>.json      # 每次开始／继续采集的独立报告
  resources/<account_uid>/<resource_id>.<ext>
  resources/<account_uid>/index.json
```

作者 UID 与归档账号 UID 是不同概念。评论先按归档账号隔离，再按作者存储；账号消息同样按归档账号隔离，避免不同账号采集同一作者时互相覆盖。文件里的资源路径始终相对 `archive-v1/` 根目录，不能通过当前 JSON 所在目录猜测。手动合并导出是 JSON 加同目录的 `resources/`，复制时应一起携带；自动读取仍验证资源引用、账号与安全路径。没有账号的公开评论快照允许 `meta.account_uid: null`，资源目录使用 `public/`。

## 文档区域

| 字段 | 含义 |
| --- | --- |
| `meta` | 归档账号、程序版本、带时区的导出时间 |
| `source` | comments/messages、内容分类、作者、主题或会话范围 |
| `users` | 按 UID 索引的昵称、头像资源引用；匿名点赞用户使用局部键，UID 为 null |
| `subjects` | 按类型和对象 ID 索引的主题，含正文、封面、发布时间等资料 |
| `sessions` | 私信与粉丝团会话，保留原始会话元数据 |
| `items` | 评论、楼中楼、私信、回复通知、@通知、点赞通知 |
| `resources` | 按资源 ID 索引的地址、保存策略、文件状态、大小、MIME、MD5 校验及错误 |
| `collection` | 当前文件的实际归档数、接口报告数与可确认的完整性；不包含整轮任务异常 |

身份标识使用十进制字符串，不能交给 JavaScript Number 转换。`type` 是统一类型字段，模型、动态资料及报告异常均使用此命名，不读取旧字段别名。Rust 自有字段使用 `r#type`；标准库和 Slint 外部 API 的固定名称不改，来源 `raw` 不改写。`timestamp` 和规范时间字段使用 Unix 秒，缺失时间为 null；`raw` 保留来源内容，已知数字 ID 转为无损字符串，不能补造旧档案中不存在的原始数据。未知日期、数量与完成状态使用 null，而不是伪造成功。

评论与楼中楼采用平铺 `items`：`subject_key` 关联稿件，`user_uid` 关联用户，`root_id`、`parent_id` 表达关系。`fields` 保存来源特有的评论属性（置顶、等级、点赞数、IP 属地等），通用字段与引用为读取依据。主题保留来源的内容类型、评论对象 ID 及动态 ID，不能互相代替。

私信项目使用 `session_key`、`sender_uid`、状态及资源引用；原始消息类型、序列号、特殊内容和未知字段仍保留在 `raw` 中。通知保留正式主题引用、原网页地址和原始 native_uri；客户端协议仅在网页展示时转换，不改写源地址。点赞分开记录 `like_count`、用户明细、各自时间与 `like_detail`，接口遍历结束不代表实际名单完整。匿名用户不伪装成某个 UID。

## 资源规则

私信里的图片（包括已经采集的粉丝团图片）必须保存。其他消息附件（语音、文件等）始终启用，不提供关闭开关；用户头像、主题封面、评论/动态配图在设置中分别启用，默认关闭；卡片封面归为封面，不当作必存私信图片。不采集视频、音频稿件的完整媒体流。

资源 ID 是规范化原地址的 MD5，保持现有附件去重方式；`checksum` 是下载后的文件内容 MD5，两者用途不同。账号之间分别保存。原地址保留供追溯，文件名使用资源 ID，不使用用户提供的文件名。

| 状态 | 含义 |
| --- | --- |
| not_requested | 未启用此类离线保存，保留链接，不算待处理 |
| pending | 必存或已启用，等待保存 |
| saved | 本地文件已保存；复制归档时保留完整资源包 |
| failed | 保存失败，保留错误，可在“处理资源/处理附件”中重试 |

每个资源完成后写入持久资源索引；中断不会清除消息采集断点。取消不将未完成的下载伪装为失败。资源采用匿名下载，不携带登录 Cookie；资源文件与账号、归档引用绑定，不能通过 URL 请求任意磁盘文件。相对路径、符号链接和目标文件碰撞均检查；不同内容不静默覆盖。


用户、主题补全结果进入正式归档，缓存只是加速层。缓存删除后，可继续读取已经归档的资料。网页使用独立的视图 DTO 和本地资源映射，JSON 下载入口 `/api/comments/archive.json` 提供规范归档；`/api/comments/data.json` 是评论页面视图，不作为持久文件格式。原图链接仍指向原地址。

## 任务报告与实时状态

`collection` 仅包含 `archived_total`、`reported_total`、`status`、`exhausted`，按当前分类或会话独立计算。`archived_total` 是此文件的 items 数，不是全部来源总数；报告数量及完整性无法确认时为 null。限定日期采集、点赞明细数量与通知计数不一致时，不能推断整份历史完整。单条通知的 `like_detail`、资源自身错误仍随数据保存。

每次消息开始／继续采集，另存 `<account_uid>/reports/<run_id>.json`，格式为 `bili-collection-report` v1。报告含范围、开始／结束时间、运行状态、数量汇总，以及异常 `type`、阶段、对象 ID、时间、重试次数和恢复情况。报告不写入单独导出的数据 JSON。离线资料包会将报告作为独立文件打包，导入后保留在原账号的 reports 目录。日志显示报告位置。参见 [报告 Schema](../schemas/collection-report-v1.schema.json) 和 [示例](examples/collection-report.json)。

读取现有 Bili Archive v1 文件时忽略旧 `collection.errors` 和整轮 scope，不把它们作为当前文件的完整性证据；正常保存时精简输出。不扫描或批量修改已有用户文件，仍使用 Bili Archive v1。

消息日志实时记录采集、等待、暂停、结束、完成及失败。通知流、点赞明细、会话列表与私信/粉丝团分页首次异常先保存可用内容和有效断点、延后当前目标；正常消息采集结束后、附件处理前统一补试，每个异常页仅一次。补试成功继续后续页，失败标记 NG，不原地循环。分页补试不增加额外等待，仍遵守配置请求间隔；风控和超时沿用每轮 60 秒等待，不消耗分页预算。

延后处理关闭当前等待弹窗，记录原因，不误报恢复。报告使用 `paging_deferred / paging_recovered / paging_ng / paging_skipped_ng / paging_unresolved` 统计，分页 issue 使用 `disposition: deferred / recovered / ng` 和 `paging_key` 标识；重试次数为 0 或 1。旧报告的 `deferred_likes` 字段仍可读取，新报告不再生成该点赞专用计数。点赞诊断保留用户数量与分页信息，其他来源记录游标/序列号；详见 [NG 报告示例](examples/collection-report-ng.json)。

存在未解决分页项时显示“部分完成”。NG 与延后补试状态保存在用户数据目录的 `ng/<library_id>/messages/v1/<uid>.paging.json`，与 `checkpoints/` 完全分开，账号与导出根目录隔离；清理普通任务不清理 NG。普通继续采集跳过 NG；“重试NG项（数量）”仅补采当前账号当前目录的失败目标，沿用原范围和断点，不覆盖普通任务断点，也不重新处理无关附件。暂停、结束或关闭保留未处理队列，被取消请求不记作失败；已保存内容不丢失。旧异常文本不自动转换为 NG。登录失效、解析或持久化失败仍停止任务并保留进度。

## 保存与读取边界

评论按 `<account_uid>/comments/<author_uid>/<category>/<oid>.json` 持续自动保存，每稿件一个文件。每页仅增量写入涉及的稿件，成功后才保存下一页断点；最终补存补全信息后才清理完成任务。写入失败保留原断点，不覆盖损坏或账号不匹配的文件。默认根目录为应用配置目录下的 `exports/archive-v1/`，自定义导出目录时写入该目录下的 `archive-v1/`。手动“导出 JSON”保留为另存合并文件。消息、附件和采集报告继续自动保存。

登录后保留自动读取当前账号现有新版归档，支持从账号菜单导入新版归档，不提供拆解或旧文件转换。旧格式文件仅提示不支持，不修改或删除。评论断点使用 v3，消息断点使用 v2（`checkpoints/<library_id>/messages/v2/`），旧断点不转换。

## 规范与验证

[JSON Schema](../schemas/archive-v1.schema.json) 定义持久格式；[示例目录](examples/archive-v1) 覆盖视频、动态、转发动态、专栏、音频、普通私信、粉丝团、回复通知、@通知和点赞通知。示例中的私信图片为 pending，没有伪造本地文件。

`node tests/archive-v1-contract.cjs` 使用无依赖检查器验证本仓库 Schema 使用的关键字、十类示例、引用与破坏性反例，并检查归档入口、设置及网页资源映射接线。Rust 测试源码覆盖往返读取、超大 ID、资源策略、取消/失败、资源包校验及分文件摘要隔离；按本次约定不执行 Rust 编译或 Rust 测试。

旧断点目录内的 `.paging.json` 在读取对应账号、导出目录时自动迁入独立 NG 目录；校验写入成功后才删除旧文件，已有新记录优先，迁移失败保留原文件。普通采集和 NG 补采的临时任务进度仍属于断点，NG 标记不随断点清理删除。


## 可携带资料库

归档正文仍为 Bili Archive v1，资料库额外使用 [library-v1.schema.json](../schemas/library-v1.schema.json) 描述身份。离线 ZIP 结构为：

```text
archive-v1/.library.json
archive-v1/<uid>/comments/...
archive-v1/<uid>/messages/...
archive-v1/<uid>/reports/...
archive-v1/resources/<uid>/...
checkpoints/<uid>/...
checkpoints/messages/v2/<uid>.json
checkpoints/messages/v2/<uid>.ng.json
ng/messages/v1/<uid>.paging.json
```

ZIP 内路径相对资料包根目录；本机导入时，状态写入用户数据目录中对应的 `<library_id>` 命名空间，JSON 不携带机器绝对路径。缺失资源保留为待处理状态；包内已有资源校验失败则终止导入。资料库内容可合并，已有状态冲突保留本机版本。账号目录和独立 JSON 导入用于读取归档；迁移断点与 NG 请使用完整资料包。资料包不包含机器配置和认证凭据。
