use chromie::{
    agent::{CreateAgent, KeyKind, UpdateAgent},
    secret_crypto::SecretCipher,
    server_key::ServerKey,
    store::Store,
    types::SpaceId,
};
use p256::{
    SecretKey,
    pkcs8::{EncodePrivateKey, LineEnding},
};
use uuid::Uuid;

pub fn root() -> ServerKey {
    // 与 Secret 集成测试共用公开的虚构根密钥；只可用于隔离测试库。
    let pem = SecretKey::from_slice(&[41; 32])
        .unwrap()
        .to_pkcs8_pem(LineEnding::LF)
        .unwrap();
    ServerKey::from_pem(&pem).unwrap()
}

pub async fn enable(store: Store) -> Store {
    store
        .enable_secrets(SecretCipher::from_key(&root()).unwrap())
        .await
        .unwrap()
}

pub async fn grant(store: &Store, space: SpaceId, secrets: bool) -> (Uuid, String, String) {
    let agent = store
        .create_agent(
            space,
            CreateAgent {
                agent_name: format!("虚构测试接入-{}", Uuid::now_v7()),
            },
        )
        .await
        .unwrap();
    if secrets {
        store
            .update_agent(
                space,
                agent.agent_id,
                UpdateAgent {
                    agent_name: None,
                    enabled: None,
                    allow_secrets: Some(true),
                },
            )
            .await
            .unwrap();
    }
    (
        agent.agent_id,
        store
            .reveal_agent_key(space, agent.agent_id, KeyKind::Mcp)
            .await
            .unwrap()
            .to_string(),
        store
            .reveal_agent_key(space, agent.agent_id, KeyKind::Hook)
            .await
            .unwrap()
            .to_string(),
    )
}
