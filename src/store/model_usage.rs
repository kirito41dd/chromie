//! 用量按 UTC 日期与请求模型名原子累计，不存请求正文、凭据或逐条流水。
use super::*;
use crate::{
    embedding::{CallUsage, Connection},
    entity::model_usage_daily as daily,
};
use sea_orm::{
    ExprTrait, QueryOrder,
    sea_query::{Alias, Expr},
};
use std::time::{Duration, Instant};

impl Store {
    pub(super) async fn embed_model(
        &self,
        connection: &Connection,
        text: &str,
        timeout: Duration,
    ) -> Result<Vec<f32>> {
        self.embed_model_batch(connection, &[text], timeout)
            .await?
            .pop()
            .ok_or_else(Error::unavailable)
    }

    pub(super) async fn embed_model_batch(
        &self,
        connection: &Connection,
        texts: &[&str],
        timeout: Duration,
    ) -> Result<Vec<Vec<f32>>> {
        let deadline = tokio::time::Instant::now() + timeout;
        let limit = self.model_limit(&connection.profile.model).await?;
        let permit = tokio::time::timeout_at(
            deadline,
            self.model_runtime.acquire(&connection.profile.model, limit),
        )
        .await
        .map_err(|_| Error {
            code: "model_busy",
            message: "模型并发已满，等待超时，请稍后重试",
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        })?;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(Error {
                code: "model_busy",
                message: "模型并发等待超时，请稍后重试",
                status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            });
        }
        let report = connection.embed_batch(texts, remaining).await;
        // 额度只覆盖模型 HTTP，不占用后续用量统计和向量发布阶段。
        drop(permit);
        if let Some(usage) = report.usage {
            // 用量故障不能触发模型重试或掩盖已经获得的向量；不重试可能已提交的累计。
            match tokio::time::timeout(
                Duration::from_secs(1),
                self.record_model_usage(&connection.profile.model, &usage),
            )
            .await
            {
                Ok(Ok(())) => {}
                _ => tracing::warn!("模型用量未能确认累计，模型结果继续使用"),
            }
        }
        report.result
    }

    pub(super) async fn probe_model_connection(
        &self,
        connection: &Connection,
        batch: crate::embedding_batch::BatchSize,
    ) -> Result<Value> {
        let started = Instant::now();
        let texts = vec![
            "Chromie 虚构连接测试：记住项目的开发约定。";
            batch.value() as usize
        ];
        let vectors = self
            .embed_model_batch(connection, &texts, Duration::from_secs(10))
            .await?;
        Ok(
            json!({"model":connection.profile.model,"dimensions":connection.profile.dimensions,"batch_size":vectors.len(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}),
        )
    }

    async fn record_model_usage(&self, model: &str, usage: &CallUsage) -> Result<()> {
        let tokens = usage.tokens.unwrap_or(0);
        let failed = u64::from(usage.failed);
        let unknown = u64::from(usage.tokens.is_none());
        let updated = now();
        daily::Entity::insert(daily::ActiveModel {
            usage_date: Set(usage.date),
            model_key: Set(Sha256::digest(model.as_bytes()).to_vec()),
            model_name: Set(model.to_owned()),
            request_count: Set(1),
            total_tokens: Set(tokens),
            failed_count: Set(failed),
            unknown_usage_count: Set(unknown),
            updated_at: Set(updated),
        })
        .on_conflict(
            OnConflict::columns([daily::Column::UsageDate, daily::Column::ModelKey])
                .value(
                    daily::Column::RequestCount,
                    Expr::col(daily::Column::RequestCount).add(1u64),
                )
                .value(
                    daily::Column::TotalTokens,
                    Expr::col(daily::Column::TotalTokens).add(tokens),
                )
                .value(
                    daily::Column::FailedCount,
                    Expr::col(daily::Column::FailedCount).add(failed),
                )
                .value(
                    daily::Column::UnknownUsageCount,
                    Expr::col(daily::Column::UnknownUsageCount).add(unknown),
                )
                .value(daily::Column::UpdatedAt, updated)
                .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await?;
        Ok(())
    }

    async fn usage_totals(&self, range: Option<(time::Date, time::Date)>) -> Result<Value> {
        let mut query = daily::Entity::find().select_only();
        for (column, alias) in [
            (daily::Column::RequestCount, "requests"),
            (daily::Column::TotalTokens, "tokens"),
            (daily::Column::FailedCount, "failed"),
            (daily::Column::UnknownUsageCount, "unknown"),
        ] {
            // MySQL SUM(BIGINT) 为 DECIMAL，显式转无符号整数，无需引入 Decimal 依赖。
            query = query.column_as(
                Expr::col(column).sum().cast_as(Alias::new("UNSIGNED")),
                alias,
            );
        }
        if let Some((since, until)) = range {
            query = query
                .filter(daily::Column::UsageDate.gte(since))
                .filter(daily::Column::UsageDate.lte(until));
        }
        let (requests, tokens, failed, unknown) = query
            .into_tuple::<(Option<u64>, Option<u64>, Option<u64>, Option<u64>)>()
            .one(&self.db)
            .await?
            .unwrap_or_default();
        Ok(
            json!({"requests":requests.unwrap_or(0),"tokens":tokens.unwrap_or(0),"failed":failed.unwrap_or(0),"unknown":unknown.unwrap_or(0)}),
        )
    }

    pub async fn model_usage(&self, space: SpaceId) -> Result<Value> {
        self.check_model_admin(space).await?;
        let today = now().date();
        let since = today - time::Duration::days(6);
        let mut days = daily::Entity::find()
            .filter(daily::Column::UsageDate.gte(since))
            .filter(daily::Column::UsageDate.lte(today))
            .order_by_desc(daily::Column::UsageDate)
            .order_by_asc(daily::Column::ModelName)
            .limit(101)
            .all(&self.db)
            .await?;
        let truncated = days.len() > 100;
        days.truncate(100);
        let rows = days.into_iter().map(|row| json!({"date":row.usage_date.to_string(),"model":row.model_name,
            "requests":row.request_count,"tokens":row.total_tokens,"failed":row.failed_count,"unknown":row.unknown_usage_count})).collect::<Vec<_>>();
        Ok(
            json!({"available":true,"timezone":"UTC","today":self.usage_totals(Some((today,today))).await?,
            "seven_days":self.usage_totals(Some((since,today))).await?,"all_time":self.usage_totals(None).await?,"days":rows,"truncated":truncated}),
        )
    }
}
