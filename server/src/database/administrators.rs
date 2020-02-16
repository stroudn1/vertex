use crate::database::{Database, DbResult, InvalidUser};
use bitflags::bitflags;
use std::error::Error;
use tokio_postgres::error::{DbError, SqlState};
use tokio_postgres::types::ToSql;
use vertex::UserId;

pub(super) const CREATE_ADMINISTRATORS_TABLE: &str = r"
    CREATE TABLE IF NOT EXISTS administrators (
        user_id              UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
        permission_flags     BIGINT NOT NULL
    )";

bitflags! {
    pub struct AdminPermissionFlags: i64 {
        /// All permissions. Could be used for the server owner.
        const ALL = 1;
        /// Ban users.
        const BAN = 1 << 1;
    }
}

pub enum CreateAdminError {
    InvalidUser,
    AlreadyAdmin,
}

impl Database {
    pub async fn create_admin(
        &self,
        user: UserId,
        permissions: AdminPermissionFlags,
    ) -> DbResult<Result<(), CreateAdminError>> {
        const STMT: &str = "
            INSERT (user, permission_flags) INTO administrators
                ON CONFLICT DO NOTHING
        ";

        let conn = self.pool.connection().await?;
        let args: &[&(dyn ToSql + Sync)] = &[&user.0, &permissions.bits()];
        let res = conn.client.execute(STMT, args).await;

        match res {
            Ok(1) => {
                // 1 row modified = successfully added
                Ok(Ok(()))
            }
            Ok(0) => {
                // 0 rows modified = failed to add
                Ok(Err(CreateAdminError::AlreadyAdmin))
            }
            Ok(_n) => {
                panic!("db error: create admin query returned more than one row modified!");
            }
            Err(err) => {
                if err.code() == Some(&SqlState::FOREIGN_KEY_VIOLATION) {
                    let constraint = err
                        .source()
                        .and_then(|e| e.downcast_ref::<DbError>())
                        .and_then(|e| e.constraint());

                    match constraint {
                        Some("administrators_user_fkey") => Ok(Err(CreateAdminError::InvalidUser)),
                        Some(_) | None => Err(err.into()),
                    }
                } else {
                    Err(err.into())
                }
            }
        }
    }

    pub async fn get_admin_permissions(&self, user: UserId) -> DbResult<AdminPermissionFlags> {
        const QUERY: &str = "SELECT permission_flags FROM administrators WHERE user_id = $1";

        let conn = self.pool.connection().await?;
        let opt = conn.client.query_opt(QUERY, &[&user.0]).await?;

        if let Some(row) = opt {
            Ok(AdminPermissionFlags::from_bits_truncate(
                row.try_get("permission_flags")?,
            ))
        } else {
            Ok(AdminPermissionFlags::from_bits_truncate(0))
        }
    }

    pub async fn set_admin_permissions(
        &self,
        user: UserId,
        permissions: AdminPermissionFlags,
    ) -> DbResult<Result<(), InvalidUser>> {
        const STMT: &str = "UPDATE administrators SET permission_flags = $1 WHERE user_id = $2";

        let conn = self.pool.connection().await?;
        let args: &[&(dyn ToSql + Sync)] = &[&permissions.bits(), &user.0];
        let ret = conn.client.execute(STMT, args).await?;

        if ret == 1 {
            // 1 row modified = user was admin
            Ok(Ok(()))
        } else {
            Ok(Err(InvalidUser))
        }
    }
}
