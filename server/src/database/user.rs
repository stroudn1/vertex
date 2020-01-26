use super::*;
use crate::auth::HashSchemeVersion;
use std::convert::TryFrom;
use tokio_postgres::{error::SqlState, row::Row};
use uuid::Uuid;
use vertex_common::{ErrResponse, UserId};

pub(super) const CREATE_USERS_TABLE: &'static str = "
CREATE TABLE IF NOT EXISTS users (
    id                   UUID PRIMARY KEY,
    username             VARCHAR NOT NULL UNIQUE,
    display_name         VARCHAR NOT NULL,
    password_hash        VARCHAR NOT NULL,
    hash_scheme_version  SMALLINT NOT NULL,
    compromised          BOOLEAN NOT NULL,
    locked               BOOLEAN NOT NULL,
    banned               BOOLEAN NOT NULL
)";

pub struct UserRecord {
    pub id: UserId,
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub hash_scheme_version: HashSchemeVersion,
    pub compromised: bool,
    pub locked: bool,
    pub banned: bool,
}

impl UserRecord {
    pub fn new(
        username: String,
        display_name: String,
        password_hash: String,
        hash_scheme_version: HashSchemeVersion,
    ) -> Self {
        UserRecord {
            id: UserId(Uuid::new_v4()),
            username,
            display_name,
            password_hash,
            hash_scheme_version,
            compromised: false,
            locked: false,
            banned: false,
        }
    }
}

impl TryFrom<Row> for UserRecord {
    type Error = tokio_postgres::Error;

    fn try_from(row: Row) -> Result<UserRecord, tokio_postgres::Error> {
        Ok(UserRecord {
            id: UserId(row.try_get("id")?),
            username: row.try_get("username")?,
            display_name: row.try_get("display_name")?,
            password_hash: row.try_get("password_hash")?,
            hash_scheme_version: HashSchemeVersion::from(
                row.try_get::<&str, i16>("hash_scheme_version")?,
            ),
            compromised: row.try_get("compromised")?,
            locked: row.try_get("locked")?,
            banned: row.try_get("banned")?,
        })
    }
}

pub struct GetUserById(pub UserId);

impl Message for GetUserById {
    type Result = Result<Option<UserRecord>, ErrResponse>;
}

pub struct GetUserByName(pub String);

impl Message for GetUserByName {
    type Result = Result<Option<UserRecord>, ErrResponse>;
}

pub struct CreateUser(pub UserRecord);

impl Message for CreateUser {
    type Result = Result<bool, ErrResponse>;
}

pub struct ChangeUsername {
    pub user: UserId,
    pub new_username: String,
}

impl Message for ChangeUsername {
    type Result = Result<bool, ErrResponse>;
}

pub struct ChangeDisplayName {
    pub user: UserId,
    pub new_display_name: String,
}

impl Message for ChangeDisplayName {
    type Result = Result<(), ErrResponse>;
}

pub struct ChangePassword {
    pub user: UserId,
    pub new_password_hash: String,
    pub hash_version: HashSchemeVersion,
}

impl Message for ChangePassword {
    type Result = Result<(), ErrResponse>;
}

impl Handler<CreateUser> for DatabaseServer {
    type Responder<'a> = impl Future<Output = Result<bool, ErrResponse>> + 'a;

    fn handle(&mut self, create: CreateUser, _: &mut Context<Self>) -> Self::Responder<'_> {
        let user = create.0;
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let stmt = conn
                .client
                .prepare(
                    "INSERT INTO users
                (
                    id,
                    username,
                    display_name,
                    password_hash,
                    hash_scheme_version,
                    compromised,
                    locked,
                    banned
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                ON CONFLICT DO NOTHING",
                )
                .await
                .map_err(handle_error_psql)?;

            let ret = conn
                .client
                .execute(
                    &stmt,
                    &[
                        &user.id.0,
                        &user.username,
                        &user.display_name,
                        &user.password_hash,
                        &(user.hash_scheme_version as u8 as i16),
                        &user.compromised,
                        &user.locked,
                        &user.banned,
                    ],
                )
                .await
                .map_err(handle_error_psql)?;

            Ok(ret == 1) // Return true if 1 item was inserted (insert was successful)
        })
    }
}

impl Handler<GetUserById> for DatabaseServer {
    type Responder<'a> = impl Future<Output = Result<Option<UserRecord>, ErrResponse>> + 'a;

    fn handle(&mut self, get: GetUserById, _: &mut Context<Self>) -> Self::Responder<'_> {
        let id = get.0;
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let query = conn
                .client
                .prepare("SELECT * FROM users WHERE id=$1")
                .await
                .map_err(handle_error_psql)?;
            let opt = conn
                .client
                .query_opt(&query, &[&id.0])
                .await
                .map_err(handle_error_psql)?;

            if let Some(row) = opt {
                Ok(Some(UserRecord::try_from(row).map_err(handle_error_psql)?))
            } else {
                Ok(None)
            }
        })
    }
}

impl Handler<GetUserByName> for DatabaseServer {
    type Responder<'a> = impl Future<Output = Result<Option<UserRecord>, ErrResponse>> + 'a;

    fn handle(&mut self, get: GetUserByName, _: &mut Context<Self>) -> Self::Responder<'_> {
        let name = get.0;
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let query = conn
                .client
                .prepare("SELECT * FROM users WHERE username=$1")
                .await
                .map_err(handle_error_psql)?;
            let opt = conn
                .client
                .query_opt(&query, &[&name])
                .await
                .map_err(handle_error_psql)?;

            if let Some(row) = opt {
                Ok(Some(UserRecord::try_from(row).map_err(handle_error_psql)?))
            } else {
                Ok(None)
            }
        })
    }
}

impl Handler<ChangeUsername> for DatabaseServer {
    type Responder<'a> = impl Future<Output = Result<bool, ErrResponse>> + 'a;

    fn handle(&mut self, change: ChangeUsername, _: &mut Context<Self>) -> Self::Responder<'_> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let stmt = conn
                .client
                .prepare("UPDATE users SET username = $1 WHERE id = $2")
                .await
                .map_err(handle_error_psql)?;
            let res = conn
                .client
                .execute(&stmt, &[&change.new_username, &change.user.0])
                .await;
            match res {
                Ok(ret) => Ok(ret == 1),
                Err(ref e)
                    if e.code() == Some(&SqlState::INTEGRITY_CONSTRAINT_VIOLATION)
                        || e.code() == Some(&SqlState::UNIQUE_VIOLATION) =>
                {
                    Ok(false)
                }
                Err(e) => Err(handle_error_psql(e)),
            }
        })
    }
}

impl Handler<ChangeDisplayName> for DatabaseServer {
    type Responder<'a> = impl Future<Output = Result<(), ErrResponse>> + 'a;

    fn handle(&mut self, change: ChangeDisplayName, _: &mut Context<Self>) -> Self::Responder<'_> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let stmt = conn
                .client
                .prepare("UPDATE users SET display_name = $1 WHERE id = $2")
                .await
                .map_err(handle_error_psql)?;
            conn.client
                .execute(&stmt, &[&change.new_display_name, &change.user.0])
                .await
                .map_err(handle_error_psql)?;
            Ok(())
        })
    }
}

impl Handler<ChangePassword> for DatabaseServer {
    type Responder<'a> = impl Future<Output = Result<(), ErrResponse>> + 'a;

    fn handle(&mut self, change: ChangePassword, _: &mut Context<Self>) -> Self::Responder<'_> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let conn = pool.connection().await.map_err(handle_error)?;
            let stmt = conn
                .client
                .prepare(
                    "UPDATE users SET
                        password_hash = $1, hash_scheme_version = $2, compromised = $3
                    WHERE id = $4",
                )
                .await
                .map_err(handle_error_psql)?;

            let res = conn
                .client
                .execute(
                    &stmt,
                    &[
                        &change.new_password_hash,
                        &(change.hash_version as u8 as i16),
                        &false,
                        &change.user.0,
                    ],
                )
                .await
                .map_err(handle_error_psql)?;

            Ok(())
        })
    }
}