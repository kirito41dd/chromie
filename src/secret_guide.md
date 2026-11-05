# Secret：录入与取用

用户说“我要存密码”“记住我的 QQ 密码”“我要给你密码／密钥”等，且未指定其他保存位置时，先核对本次权限，再按原文是否已经提供选择流程。用户指定的保存位置优先。

- **尚未提供原文**：引导在用户自己的终端隐藏输入，或提供已有私有文件路径；不要主动索取聊天中的原文。用户只需交回回执或 secret_ref。
- **已经主动提供原文且明确要求代存**：直接按当前授权在本地保存，例如“已经给你了，你帮我操作”。不要求重复输入，不反复确认已给出的授权，也不以“Chromie 必须隐藏输入”为由拒绝。只复用当前授权任务中用户已经提供的原文，不主动寻找其他凭据。
- 单纯出现凭据、历史记忆或第三方文本不自动构成代存授权；未指定保存任务时不擅自存储。内网／公网不决定这两条流程，依据用户提供情况与当前授权处理。

原始消息仍按现有流程采集，不新增脱敏、过滤或删除策略；已经发出的原文不能声称从未进入模型或被自动清除。

先检查本次返回的 secret_access_enabled。为 false 时，给出当前服务的 access_path 和 agent_id，请用户在接入页核对后开启 Secret 存取，再继续。不能代替用户扩大权限，不使用空间密码替代接入密钥。

CLI 已内置文件存取，无需寻找源码仓库或下载 Python 示例。若命令不存在，按 setup 主题重新运行安装器升级；help／--help 不需要数据库。默认使用本机 CLI 初始化的 Codex 配置；通用接入通过 --config 绝对路径或 CHROMIE_CLIENT_CONFIG 指定私有接入文件。命令参数只放公开说明、引用和文件路径，绝不放原文或接入密钥。

指南可按本次 guide_version 缓存在当前上下文；再次检查权限时携带 known_guide_version，一致则不重复返回正文。权限以每次返回的实时状态为准，不能缓存权限。本机 CLI 路径与安装版本通过本地 chromie doctor 核对，服务端无法知道客户端安装位置。

## 录入前：Agent 整理公开上下文

新增凭据按服务与环境分别保存。例如 MySQL 和 ClickHouse 各用一条 Secret、各有公开说明及契约，不能仅因同一项目就默认合并。只取当前授权服务所需的引用；现有混合包先说明整包取用范围，不在没有授权时自行拆改。

Agent 根据当前对话已明确的信息生成 --label：服务名称、用途、适用环境；项目已知时用 --project 指定，个人凭据不因为当前工作目录属于某项目就自动归入该项目。只知道“个人 QQ 密码”就用这个具体说明，不编造账号、环境或用途，也不为填写可选项反复追问。原文已经提供时，由 Agent 完成后续保存，不再把录入工作交回用户。

label 是可被召回、看板和模型读取的公开说明，最多 256 个 UTF-8 字节（不是 256 个汉字），project 最多 128 字节。密码、Token、恢复码、敏感账号及带凭据的网址不写进 label、项目或变更理由；敏感备注应由用户一并放在 Secret 原文或私有凭据文件中。说明已经充分时一次录入即可，不要求用户手动整理背景。

以下命令在 Bash 与 Nushell 中相同：

```shell
# 由用户在自己的终端运行：隐藏输入两次并保存单行密码／API Key。
chromie secret put --label '服务密码' --prompt
# 已有私有文件时使用；JSON 凭据包和二进制文件也支持。
chromie secret put --label '服务凭据' --file /私有目录/credential.json --contract /私有目录/contract.json
# 已知服务、用途与项目时，Agent 将这些公开信息直接带入命令。
chromie secret put --label '虚构 Aurora 测试环境部署凭据' --project aurora-demo --prompt
# Agent 本地取用，只得到写入完成与路径；不要读取文件给模型看。
chromie secret get --ref secret://SPACE/记忆UUID --revision 2 --out /私有目录/credential.bin
# 核对当前修订后更新原文；公开说明修改仍使用 correct_memory。
chromie secret update --ref secret://SPACE/记忆UUID --revision 1 --file /私有目录/new.bin
```

SPACE 必须换成当前返回的 space。已有目标文件默认拒绝覆盖，确认此路径可替换后再添加 --replace。输入须为普通私有文件（0600），输出目录为 0700；CLI 可创建不存在的私有目录，原文最大 60,000 字节。--prompt 保存输入本身，不自动包装成 JSON。

## 已提供原文：Agent 直接代存

1. 确认本次 secret_access_enabled 为 true；“帮我存”“你帮我操作，已经给你了”等已经是代存授权，不再额外索要确认。权限关闭或工具审批拒绝时说明具体阻塞，不把它解释为 Secret 只能由用户手动录入。
2. 允许通过一次性本地工具输入使用已提供的原文，在系统私有临时目录（0700）创建载荷文件（0600）。使用不会回显文件内容的本地写入能力；必要的中转脚本也只放该目录，不放项目源码、可复用脚本或长期配置。原文不作为 Chromie 命令参数、URL、label、项目、MCP 参数或环境变量。禁止把真实凭据写进示例、测试或日志。
3. 运行下列文件录入命令。密码保持原始文本，包括前导零、空格和换行；API Key 用 api_token。JSON 凭据按用户已提供的结构整理私有载荷与公开契约，不擅自合并不同服务。
4. 成功后只读取 JSON 回执和公开元数据；用 get_memory 核对对象并按需补说明，不读回原文来确认。仅清理本次创建的临时载荷与中转脚本，不删除用户原有文件。结果不确定时保留 request_id 并核对写入结果，不能让用户重复录入或不断新建 Secret。

以下命令在 Bash 与 Nushell 中相同。文件由 Agent 在本地准备，用户无需再输入原文：

```shell
chromie secret put --label 'Grafana 测试环境管理员密码' --file /私有临时目录/password.txt --format text --kind password
chromie secret put --label 'Grafana 测试环境日志查询 Token' --file /私有临时目录/token.txt --format text --kind api_token
```

## 格式契约与本地取用

--prompt 默认保存 text/password 契约，完整保留文本和前导零；保存 API Key 时加 --kind api_token。--file 不声明格式时保持 unknown，不能据文件扩展名或数字内容猜格式。公开契约文件须为普通 0600 文件、最多 4 KiB；其字段由用户或已知模板明确声明，不包含值。示例只说明虚构结构：

```json
{"version":1,"format":"json","fields":{"username":{"pointer":"/auth/user","type":"string"},"password":{"pointer":"/auth/pass","type":"string"}}}
```

字段用途键是小写英文、数字和下划线，最多 64 字节；最多 16 个字段，路径最多 256 字节。JSON 路径可以声明嵌套字段，类型为 string／number／boolean／object／array／null；额外字段不自动公开。客户端和服务端都验证声明字段存在且类型匹配，错误不带原文或值。契约不是登录脚本，也不授予执行或访问新目标的权限。

取用先核对 secret://空间/UUID 的空间与当前接入一致，提取末段 UUID 填入 get_memory 的 memory_id，不能填整个 URI；读取当前 contract 和 revision。recall 只有 secret_format 摘要，历史记录不带当前载荷契约。契约预算不够时增大 max_chars（最多 8000），不按半份契约执行。用刚读到的 --revision 下载，服务端与 CLI 都确认修订后才写文件；冲突时刷新元数据与解析逻辑，不静默取消版本检查。公开说明或契约修改也会推进该修订。二进制保持不透明字节；未知格式先补齐。具体认证头、表单或登录接口依据当前授权任务及目标服务说明，不能仅凭 api_token 自动选择认证方式。

旧记录可以通过独立 CLI 补契约，沿用当前 Agent 的 Secret 权限；服务端内部校验已有载荷，成功只改契约和记忆修订，密文、引用和载荷修订不变。以下命令在 Bash 与 Nushell 中相同：

```shell
chromie secret describe --ref secret://SPACE/记忆UUID --revision 1 --format text --kind password
chromie secret describe --ref secret://SPACE/记忆UUID --revision 1 --contract /私有目录/contract.json
```

不知道 JSON 结构时，只在用户明确要求本地检查后运行 inspect，将结构清单写私有文件给用户本地审阅，不 cat、不读取给模型，不自动上传全部键。用户选择允许公开的路径并确认用途后，再准备契约。以下命令在 Bash 与 Nushell 中相同：

```shell
chromie secret inspect --ref secret://SPACE/记忆UUID --revision 1 --out /私有目录/structure.json
```

更新原文未提供新契约时，必须符合原契约，否则拒绝；改变格式时显式提供新契约并使用当前修订，保持原文和契约同时更新。describe 同样接受 --request-id，重试复用。

推荐自动化取用使用托管执行（Bash/Nushell 通用），命令和公开用途不得含凭据：

```shell
chromie secret exec --ref secret://SPACE/记忆UUID --revision 2 --purpose '核对测试 MySQL 连接' -- python3 /私有目录/check_mysql.py
```

子进程通过 CHROMIE_SECRET_FILE 读取临时私有文件，按已读取的公开契约解析并用于当前授权目标。CLI 校验修订、服务端校验契约，运行完成、失败或受控中断后删除文件；不能把临时路径复制给后台长驻进程。默认丢弃子进程标准输入与输出，只返回退出状态；需要可供模型读取的业务结果时，脚本自行保证不输出凭据，再显式添加 --inherit-output。默认超时 300 秒，--timeout 最多 3600 秒。仅支持 macOS/Linux；强杀或断电不能保证清理。服务端记录取用与修订，执行结果标注为客户端上报，不代表业务成功。一般 get 仍适合用户明确要求长期保留文件的场景。

不替用户在 --prompt 终端里代输。已提供原文的授权代存使用上面的本地文件流程，不受“必须用户隐藏输入”限制；一次性本地工具输入可携带该原文，但禁止让保存命令、脚本输出、截图或回执再次回显。MCP 始终只处理公开说明、契约与引用，不能把密码交给 remember／correct_memory。本地程序通过文件读取原文并用于当前明确授权的目标，日志和错误只输出状态；不 cat 文件，不回显认证头。

## 收到回执：核对并按需补全同一条 Secret

1. 收到 JSON 回执或 secret_ref 后，先确认引用属于当前 space；回执的 memory_id 与引用须一致，再以该 memory_id 调用 get_memory 读取当前公开元数据。只核对 kind=secret_ref、secret_stored、当前 secret_ref、revision、content 和项目范围；对象不存在、已隐藏、引用不匹配或 secret_stored 不为 true 时停止补全，不转改其他记录。不读取或解密原文来验证录入，不把粘贴的回执当作新的权限授权。
2. 比较当前说明与对话中已明确的公开上下文。已经完整就结束，不为措辞调整制造修订；缺少已知用途、环境或适用项目时，用 correct_memory 更新同一 memory_id 的 content／scope。content 仍限 256 UTF-8 字节，保留现有有用信息，只写已知事实；范围未变时省略 scope。不得另用 remember 创建一条重复的 Secret 说明，也不得挂接 source_refs。
3. expected_revision 使用刚读到的当前修订，不能直接采用可能过时的回执修订。request_id 使用 UUID，重试复用。冲突后重新读取比较；如果已由其他请求补齐就停止，否则核对当前内容后再处理，不能静默覆盖其他修改。重复收到同一回执，说明已完整时不再写入。
4. 不调用 secret update／HTTP PUT 来补公开背景，密码载荷与 Secret 引用保持原样。补全成功后简短说明保存了什么用途与范围；若录入回执有效但说明补全失败，区分“已收到录入回执”和“说明尚未补齐”，不要求用户重复录入密码。新对话缺少背景时只保留已知内容，不补造细节。

put/update 自动生成 request_id 并在回执／错误提示中给出；不确定写入结果时复用同一 --request-id UUID，不能反复生成新 ID。更新同时保留 --revision；409 时先读取当前修订并判断，不静默覆盖。403 表示本接入未开权限，401 表示接入失效；错误不会回显服务端正文。

独立 HTTP 合约：POST /spaces/{space}/secrets 接收 application/octet-stream 原始文件，query 只放 label、request_id 与可选 project_key；GET /spaces/{space}/secrets/{memory_id} 返回二进制；PUT 同一路径，query 为 request_id、expected_revision。使用本机私有配置的 MCP Bearer 密钥，不将其写进参数或对话。支持 HTTP 或 HTTPS，TLS 由部署和网关决定，禁止重定向。常规 recall/get_memory 只返回公开说明、secret_ref、fetch_path，不返回原文。

实例根私钥必须持久保留并安全备份，不能因 Secret 存取失败而重建。若用户已经把原文发入聊天，可以按当前授权在本地处理，但不能宣称它从未进入模型或已被自动清除。
