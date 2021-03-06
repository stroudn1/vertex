#![feature(type_ascription, type_alias_impl_trait)]

use std::convert::Infallible;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::StreamExt;
use governor::clock::DefaultClock;
use governor::state::keyed::DashMapStateStore;
use governor::{Quota, RateLimiter};
use log::{info, LevelFilter};
use warp::reply::Reply;
use warp::Filter;
use xtra::prelude::*;
use xtra::Disconnected;

use client::ActiveSession;
use database::Database;
use vertex::prelude::*;

use crate::client::Authenticator;
use crate::community::{Community, CommunityActor};
use crate::config::Config;
use crate::database::{DbResult, MalformedInviteCode};
use clap::{App, Arg};
use crate::client::session::WsMessage;
use vertex::RATELIMIT_BURST_PER_MIN;

mod auth;
mod client;
mod community;
mod config;
mod database;

#[derive(Clone)]
pub struct Global {
    pub database: Database,
    pub config: Arc<Config>,
    pub ratelimiter: ArcSwap<RateLimiter<DeviceId, DashMapStateStore<DeviceId>, DefaultClock>>,
}

/// Marker trait for `vertex_common` structs that are actor messages too
trait VertexActorMessage: Send + 'static {
    type Result: Send;
}

impl VertexActorMessage for ClientSentMessage {
    type Result = MessageConfirmation;
}

impl VertexActorMessage for Edit {
    type Result = ();
}

struct IdentifiedMessage<T: VertexActorMessage> {
    user: UserId,
    device: DeviceId,
    message: T,
}

impl<T> xtra::Message for IdentifiedMessage<T>
where
    T: VertexActorMessage,
    T::Result: 'static,
{
    type Result = Result<T::Result, Error>;
}

fn new_ratelimiter() -> RateLimiter<DeviceId, DashMapStateStore<DeviceId>, DefaultClock> {
    RateLimiter::dashmap(Quota::per_minute(NonZeroU32::new(RATELIMIT_BURST_PER_MIN).unwrap()))
}

async fn refresh_ratelimiter(
    rl: ArcSwap<RateLimiter<DeviceId, DashMapStateStore<DeviceId>, DefaultClock>>,
) {
    use tokio::time::Instant;
    let duration = Duration::from_secs(60 * 60); // 1/hr
    let mut timer = tokio::time::interval_at(Instant::now() + duration, duration);

    loop {
        timer.tick().await;
        rl.store(Arc::new(new_ratelimiter()));
    }
}

fn handle_disconnected(actor_name: &'static str) -> impl Fn(Disconnected) -> Error {
    move |_| {
        log::warn!(
            "{} actor disconnected. This may be a timing anomaly.",
            actor_name
        );
        Error::Internal
    }
}

async fn load_communities(db: Database) {
    let stream = db
        .get_all_communities()
        .await
        .expect("Error loading communities");
    futures::pin_mut!(stream);

    while let Some(res) = stream.next().await {
        let community_record = res.expect("Error loading community");
        CommunityActor::load_and_spawn(community_record, db.clone())
            .await
            .expect("Error loading community!");
    }
}

#[tokio::main]
async fn main() {
    let args = App::new("Vertex server")
        .version("0.1")
        .author("Restioson <restiosondev@gmail.com>")
        .about("Server for the Vertex chat application https://github.com/Restioson/vertex")
        .arg(
            Arg::with_name("add-admin")
                .short("A")
                .long("add-admin")
                .value_name("USERNAME")
                .help("Adds an admin with all permissions")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("remove-admin")
                .short("R")
                .long("remove-admin")
                .value_name("USERNAME")
                .help("Removes a user as admin")
                .takes_value(true),
        )
        .get_matches();

    println!("Vertex server starting...");

    let config = config::load_config();
    vertex::setup_logging(
        "vertex_server",
        LevelFilter::from_str(&config.log_level).unwrap(),
    );

    let (cert_path, key_path) = config::ssl_config();
    let database = Database::new().await.expect("Error in database setup");
    tokio::spawn(database.clone().sweep_tokens_loop(
        config.token_expiry_days,
        Duration::from_secs(config.tokens_sweep_interval_secs),
    ));
    tokio::spawn(
        database
            .clone()
            .sweep_invite_codes_loop(Duration::from_secs(config.invite_codes_sweep_interval_secs)),
    );

    promote_and_demote(args, &database).await;

    load_communities(database.clone()).await;

    let config = Arc::new(config);
    let global = Global {
        database,
        config: config.clone(),
        ratelimiter: ArcSwap::from_pointee(new_ratelimiter()),
    };

    tokio::spawn(refresh_ratelimiter(global.ratelimiter.clone()));

    let global = warp::any().map(move || global.clone());

    let authenticate = warp::path("authenticate")
        .and(global.clone())
        .and(warp::query())
        .and(warp::ws())
        .and_then(
            |global: Global, authenticate, ws: warp::ws::Ws| async move {
                let response: Box<dyn warp::Reply> =
                    match self::login(global.clone(), ws, authenticate).await {
                        Ok(response) => Box::new(response),
                        Err(e) => return reply_err(e),
                    };
                Ok(response)
            },
        );

    let register = warp::path("register")
        .and(global.clone())
        .and(warp::post())
        .and(warp::body::bytes())
        .and_then(
            |global, bytes| async move { reply_protobuf(self::register(global, bytes).await) },
        );

    let create_token = warp::path("create")
        .and(global.clone())
        .and(warp::post())
        .and(warp::body::bytes())
        .and_then(|global, bytes| async move {
            reply_protobuf(self::create_token(global, bytes).await)
        });

    let revoke_token = warp::path("revoke")
        .and(global.clone())
        .and(warp::post())
        .and(warp::body::bytes())
        .and_then(|global, bytes| async move {
            reply_protobuf(self::revoke_token(global, bytes).await)
        });

    let refresh_token = warp::path("refresh")
        .and(global.clone())
        .and(warp::post())
        .and(warp::body::bytes())
        .and_then(|global, bytes| async move {
            reply_protobuf(self::refresh_token(global, bytes).await)
        });

    let change_password = warp::path("change_password")
        .and(global.clone())
        .and(warp::post())
        .and(warp::body::bytes())
        .and_then(|global, bytes| async move {
            reply_protobuf(self::change_password(global, bytes).await)
        });

    let invite = warp::path!("invite" / String)
        //  .and(warp::header::<String>("host")) // https://github.com/seanmonstar/warp/issues/432
        .and(global.clone())
        .and_then(|invite, global| self::invite_reply(global, invite));

    let token = warp::path("token").and(create_token.or(revoke_token).or(refresh_token));
    let auth = authenticate.or(register.or(token.or(change_password)));
    let client = warp::path("client").and(auth);
    let routes = invite.or(client);
    let routes = warp::path("vertex").and(routes);

    info!("Vertex server starting on addr {}", config.ip);

    if config.https {
        warp::serve(routes)
            .tls()
            .cert_path(cert_path)
            .key_path(key_path)
            .run(config.ip)
            .await;
    } else {
        warp::serve(routes).run(config.ip).await;
    }
}

async fn promote_and_demote(args: clap::ArgMatches<'_>, database: &Database) {
    for name in args.values_of("add-admin").into_iter().flatten() {
        let id = database
            .get_user_by_name(name.to_string())
            .await
            .expect("Error promoting user to admin")
            .unwrap_or_else(|| panic!("Invalid username {} to add as admin", name))
            .id;

        database
            .set_admin_permissions(id, AdminPermissionFlags::ALL)
            .await
            .unwrap_or_else(|e| panic!("Error promoting user {} to admin: {:?}", name, e))
            .unwrap_or_else(|e| panic!("Error promoting user {} to admin: {:?}", name, e));

        info!(
            "User {} successfully promoted to admin with all permissions!",
            name
        );
    }

    for name in args.values_of("remove-admin").into_iter().flatten() {
        let id = database
            .get_user_by_name(name.to_string())
            .await
            .expect("Error removing user as admin")
            .unwrap_or_else(|| panic!("Invalid username {} to demote", name))
            .id;

        database
            .set_admin_permissions(id, AdminPermissionFlags::from_bits_truncate(0))
            .await
            .unwrap_or_else(|e| panic!("Error demoting user {}: {:?}", name, e))
            .unwrap_or_else(|e| panic!("Error demoting user {}: {:?}", name, e));

        info!("User {} successfully demoted!", name);
    }
}

#[inline]
fn reply_err(err: AuthError) -> Result<Box<dyn warp::Reply>, Infallible> {
    Ok(Box::new(AuthResponse::Err(err).into(): Vec<u8>))
}

#[inline]
fn reply_protobuf(res: AuthResponse) -> Result<Box<dyn warp::Reply>, Infallible> {
    Ok(Box::new(res.into(): Vec<u8>))
}

async fn login(
    global: Global,
    ws: warp::ws::Ws,
    login: Login,
) -> Result<impl warp::Reply, AuthError> {
    let authenticator = Authenticator {
        global: global.clone(),
    };

    let details = authenticator.login(login.device, login.token).await?;
    let (user, device, perms, hsv) = details;

    match client::session::insert(global.database.clone(), user, device, hsv).await? {
        Ok(_) => {
            let upgrade = ws.on_upgrade(move |websocket| {
                let (sink, stream) = websocket.split();

                let session = ActiveSession::new(sink, global, user, device, perms);
                session.clone().into_address().attach_stream(stream.map(WsMessage));

                // if the session fails to spawn, that means it has since been removed. we can ignore the error.
                let _ = client::session::upgrade(user, device, session);

                futures::future::ready(())
            });

            Ok(upgrade)
        }
        Err(_) => Err(AuthError::TokenInUse),
    }
}

async fn register(global: Global, bytes: bytes::Bytes) -> AuthResponse {
    let register = match AuthRequest::from_protobuf_bytes(&bytes)? {
        AuthRequest::RegisterUser(register) => register,
        _ => return AuthResponse::Err(AuthError::WrongEndpoint),
    };

    let credentials = register.credentials;
    let display_name = register
        .display_name
        .unwrap_or_else(|| credentials.username.clone());

    let authenticator = Authenticator { global };
    authenticator.create_user(credentials, display_name).await
}

async fn create_token(global: Global, bytes: bytes::Bytes) -> AuthResponse {
    let create_token = match AuthRequest::from_protobuf_bytes(&bytes)? {
        AuthRequest::CreateToken(create) => create,
        _ => return AuthResponse::Err(AuthError::WrongEndpoint),
    };

    let authenticator = Authenticator { global };
    authenticator
        .create_token(create_token.credentials, create_token.options)
        .await
}

async fn refresh_token(global: Global, bytes: bytes::Bytes) -> AuthResponse {
    let refresh_token = match AuthRequest::from_protobuf_bytes(&bytes)? {
        AuthRequest::RefreshToken(refresh) => refresh,
        _ => return AuthResponse::Err(AuthError::WrongEndpoint),
    };

    let authenticator = Authenticator { global };
    authenticator
        .refresh_token(refresh_token.credentials, refresh_token.device)
        .await
}

async fn revoke_token(global: Global, bytes: bytes::Bytes) -> AuthResponse {
    let revoke_token = match AuthRequest::from_protobuf_bytes(&bytes)? {
        AuthRequest::RevokeToken(revoke) => revoke,
        _ => return AuthResponse::Err(AuthError::WrongEndpoint),
    };

    let authenticator = Authenticator { global };
    authenticator
        .revoke_token(revoke_token.credentials, revoke_token.device)
        .await
}

async fn change_password(global: Global, bytes: bytes::Bytes) -> AuthResponse {
    let change = match AuthRequest::from_protobuf_bytes(&bytes)? {
        AuthRequest::ChangePassword(change) => change,
        _ => return AuthResponse::Err(AuthError::WrongEndpoint),
    };

    let credentials = Credentials {
        username: change.username,
        password: change.old_password,
    };

    let authenticator = Authenticator { global };
    authenticator
        .change_password(credentials, change.new_password)
        .await
}

async fn invite_reply(
    global: Global,
    //  hostname: String, // https://github.com/seanmonstar/warp/issues/432
    invite_code: String,
) -> Result<Box<dyn Reply>, Infallible> {
    let res = invite(global, invite_code).await;

    match res {
        Ok(Ok(html)) => Ok(Box::new(warp::reply::html(html))),
        _ => {
            let response = http::response::Builder::new()
                .status(404) // Not found
                .body("")
                .unwrap();
            Ok(Box::new(response))
        }
    }
}

async fn invite(
    global: Global,
    //  hostname: String, // https://github.com/seanmonstar/warp/issues/432
    invite_code: String,
) -> DbResult<Result<String, MalformedInviteCode>> {
    let code = InviteCode(invite_code.clone());
    let id = match global.database.get_community_from_invite_code(code).await? {
        Ok(Some(id)) => id,
        _ => return Ok(Err(MalformedInviteCode)),
    };
    let community_record = match global.database.get_community_metadata(id).await? {
        Some(rec) => rec,
        None => return Ok(Err(MalformedInviteCode)),
    };

    let html = format!(
        r#"
        <!DOCTYPE html>
        <html>
            <head>
                <meta charset="UTF-8">
                <meta property="vertex:invite_code" content="{invite_code}">
                <meta property="vertex:invite_name" content="{community}">
                <meta property="vertex:invite_description" content="{description}">
                <meta property="og:title" content="Vertex Community Invite">
                <meta property="og:description" content="You are invited to join {community} on Vertex!">
            </head>
            <body>
                <script type="text/javascript">
                    // Redirect to vertex://...
                    const url = new URL(location);
                    url.protocol = "vertex:";
                    alert(url);
                    location.replace(url);
                </script>
            </body>
        </html>
        "#,
        //        hostname = hostname, // TODO https://github.com/seanmonstar/warp/issues/432
        // We just use JS as a workaround
        invite_code = invite_code,
        community = community_record.name,
        description = Community::desc_or_default(&community_record.description),
    );

    Ok(Ok(html))
}
