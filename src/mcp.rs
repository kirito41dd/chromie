use crate::{
    agent::AgentAccess,
    error::{Error, Result as AppResult},
    store::Store,
    types::{Correct, Forget, GetMemory, Recall, Remember, SpaceId},
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, InitializeRequestParams, InitializeResult, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
pub struct MemoryMcp {
    store: Store,
    space: SpaceId,
    bound_agent: Arc<Mutex<Option<Uuid>>>,
}

fn result(value: AppResult<Value>) -> CallToolResult {
    let mut response = match value {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => CallToolResult::structured_error(error.json()),
    };
    // SDK 默认同时复制到 text 与 structuredContent，这里只保留结构化结果。
    response.content.clear();
    response
}

impl MemoryMcp {
    fn identity(context: &RequestContext<RoleServer>) -> AppResult<AgentAccess> {
        context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<AgentAccess>())
            .cloned()
            .ok_or_else(Error::unauthorized)
    }

    async fn authorized_store(&self, context: &RequestContext<RoleServer>) -> AppResult<Store> {
        let access = Self::identity(context)?;
        {
            let mut owner = self.bound_agent.lock().map_err(|_| Error::unavailable())?;
            if owner.is_some_and(|id| id != access.agent_id()) {
                return Err(Error::unauthorized());
            }
            // SDK 的无状态请求没有 initialize 实例；首次已认证工具调用也能建立本实例身份。
            *owner = Some(access.agent_id());
        }
        Store::validate_agent(&self.store.db, self.space, &access, false).await?;
        Ok(self.store.for_agent(access))
    }
}

#[tool_router]
impl MemoryMcp {
    pub fn new(store: Store, space: SpaceId) -> Self {
        Self {
            store,
            space,
            bound_agent: Arc::new(Mutex::new(None)),
        }
    }

    #[tool(
        description = "列出当前空间的云端 Skill 目录，每页最多 10 项。根据作者的 name、description 选择流程，用 read_skill 读取选中项。next_cursor 非空时可继续翻页。",
        annotations(read_only_hint = true)
    )]
    async fn list_skills(
        &self,
        Parameters(q): Parameters<crate::skill_mcp::ListSkills>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        result(
            async {
                self.authorized_store(&ctx)
                    .await?
                    .skill_directory(self.space, q.cursor)
                    .await
            }
            .await,
        )
    }
    #[tool(
        description = "按路径读云端 Skill，默认读 SKILL.md；后续文件带 expected_revision。版本变化请重读入口。执行沿用当前宿主授权。",
        annotations(read_only_hint = true)
    )]
    async fn read_skill(
        &self,
        Parameters(q): Parameters<crate::skill_mcp::Read>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        result(
            async {
                crate::skill_mcp::read(&self.authorized_store(&ctx).await?, self.space, q).await
            }
            .await,
        )
    }
    #[tool(
        description = "记录 Skill 的实际使用结果。used 指实际用过的修订，corrected 指发现需要纠正的旧修订，verified 指已验证通过的修订。每个观察用独立 request_id，同一观察重试复用 ID。修订未知可省略。",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn report_skill_use(
        &self,
        Parameters(q): Parameters<crate::skill_mcp::Report>,
        ctx: RequestContext<RoleServer>,
    ) -> CallToolResult {
        result(
            async {
                self.authorized_store(&ctx)
                    .await?
                    .skill_report(
                        self.space,
                        q.skill_id,
                        q.revision,
                        q.request_id,
                        &q.outcome,
                        q.reason.as_deref(),
                    )
                    .await
            }
            .await,
        )
    }
    #[tool(
        description = "分页查看当前空间的简短主题导航，了解已保存哪些领域及入口；不调用模型。按需 get_memory 阅读主题，再用 get_memory_links 沿引用读取。project_key 只填已知值，返回该项目和空间通用主题。",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_navigation(
        &self,
        Parameters(input): Parameters<crate::knowledge::Navigation>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.navigation(self.space, input).await)
    }

    #[tool(
        description = "分页读取记忆的结构化引用；incoming=true 查看当前仍引用它的记忆，默认查看它引用的内容。只返回简短入口，按需 get_memory 阅读，避免展开整张图。derived_from 固定依据修订，needs_review 表示依据已有变化；关联不等于直接命中或事实已验证。",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_memory_links(
        &self,
        Parameters(input): Parameters<crate::knowledge::Neighbors>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.memory_neighbors(self.space, input).await)
    }

    #[tool(
        description = "首次检索前读取当前空间的检索能力，按 mode 选择关键词或自然问法；可按 cache_ttl_seconds 短期缓存。返回语义状态和配置修订，不调用模型、不探活、不返回连接或密钥。快照不保证下一次请求成功，以 recall 的实际路径为准；配置变化或降级时刷新。",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_capabilities(&self, context: RequestContext<RoleServer>) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(
            store
                .capabilities(self.space)
                .await
                .and_then(|v| serde_json::to_value(v).map_err(|_| Error::unavailable())),
        )
    }

    #[tool(
        description = "用户要保存密码、记住账号密码或提供密钥时，先用 topic=secret 核对权限与指南。原文未提供时引导隐藏输入；已经主动提供且明确要求代存时，按指南在本地直接代存，不要求重复输入。此工具只返回指南与权限，不接收原文。也支持 setup、capture、diagnostics；不返回密钥，不执行本机安装。",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_setup_guide(
        &self,
        Parameters(input): Parameters<crate::setup_guides::Request>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        if input.purpose.as_ref().is_some_and(|v| v.len() > 512) {
            return result(Err(Error::invalid(
                "用途说明最多 512 字节，不能填写凭据原文",
            )));
        }
        if input
            .known_guide_version
            .as_ref()
            .is_some_and(|v| v.len() > 128)
        {
            return result(Err(Error::invalid("指南版本超过长度限制")));
        }
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        let space = match store.space_key(self.space).await {
            Ok(space) => space,
            Err(error) => return result(Err(error)),
        };
        let access = match Self::identity(&context) {
            Ok(access) => access,
            Err(error) => return result(Err(error)),
        };
        let secret_access_enabled =
            match Store::validate_agent(&store.db, self.space, &access, true).await {
                Ok(()) => true,
                Err(error) if error.code == "secret_access_denied" => false,
                Err(error) => return result(Err(error)),
            };
        let mut reply =
            crate::setup_guides::payload(input.topic, input.known_guide_version.as_deref());
        reply["space"] = space.as_str().into();
        reply["agent_id"] = access.agent_id().to_string().into();
        reply["secret_access_enabled"] = secret_access_enabled.into();
        reply["access_path"] = format!("/spaces/{}/access", space.as_str()).into();
        if reply["guide_cached"] != true {
            reply["secret_ref_example"] = format!("secret://{}/记忆UUID", space.as_str()).into();
            reply["plugin_download"] = "/integrations/codex/chromie-codex.zip".into();
            reply["capture_download"] = "/integrations/chromie-client.zip".into();
            reply["installer"] = "/install.sh".into();
            reply["setup_command"] = format!(
                "chromie init --url <服务Origin> --space {} --agent codex",
                space.as_str()
            )
            .into();
        }
        result(Ok(reply))
    }

    #[tool(
        description = "首次检索前用 get_capabilities 了解当前模式。找回当前空间已保存的记忆，不搜索原始对话；关键词模式用项目名、专名或关键术语，混合模式可用自然问法。找已有凭据引用时设 prefer_secret_refs=true，并带上服务或环境；排障和查方法沿用默认排序。只使用已知 project_key，核对命中对象和范围；分数表示相关，不保证包含答案，缺少事实时说明未记录。查看 retrieval_mode、semantic_available、semantic_reason 和 retrieval_revision 判断实际路径；降级后可用关键词补查一次。content_truncated 为真且片段不足以判断时，按 memory_id 用 get_memory 展开。返回 recall_id，可用 feedback_recall 反馈实际判断过且有帮助的返回条目。",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn recall(
        &self,
        Parameters(input): Parameters<Recall>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.recall(self.space, input).await)
    }

    #[tool(
        description = "批量反馈当前任务中已判断的召回结果。使用 recall_id，只填实际返回且评价过的记忆 ID；useful_memory_ids 是 evaluated_memory_ids 中对本次任务有帮助的子集，未评价不要填。可用 negative_reasons 给已评价且无用的条目标注范围不符、结果类型不符或缺少细节，每项只选一个，不能判断时省略。一次最多 20 次召回，每次 10 条。相同反馈重试不重复计数，矛盾反馈冲突；只接受原召回 Agent。usage 可独立反馈真正使用过的条目：succeeded 成功、content_outdated 内容过时、environment_blocked 环境受限、unverified 未验证；reason 只写公开简短原因，不含日志或凭据。服务固定到原召回修订。未验证可补充最终结果，最终结果冲突不能覆盖，新尝试用新的召回。只是 Agent 报告，不宣称独立验证。",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn feedback_recall(
        &self,
        Parameters(input): Parameters<crate::recall_usage::FeedbackRecall>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.feedback_recall(self.space, input).await)
    }

    #[tool(
        description = "memory_id／event_id 填 UUID；Secret URI 须先核对空间并提取 UUID。读取内容、修订或有限证据；Secret 当前视图返回公开说明、完整 contract 与修订，自动取用优先用 CLI secret exec，按契约解析并指定同一修订，unknown 先补契约。历史不提供当前契约，不返回原文或密文。",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn get_memory(
        &self,
        Parameters(input): Parameters<GetMemory>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.get(self.space, input).await)
    }

    #[tool(
        description = "保存可跨对话复用的明确结论，附真实消息来源。memory_type 可选 note／topic；links 使用真实记忆 ID，派生依据填写已读取的当前修订。正文组织由 Agent 判断，禁止传入密码或密钥原文。request_id 使用 UUID，重试复用。",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn remember(
        &self,
        Parameters(input): Parameters<Remember>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.remember(self.space, input).await)
    }

    #[tool(
        description = "按已读取修订号更正记忆或适用范围，说明理由；冲突须重新读取，不能静默覆盖。收到 Secret 录入回执后，先 get_memory 核对，仅在缺少已知背景时补全同一条公开说明（最多 256 UTF-8 字节）或范围，不另建重复记忆、不挂来源、不读改原文。说明已完整则不写入。",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn correct_memory(
        &self,
        Parameters(input): Parameters<Correct>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.correct(self.space, input).await)
    }

    #[tool(
        description = "按用户明确遗忘意图删除已定位的记忆或原始事件。共享来源暂不能切分时整体清理并停止提供依赖记忆。",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn forget_memory(
        &self,
        Parameters(input): Parameters<Forget>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let store = match self.authorized_store(&context).await {
            Ok(store) => store,
            Err(error) => return result(Err(error)),
        };
        result(store.forget(self.space, input).await)
    }
}

#[tool_handler]
impl ServerHandler for MemoryMcp {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let access = Self::identity(&context)
            .map_err(|_| ErrorData::invalid_request("接入鉴权失败", None))?;
        *self
            .bound_agent
            .lock()
            .map_err(|_| ErrorData::internal_error("会话不可用", None))? = Some(access.agent_id());
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn on_custom_request(
        &self,
        request: rmcp::model::CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CustomResult, ErrorData> {
        let store = self
            .authorized_store(&context)
            .await
            .map_err(crate::skill_mcp::protocol_error)?;
        crate::skill_mcp::custom(&store, self.space, request).await
    }
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        let store = self
            .authorized_store(&context)
            .await
            .map_err(crate::skill_mcp::protocol_error)?;
        crate::skill_mcp::resource(&store, self.space, &request.uri)
            .await
            .map_err(crate::skill_mcp::protocol_error)
    }
    fn get_info(&self) -> ServerConfig {
        let mut caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        caps.extensions = Some(
            [(
                "io.modelcontextprotocol/skills".to_owned(),
                Default::default(),
            )]
            .into(),
        );
        ServerConfig::new(caps).with_instructions(include_str!("mcp_instructions.md"))
    }
}
