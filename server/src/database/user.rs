use super::*;
use crate::auth::HashSchemeVersion;
use std::convert::TryFrom;
use tokio_postgres::{error::SqlState, row::Row, types::ToSql};
use uuid::Uuid;

pub(super) const CREATE_USERS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS users (
        id                   UUID PRIMARY KEY,
        username             VARCHAR NOT NULL UNIQUE,
        display_name         VARCHAR NOT NULL,
        profile_version      INTEGER NOT NULL,
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
    pub profile_version: ProfileVersion,
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
            profile_version: ProfileVersion(0),
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
            profile_version: ProfileVersion(row.try_get::<&str, i32>("profile_version")? as u32),
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

impl Into<ServerUser> for UserRecord {
    fn into(self) -> ServerUser {
        ServerUser {
            username: self.username,
            display_name: self.display_name,
            banned: self.banned,
            locked: self.locked,
            compromised: self.compromised,
            latest_hash_scheme: self.hash_scheme_version == HashSchemeVersion::LATEST,
            id: self.id,
        }
    }
}

pub struct UsernameConflict;
pub struct NonexistentUser;

pub enum ChangeUsernameError {
    NonexistentUser,
    UsernameConflict,
}

impl Database {
    pub async fn get_user_by_id(&self, id: UserId) -> DbResult<Option<UserRecord>> {
        let query = "SELECT * FROM users WHERE id=$1";
        let row = self.query_opt(query, &[&id.0]).await?;
        if let Some(row) = row {
            Ok(Some(UserRecord::try_from(row)?)) // Can't opt::map because of ?
        } else {
            Ok(None)
        }
    }

    pub async fn get_user_by_name(&self, name: String) -> DbResult<Option<UserRecord>> {
        let query = "SELECT * FROM users WHERE username=$1";
        let row = self.query_opt(query, &[&name]).await?;
        if let Some(row) = row {
            Ok(Some(UserRecord::try_from(row)?)) // Can't opt::map because of ?
        } else {
            Ok(None)
        }
    }

    pub async fn get_user_profile(&self, id: UserId) -> DbResult<Option<Profile>> {
        let query = "SELECT username, display_name, profile_version FROM users WHERE id=$1";
        let opt = self.query_opt(query, &[&id.0]).await?;
        if let Some(row) = opt {
            // Can't opt::map because of ?
            Ok(Some(Profile {
                version: ProfileVersion(row.try_get::<&str, i32>("profile_version")? as u32),
                username: row.try_get("username")?,
                display_name: row.try_get("display_name")?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Creates a user, returning whether it was successful (i.e, if there were no conflicts with
    /// respect to the ID and username).
    pub async fn create_user(&self, user: UserRecord) -> DbResult<Result<(), UsernameConflict>> {
        const STMT: &str = "
            INSERT INTO users
                (
                    id,
                    username,
                    display_name,
                    profile_version,
                    password_hash,
                    hash_scheme_version,
                    compromised,
                    locked,
                    banned
                )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT DO NOTHING";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(STMT).await?;
        let args: &[&(dyn ToSql + Sync)] = &[
            &user.id.0,
            &user.username,
            &user.display_name,
            &(user.profile_version.0 as i32),
            &user.password_hash,
            &(user.hash_scheme_version as i16),
            &user.compromised,
            &user.locked,
            &user.banned,
        ];

        let ret = conn.client.execute(&stmt, args).await?;

        Ok(if ret == 1 {
            // 1 item was inserted (insert was successful)
            Ok(())
        } else {
            Err(UsernameConflict)
        })
    }

    pub async fn change_username(
        &self,
        user: UserId,
        new_username: String,
    ) -> DbResult<Result<(), ChangeUsernameError>> {
        const STMT: &str = "
            UPDATE users
                SET username = $1, profile_version = profile_version + 1
                WHERE id = $2
        ";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(STMT).await?;
        let res = conn.client.execute(&stmt, &[&new_username, &user.0]).await;

        match res {
            Ok(ret) => {
                if ret == 1 {
                    Ok(Ok(()))
                } else {
                    Ok(Err(ChangeUsernameError::NonexistentUser))
                }
            }
            Err(e) => {
                if e.code() == Some(&SqlState::INTEGRITY_CONSTRAINT_VIOLATION)
                    || e.code() == Some(&SqlState::UNIQUE_VIOLATION)
                {
                    Ok(Err(ChangeUsernameError::UsernameConflict))
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// Changes the display name of a user, returning whether the user existed at all.
    pub async fn change_display_name(
        &self,
        user: UserId,
        new_display_name: String,
    ) -> DbResult<Result<(), NonexistentUser>> {
        const STMT: &str = "
            UPDATE users
                SET display_name = $1, profile_version = profile_version + 1
                WHERE id = $2
        ";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(STMT).await?;
        let res = conn
            .client
            .execute(&stmt, &[&new_display_name, &user.0])
            .await?;
        Ok(if res == 1 {
            Ok(())
        } else {
            Err(NonexistentUser)
        })
    }

    /// Changes the password of a user, returning whether the user existed at all.
    pub async fn change_password(
        &self,
        user: UserId,
        new_password_hash: String,
        hash_scheme_version: HashSchemeVersion,
    ) -> DbResult<Result<(), NonexistentUser>> {
        const STMT: &str = "
            UPDATE users
                SET password_hash = $1, hash_scheme_version = $2, compromised = $3
                WHERE id = $4";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(STMT).await?;
        let args: &[&(dyn ToSql + Sync)] = &[
            &new_password_hash,
            &(hash_scheme_version as i16),
            &false,
            &user.0,
        ];

        let res = conn.client.execute(&stmt, args).await?;
        Ok(if res == 1 {
            Ok(())
        } else {
            Err(NonexistentUser)
        })
    }

    pub async fn set_banned(
        &self,
        user: UserId,
        banned: bool,
    ) -> DbResult<Result<(), NonexistentUser>> {
        const STMT: &str = "UPDATE users SET banned = $1 WHERE id = $2";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(STMT).await?;
        let args: &[&(dyn ToSql + Sync)] = &[&banned, &user.0];

        let res = conn.client.execute(&stmt, args).await?;
        Ok(if res == 1 {
            Ok(())
        } else {
            Err(NonexistentUser)
        })
    }

    pub async fn set_locked(
        &self,
        user: UserId,
        locked: bool,
    ) -> DbResult<Result<(), NonexistentUser>> {
        const STMT: &str = "UPDATE users SET locked = $1 WHERE id = $2";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(STMT).await?;
        let args: &[&(dyn ToSql + Sync)] = &[&locked, &user.0];

        let res = conn.client.execute(&stmt, args).await?;
        Ok(if res == 1 {
            Ok(())
        } else {
            Err(NonexistentUser)
        })
    }

    pub async fn search_user(
        &self,
        name: String,
    ) -> DbResult<impl Stream<Item = DbResult<UserRecord>>> {
        const QUERY: &str = "SELECT * FROM users
                                WHERE $1 % username
                                ORDER BY SIMILARITY($1, username) DESC";

        let stream = self.query_stream(QUERY, &[&name]).await?;
        let stream = stream
            .and_then(|row| async move { Ok(UserRecord::try_from(row)?) })
            .map_err(|e| e.into());

        Ok(stream)
    }

    pub async fn list_all_server_users(
        &self,
    ) -> DbResult<impl Stream<Item = DbResult<UserRecord>>> {
        const QUERY: &str = "SELECT * FROM users";

        let stream = self.query_stream(QUERY, &[]).await?;
        let stream = stream
            .and_then(|row| async move { Ok(UserRecord::try_from(row)?) })
            .map_err(|e| e.into());

        Ok(stream)
    }

    pub async fn set_all_accounts_compromised(&self) -> DbResult<()> {
        const SET_COMPROMISED: &str = "UPDATE users SET compromised = $1";
        const DELETE_TOKENS: &str = "DELETE FROM login_tokens";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(SET_COMPROMISED).await?;
        conn.client.execute(&stmt, &[&true]).await?;

        let stmt = conn.client.prepare(DELETE_TOKENS).await?;
        conn.client.execute(&stmt, &[]).await?;

        Ok(())
    }

    pub async fn set_accounts_with_old_hashes_compromised(&self) -> DbResult<()> {
        const SET_COMPROMISED: &str =
            "UPDATE users SET compromised = $1 WHERE hash_scheme_version < $2";
        const DELETE_TOKENS: &str = "
            DELETE FROM login_tokens
                USING users
                WHERE login_tokens.user_id = users.id
                AND users.compromised;";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(SET_COMPROMISED).await?;
        conn.client
            .execute(&stmt, &[&true, &(HashSchemeVersion::LATEST as i16)])
            .await?;

        let stmt = conn.client.prepare(DELETE_TOKENS).await?;
        conn.client.execute(&stmt, &[]).await?;

        Ok(())
    }
}
