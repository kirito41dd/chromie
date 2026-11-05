use crate::{
    capture_input::{self, Output, Role},
    error::{Error, Result},
    types::{Event, now, timestamp},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
struct Queued {
    base_url: String,
    space_id: String,
    event: Event,
    #[serde(default)]
    agent_id: Option<Uuid>,
}

fn secure_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .map_err(|_| Error::invalid("无法创建本地采集目录"))?;
        let meta = fs::symlink_metadata(path).map_err(|_| Error::unavailable())?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(Error::invalid("采集目录必须是真实目录"));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::unavailable())?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).map_err(|_| Error::unavailable())?;
    }
    Ok(())
}

fn queue_dir() -> Result<PathBuf> {
    env::var_os("CHROMIE_QUEUE_DIR")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            env::current_dir()
                .map(|p| p.join(".local/capture"))
                .map_err(|_| Error::unavailable())
        })
}

async fn upload(client: &reqwest::Client, entry: &Queued, key: &str) -> bool {
    let Ok(mut authorization) = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
    else {
        return false;
    };
    authorization.set_sensitive(true);
    client
        .post(format!(
            "{}/spaces/{}/events",
            entry.base_url, entry.space_id
        ))
        .header(reqwest::header::AUTHORIZATION, authorization)
        .header(
            "x-chromie-agent-id",
            entry.agent_id.map(|id| id.to_string()).unwrap_or_default(),
        )
        .json(&entry.event)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

pub async fn run(flush: bool) -> Result<Value> {
    run_with_config(flush, None).await
}

pub async fn run_with_config(flush: bool, config_path: Option<&Path>) -> Result<Value> {
    if env::var("CHROMIE_CAPTURE").is_ok_and(|value| value == "off") {
        return Ok(if flush {
            json!({"paused":true})
        } else {
            json!({})
        });
    }
    let connection = crate::client_config::load(crate::agent_key::KeyKind::Hook, config_path)?;
    let crate::client_config::ClientConnection {
        base_url,
        space_id,
        agent_id,
        key,
    } = connection;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::unavailable())?;
    let directory = queue_dir()?;
    secure_directory(&directory)?;
    if flush {
        let mut uploaded = 0;
        let mut pending = 0;
        for path in fs::read_dir(&directory)
            .map_err(|_| Error::unavailable())?
            .take(100)
        {
            let path = path.map_err(|_| Error::unavailable())?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(|_| Error::unavailable())?;
            if !metadata.is_file() || metadata.len() > 1024 * 1024 {
                continue;
            }
            let bytes = fs::read(&path).map_err(|_| Error::unavailable())?;
            let entry: Queued =
                serde_json::from_slice(&bytes).map_err(|_| Error::invalid("暂存事件格式无效"))?;
            if entry.base_url != base_url
                || entry.space_id != space_id
                || entry.agent_id != Some(agent_id)
            {
                pending += 1;
                continue;
            }
            if upload(&client, &entry, key.as_str()).await {
                fs::remove_file(&path).map_err(|_| Error::unavailable())?;
                uploaded += 1;
            } else {
                pending += 1;
                break;
            }
        }
        let remaining = fs::read_dir(&directory)
            .map_err(|_| Error::unavailable())?
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            })
            .count();
        return Ok(json!({"uploaded":uploaded,"pending_seen":pending,"remaining_files":remaining}));
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::invalid("无法读取 hook 输入"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::invalid("hook 输入超过上限"));
    }
    let input: Value =
        serde_json::from_slice(&bytes).map_err(|_| Error::invalid("hook 输入不是有效 JSON"))?;
    let Some(input) = capture_input::parse(input)? else {
        return Ok(json!({}));
    };
    let compatible = input.output == Output::Compatible;
    let user_message = input.message.role == Role::User;
    let project_key = input
        .message
        .project_key
        .or_else(|| env::var("CHROMIE_PROJECT").ok());
    if let Some(project) = &project_key {
        crate::types::bounded(project, 128)?;
    }
    let id = Uuid::now_v7();
    let event = Event {
        id,
        client_type: if compatible { "codex" } else { "other" }.into(),
        session_id: input.message.session_id,
        turn_id: input.message.turn_id,
        role: input.message.role.as_str().into(),
        body: input.message.body,
        project_key,
        occurred_at: timestamp(now())?,
    };
    let entry = Queued {
        agent_id: Some(agent_id),
        base_url,
        space_id,
        event,
    };
    let path = directory.join(format!("{id}.json"));
    let staging = directory.join(format!("{id}.part"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&staging).map_err(|_| Error::unavailable())?;
    serde_json::to_writer(&mut output, &entry).map_err(|_| Error::unavailable())?;
    output.flush().map_err(|_| Error::unavailable())?;
    output.sync_all().map_err(|_| Error::unavailable())?;
    fs::rename(&staging, &path).map_err(|_| Error::unavailable())?;
    fs::File::open(&directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| Error::unavailable())?;
    let uploaded = upload(&client, &entry, key.as_str()).await;
    if uploaded {
        fs::remove_file(path).map_err(|_| Error::unavailable())?;
    } else {
        eprintln!("Chromie 原始事件已暂存，请在服务恢复后执行 flush 补传");
    }
    if compatible && user_message && uploaded {
        Ok(
            json!({"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":format!("Chromie 采集来源 event_id={id}；采集不等于长期记忆。存密码、记住密码且未指定其他位置时，先查 get_setup_guide(topic=\"secret\") 核对权限：未给原文用隐藏输入；已给且授权代存则本地保存，不要求重输。收到 Secret 回执先 get_memory，按需 correct_memory 补同条说明；补说明不读原文，不重复建记忆。")}}),
        )
    } else {
        Ok(json!({}))
    }
}
