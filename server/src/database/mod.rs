use std::fs;
use std::time::{Duration, Instant};

use futures::{Stream, TryStreamExt};
use l337_postgres::PostgresConnectionManager;
use log::{error, warn};
use tokio_postgres::types::ToSql;
use tokio_postgres::NoTls;

pub use communities::*;
pub use community_membership::*;
pub use invite_code::*;
pub use token::*;
pub use user::*;
use vertex::{AuthError, DeviceId, ErrResponse, UserId};

use crate::client;

mod communities;
mod community_membership;
mod invite_code;
mod token;
mod user;

pub type DbResult<T> = Result<T, DatabaseError>;

#[derive(Debug)]
pub struct DatabaseError(l337::Error<tokio_postgres::Error>);

impl From<l337::Error<tokio_postgres::Error>> for DatabaseError {
    fn from(e: l337::Error<tokio_postgres::Error>) -> Self {
        DatabaseError(e)
    }
}

impl From<tokio_postgres::Error> for DatabaseError {
    fn from(e: tokio_postgres::Error) -> Self {
        DatabaseError(l337::Error::External(e))
    }
}

impl From<DatabaseError> for ErrResponse {
    fn from(e: DatabaseError) -> ErrResponse {
        match e.0 {
            l337::Error::Internal(e) => {
                error!("Database connection pooling error: {:#?}", e);
            }
            l337::Error::External(sql_error) => {
                error!("Database error: {:#?}", sql_error);
            }
        }

        ErrResponse::Internal
    }
}

impl From<DatabaseError> for AuthError {
    fn from(_: DatabaseError) -> AuthError {
        AuthError::Internal
    }
}

#[derive(Clone)]
pub struct Database {
    pool: l337::Pool<PostgresConnectionManager<NoTls>>,
}

impl Database {
    pub async fn new() -> DbResult<Self> {
        let mgr = PostgresConnectionManager::new(
            fs::read_to_string("db.conf") // TODO use config dirs
                .expect("db.conf not found")
                .parse()
                .unwrap(),
            NoTls,
        );

        let pool = l337::Pool::new(mgr, Default::default())
            .await
            .expect("db error");

        let db = Database { pool };
        db.create_tables().await?;
        Ok(db)
    }

    async fn create_tables(&self) -> DbResult<()> {
        let conn = self.pool.connection().await?;
        let cmds = [
            CREATE_USERS_TABLE,
            CREATE_TOKENS_TABLE,
            CREATE_COMMUNITIES_TABLE,
            CREATE_COMMUNITY_MEMBERSHIP_TABLE,
            CREATE_INVITE_CODES_TABLE,
        ];

        for cmd in &cmds {
            let stmt = conn.client.prepare(cmd).await?;
            conn.client.execute(&stmt, &[]).await?;
        }

        Ok(())
    }

    pub async fn sweep_tokens_loop(self, token_expiry_days: u16, interval: Duration) {
        let mut timer = tokio::time::interval(interval);

        loop {
            timer.tick().await;
            let begin = Instant::now();
            self.expired_tokens(token_expiry_days)
                .await
                .expect("Database error while sweeping tokens")
                .try_for_each(|(user, device)| async move {
                    client::session::remove_and_notify(user, device);
                    Ok(())
                })
                .await
                .expect("Database error while sweeping tokens");

            let time_taken = Instant::now().duration_since(begin);
            if time_taken > interval {
                warn!(
                    "Took {}s to sweep the database for expired tokens, but the interval is {}s!",
                    time_taken.as_secs(),
                    interval.as_secs(),
                );
            }
        }
    }

    async fn expired_tokens(
        &self,
        token_expiry_days: u16,
    ) -> DbResult<impl Stream<Item = DbResult<(UserId, DeviceId)>>> {
        const QUERY: &str = "
            DELETE FROM login_tokens
                WHERE expiration_date < NOW()::timestamp OR
                DATE_PART('days', NOW()::timestamp - last_used) > $1
            RETURNING device, user_id";

        let token_expiry_days = token_expiry_days as f64;
        let args = [token_expiry_days];
        let args = args.iter().map(|x| x as &dyn ToSql);
        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(QUERY).await?;

        let stream = conn
            .client
            .query_raw(&stmt, args)
            .await?
            .and_then(|row| async move {
                Ok((
                    UserId(row.try_get("user_id")?),
                    DeviceId(row.try_get("device")?),
                ))
            })
            .map_err(|e| e.into());
        Ok(stream)
    }

    pub async fn sweep_invite_codes_loop(self, interval: Duration) {
        let mut timer = tokio::time::interval(interval);

        loop {
            timer.tick().await;
            let begin = Instant::now();
            self.delete_expired_invite_codes()
                .await
                .expect("Database error while sweeping invite codes");

            let time_taken = Instant::now().duration_since(begin);
            if time_taken > interval {
                warn!(
                    "Took {}s to sweep the database for expired invite codes, but the interval is {}s!",
                    time_taken.as_secs(),
                    interval.as_secs(),
                );
            }
        }
    }

    async fn delete_expired_invite_codes(&self) -> DbResult<()> {
        const STMT: &str = "DELETE FROM invite_codes WHERE expiration_date < NOW()::timestamp";

        let conn = self.pool.connection().await?;
        let stmt = conn.client.prepare(STMT).await?;
        conn.client.execute(&stmt, &[]).await?;
        Ok(())
    }
}
