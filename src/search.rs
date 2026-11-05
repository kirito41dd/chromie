use crate::{
    error::{Error, Result},
    types::SpaceId,
};
use qdrant_client::{
    Payload, Qdrant,
    qdrant::{
        Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, DeletePointsBuilder,
        Distance, FieldType, Filter, PointStruct, Query, QueryPointsBuilder,
        TextIndexParamsBuilder, TokenizerType, UpsertPointsBuilder, VectorParamsBuilder,
    },
};
use serde_json::json;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use uuid::Uuid;

pub(crate) const TEXT_FIELD: &str = "content";
pub(crate) const CANDIDATE_LIMIT: u64 = 30;

pub(crate) struct SearchHit {
    pub id: Uuid,
    pub revision: i32,
    pub score: f32,
}

pub(crate) struct SearchReport {
    pub hits: Vec<SearchHit>,
    pub candidate_count: usize,
    pub server_ms: Option<f64>,
}

#[derive(Clone)]
pub struct Search {
    client: Qdrant,
    ready: Arc<Mutex<HashSet<String>>>,
}

impl Search {
    fn vector_collection(space: SpaceId, generation: Uuid) -> String {
        format!(
            "chromie_vec_{}_{}",
            space.as_uuid().simple(),
            generation.simple()
        )
    }

    pub(crate) async fn upsert_vector(
        &self,
        space: SpaceId,
        generation: Uuid,
        id: Uuid,
        revision: i32,
        project: Option<&str>,
        vector: Vec<f32>,
    ) -> Result<()> {
        let name = Self::vector_collection(space, generation);
        if !self
            .client
            .collection_exists(&name)
            .await
            .map_err(|_| Error::unavailable())?
        {
            let created = self
                .client
                .create_collection(CreateCollectionBuilder::new(&name).vectors_config(
                    VectorParamsBuilder::new(vector.len() as u64, Distance::Cosine),
                ))
                .await;
            if created.is_err()
                && !self
                    .client
                    .collection_exists(&name)
                    .await
                    .map_err(|_| Error::unavailable())?
            {
                return Err(Error::unavailable());
            }
        }
        let payload=Payload::try_from(json!({"space_id":space.as_uuid(),"memory_id":id,"revision":revision,"project_key":project})).map_err(|_|Error::unavailable())?;
        self.client
            .upsert_points(
                UpsertPointsBuilder::new(
                    name,
                    vec![PointStruct::new(id.to_string(), vector, payload)],
                )
                .wait(true),
            )
            .await
            .map_err(|_| Error::unavailable())?;
        Ok(())
    }

    pub(crate) async fn delete_vector(
        &self,
        space: SpaceId,
        generation: Uuid,
        id: Uuid,
    ) -> Result<()> {
        let name = Self::vector_collection(space, generation);
        if self
            .client
            .collection_exists(&name)
            .await
            .map_err(|_| Error::unavailable())?
        {
            self.client
                .delete_points(
                    DeletePointsBuilder::new(name)
                        .points(vec![qdrant_client::qdrant::PointId::from(id.to_string())])
                        .wait(true),
                )
                .await
                .map_err(|_| Error::unavailable())?;
        }
        Ok(())
    }

    pub(crate) async fn delete_vector_generation(
        &self,
        space: SpaceId,
        generation: Uuid,
    ) -> Result<()> {
        let name = Self::vector_collection(space, generation);
        if self
            .client
            .collection_exists(&name)
            .await
            .map_err(|_| Error::unavailable())?
        {
            self.client
                .delete_collection(name)
                .await
                .map_err(|_| Error::unavailable())?;
        }
        Ok(())
    }

    pub(crate) async fn query_vectors(
        &self,
        space: SpaceId,
        generation: Uuid,
        vector: Vec<f32>,
        project: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let mut request = QueryPointsBuilder::new(Self::vector_collection(space, generation))
            .query(Query::new_nearest(vector))
            .limit(CANDIDATE_LIMIT)
            .with_payload(true);
        if let Some(project) = project {
            request = request.filter(Filter::should([
                Condition::matches("project_key", project.to_owned()),
                Condition::is_empty("project_key"),
            ]));
        }
        let response = self
            .client
            .query(request)
            .await
            .map_err(|_| Error::unavailable())?;
        let mut hits = Vec::new();
        for point in response.result {
            if !point.score.is_finite() || point.score <= 0.0 {
                continue;
            }
            if let (Some(id), Some(revision), Some(owner)) = (
                point.payload.get("memory_id").and_then(|v| v.as_str()),
                point.payload.get("revision").and_then(|v| v.as_integer()),
                point.payload.get("space_id").and_then(|v| v.as_str()),
            ) && owner.as_str() == space.as_uuid().to_string()
                && let (Ok(id), Ok(revision)) = (Uuid::parse_str(id), i32::try_from(revision))
            {
                hits.push(SearchHit {
                    id,
                    revision,
                    score: point.score,
                });
            }
        }
        Ok(hits)
    }

    pub async fn available(&self) -> bool {
        self.client.health_check().await.is_ok()
    }

    /// 对 BM25-only 候选补充同一次查询的余弦分数，不进行第二次 embedding。
    pub(crate) async fn score_vectors(
        &self,
        space: SpaceId,
        generation: Uuid,
        vector: Vec<f32>,
        ids: &[Uuid],
        project: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        if ids.len() > CANDIDATE_LIMIT as usize {
            return Err(Error::unavailable());
        }
        let mut conditions = vec![Condition::has_id(ids.iter().map(Uuid::to_string))];
        if let Some(project) = project {
            conditions.push(
                Filter::should([
                    Condition::matches("project_key", project.to_owned()),
                    Condition::is_empty("project_key"),
                ])
                .into(),
            );
        }
        let response = self
            .client
            .query(
                QueryPointsBuilder::new(Self::vector_collection(space, generation))
                    .query(Query::new_nearest(vector))
                    .limit(ids.len() as u64)
                    .filter(Filter::must(conditions))
                    .with_payload(true)
                    .params(qdrant_client::qdrant::SearchParams {
                        exact: Some(true),
                        ..Default::default()
                    }),
            )
            .await
            .map_err(|_| Error::unavailable())?;
        let mut hits = Vec::new();
        for point in response.result {
            if !point.score.is_finite() {
                continue;
            }
            if let (Some(id), Some(revision), Some(owner)) = (
                point.payload.get("memory_id").and_then(|v| v.as_str()),
                point.payload.get("revision").and_then(|v| v.as_integer()),
                point.payload.get("space_id").and_then(|v| v.as_str()),
            ) && owner.as_str() == space.as_uuid().to_string()
                && let (Ok(id), Ok(revision)) = (Uuid::parse_str(id), i32::try_from(revision))
                && ids.contains(&id)
            {
                hits.push(SearchHit {
                    id,
                    revision,
                    score: point.score,
                });
            }
        }
        Ok(hits)
    }

    pub fn new(url: &str, key: Option<String>) -> Result<Self> {
        let client = Qdrant::from_url(url)
            .api_key(key)
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| Error::unavailable())?;
        Ok(Self {
            client,
            ready: Arc::new(Mutex::new(HashSet::new())),
        })
    }
    fn collection(space: SpaceId) -> String {
        format!("chromie_{}", space.as_uuid().simple())
    }

    pub async fn ensure(&self, space: SpaceId) -> Result<()> {
        let name = Self::collection(space);
        let mut ready = self.ready.lock().await;
        if ready.contains(&name) {
            return Ok(());
        }
        // 个人部署的空间数量很小，独立 collection 同时隔离候选和 BM25 统计。
        let exists = self
            .client
            .collection_exists(name.clone())
            .await
            .map_err(|_| Error::unavailable())?;
        if !exists {
            self.client
                .create_collection(CreateCollectionBuilder::new(name.clone()))
                .await
                .map_err(|_| Error::unavailable())?;
        }
        self.client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(name.clone(), TEXT_FIELD, FieldType::Text)
                    .field_index_params(
                        TextIndexParamsBuilder::new(TokenizerType::Multilingual).bm25_scoring(),
                    )
                    .wait(true),
            )
            .await
            .map_err(|_| Error::unavailable())?;
        ready.insert(name);
        Ok(())
    }

    pub async fn upsert(
        &self,
        space: SpaceId,
        id: Uuid,
        revision: i32,
        content: &str,
        project: Option<&str>,
    ) -> Result<()> {
        self.ensure(space).await?;
        let payload = Payload::try_from(json!({"space_id":space.as_uuid(),"memory_id":id,"revision":revision,"content":content,"project_key":project}))
            .map_err(|_| Error::unavailable())?;
        let point = PointStruct::new(
            id.to_string(),
            std::collections::HashMap::<String, Vec<f32>>::new(),
            payload,
        );
        let result = self
            .client
            .upsert_points(
                UpsertPointsBuilder::new(Self::collection(space), vec![point]).wait(true),
            )
            .await;
        if result.is_err() {
            self.ready.lock().await.remove(&Self::collection(space));
        }
        result.map_err(|_| Error::unavailable())?;
        Ok(())
    }

    pub async fn delete(&self, space: SpaceId, id: Uuid) -> Result<()> {
        self.ensure(space).await?;
        let result = self
            .client
            .delete_points(
                DeletePointsBuilder::new(Self::collection(space))
                    .points(vec![qdrant_client::qdrant::PointId::from(id.to_string())])
                    .wait(true),
            )
            .await;
        if result.is_err() {
            self.ready.lock().await.remove(&Self::collection(space));
        }
        result.map_err(|_| Error::unavailable())?;
        Ok(())
    }

    pub async fn query(
        &self,
        space: SpaceId,
        query: &str,
        project: Option<&str>,
    ) -> Result<Vec<(Uuid, i32)>> {
        Ok(self
            .query_report(space, query, project)
            .await?
            .hits
            .into_iter()
            .map(|hit| (hit.id, hit.revision))
            .collect())
    }

    pub(crate) async fn query_report(
        &self,
        space: SpaceId,
        query: &str,
        project: Option<&str>,
    ) -> Result<SearchReport> {
        self.ensure(space).await?;
        let mut request = QueryPointsBuilder::new(Self::collection(space))
            .query(Query::new_text(query))
            .using(TEXT_FIELD)
            .limit(CANDIDATE_LIMIT)
            .with_payload(true);
        if let Some(project) = project {
            request = request.filter(Filter::should([
                Condition::matches("project_key", project.to_owned()),
                Condition::is_empty("project_key"),
            ]));
        }
        let response = self.client.query(request).await;
        if response.is_err() {
            self.ready.lock().await.remove(&Self::collection(space));
        }
        let response = response.map_err(|_| Error::unavailable())?;
        let candidate_count = response.result.len();
        let server_ms = Some(response.time * 1000.0).filter(|ms| ms.is_finite() && *ms >= 0.0);
        let mut hits = Vec::new();
        for point in response.result {
            if let (Some(id), Some(revision), Some(owner)) = (
                point.payload.get("memory_id").and_then(|v| v.as_str()),
                point.payload.get("revision").and_then(|v| v.as_integer()),
                point.payload.get("space_id").and_then(|v| v.as_str()),
            ) && owner.as_str() == space.as_uuid().to_string()
                && let (Ok(id), Ok(revision)) = (Uuid::parse_str(id), i32::try_from(revision))
            {
                hits.push(SearchHit {
                    id,
                    revision,
                    score: point.score,
                });
            }
        }
        Ok(SearchReport {
            hits,
            candidate_count,
            server_ms,
        })
    }
}
