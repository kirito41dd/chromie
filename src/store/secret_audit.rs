use super::*;
use crate::{entity::secret_access_audit, secret_audit::ExecutionReport};
use sea_orm::IntoActiveModel;

impl Store {
    pub(super) async fn audit_delivery<C: ConnectionTrait>(
        &self,
        db: &C,
        space: SpaceId,
        id: Uuid,
        revision: i32,
        purpose: Option<String>,
    ) -> Result<Uuid> {
        let audit = Uuid::now_v7();
        secret_access_audit::ActiveModel {
            space_id: Set(space.bytes()),
            id: Set(audit.as_bytes().to_vec()),
            memory_id: Set(id.as_bytes().to_vec()),
            agent_id: Set(self.actor_bytes()),
            revision: Set(revision),
            purpose: Set(purpose),
            outcome: Set("delivered".into()),
            exit_code: Set(None),
            delivered_at: Set(now()),
            reported_at: Set(None),
        }
        .insert(db)
        .await?;
        Ok(audit)
    }

    /// 本机结果只能由原取用 Agent 上报；不能作为服务端认证的业务成功。
    pub async fn report_secret_execution(
        &self,
        space: SpaceId,
        audit: Uuid,
        report: ExecutionReport,
    ) -> Result<()> {
        report.validate()?;
        let actor = self.actor.as_ref().ok_or_else(Error::agent_unauthorized)?;
        let tx = self.begin(space).await?;
        Self::validate_agent(&tx, space, actor, true).await?;
        let row =
            secret_access_audit::Entity::find_by_id((space.bytes(), audit.as_bytes().to_vec()))
                .one(&tx)
                .await?
                .ok_or_else(Error::missing)?;
        if row.agent_id != self.actor_bytes() {
            return Err(Error::missing());
        }
        if row.outcome != "delivered" {
            if row.outcome != report.outcome.as_str() || row.exit_code != report.exit_code {
                return Err(Error::conflict("execution_report_conflict"));
            }
        } else {
            let mut active = row.into_active_model();
            active.outcome = Set(report.outcome.as_str().into());
            active.exit_code = Set(report.exit_code);
            active.reported_at = Set(Some(now()));
            active.update(&tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
