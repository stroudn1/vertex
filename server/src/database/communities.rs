use vertex_common::{ServerError, CommunityId};
use tokio_postgres::Row;
use std::convert::TryFrom;
use actix::{Message, Handler, ResponseFuture, Context};
use crate::database::{DatabaseServer, handle_error, handle_error_psql};
use uuid::Uuid;

pub(super) const CREATE_COMMUNITIES_TABLE: &'static str = "
CREATE TABLE IF NOT EXISTS communities (
    id   UUID PRIMARY KEY,
    name VARCHAR NOT NULL
)";

#[derive(Debug, Clone)]
pub struct CommunityRecord {
    pub id: CommunityId,
    pub name: String,
}

impl TryFrom<Row> for CommunityRecord {
    type Error = tokio_postgres::Error;

    fn try_from(row: Row) -> Result<CommunityRecord, tokio_postgres::Error> {
        Ok(CommunityRecord {
            id: CommunityId(row.try_get("id")?),
            name: row.try_get("name")?,
        })
    }
}

pub struct GetCommunityMetadata(CommunityId);

impl Message for GetCommunityMetadata {
    type Result = Result<Option<CommunityRecord>, ServerError>;
}

pub struct CreateCommunity {
    pub name: String,
}

impl Message for CreateCommunity {
    type Result = Result<CommunityRecord, ServerError>;
}

// TODO(next): load at boot
impl Handler<GetCommunityMetadata> for DatabaseServer {
    type Result = ResponseFuture<Result<Option<CommunityRecord>, ServerError>>;

    fn handle(&mut self, get: GetCommunityMetadata, _: &mut Context<Self>) -> Self::Result {
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let query = conn.client.prepare("SELECT * FROM communities WHERE id=$1").await.map_err(handle_error_psql)?;
            let opt = conn.client.query_opt(&query, &[&(get.0).0]).await.map_err(handle_error_psql)?;

            if let Some(row) = opt {
                Ok(Some(CommunityRecord::try_from(row).map_err(handle_error_psql)?))
            } else {
                Ok(None)
            }
        })
    }
}

impl Handler<CreateCommunity> for DatabaseServer {
    type Result = ResponseFuture<Result<CommunityRecord, ServerError>>;

    fn handle(&mut self, create: CreateCommunity, _: &mut Context<Self>) -> Self::Result {
        let id = Uuid::new_v4();
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let query = conn.client
                .prepare("INSERT INTO communities (id, name) VALUES ($1, $2) RETURNING *")
                .await
                .map_err(handle_error_psql)?;
            let row = conn.client.query_one(&query, &[&id, &create.name])
                .await
                .map_err(handle_error_psql)?;
            CommunityRecord::try_from(row).map_err(handle_error_psql)
        })
    }
}
