// TODO: how to split this into backend?

use tokio_tungstenite::WebSocketStream;

use vertex::*;

use crate::Server;

pub struct AuthenticatedWs {
    pub stream: AuthenticatedWsStream,
    pub device: DeviceId,
    pub token: AuthToken,
}

pub type AuthenticatedWsStream = WebSocketStream<hyper::upgrade::Upgraded>;

type Connector = hyper_tls::HttpsConnector<hyper::client::HttpConnector>;

pub struct Client {
    server: Server,
    client: hyper::Client<Connector>,
}

impl Client {
    pub fn new(server: Server) -> Client {
        let tls = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("failed to build tls connector");
        let tls = tokio_tls::TlsConnector::from(tls);

        let mut http = hyper::client::HttpConnector::new();
        http.enforce_http(false);

        let https = (http, tls).into();

        let client = hyper::client::Client::builder()
            .build(https);

        Client { server, client }
    }

    pub async fn authenticate(
        &self,
        device: DeviceId,
        token: AuthToken,
    ) -> Result<AuthenticatedWs> {
        let request = serde_urlencoded::to_string(AuthenticateRequest { device, token: token.clone() })?;
        let url = format!("{}/client/authenticate?{}", self.server.url(), request);

        let key: [u8; 16] = rand::random();
        let key = base64::encode(&key);

        let request = hyper::Request::builder()
            .uri(url.parse::<hyper::Uri>().unwrap())
            .header("upgrade", "websocket")
            .header("connection", "upgrade")
            .header("sec-websocket-key", key)
            .header("sec-websocket-version", "13")
            .body(hyper::Body::empty())
            .unwrap();

        let response = self.client.request(request).await?;

        match response.status() {
            hyper::StatusCode::SWITCHING_PROTOCOLS => {
                let body = response.into_body();
                let upgraded = body.on_upgrade().await?;

                let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    upgraded,
                    tungstenite::protocol::Role::Client,
                    None,
                ).await;

                Ok(AuthenticatedWs { stream: ws, device, token })
            }
            _ => {
                let body = response.into_body();
                let bytes = hyper::body::to_bytes(body).await?;

                match serde_cbor::from_slice::<AuthResult<()>>(&bytes)? {
                    Ok(_) => Err(Error::DidNotUpgrade),
                    Err(e) => Err(e.into()),
                }
            }
        }
    }

    pub async fn register(
        &self,
        credentials: UserCredentials,
        display_name: Option<String>,
    ) -> Result<RegisterUserResponse> {
        let response: AuthResult<RegisterUserResponse> = self.post(
            RegisterUserRequest { credentials, display_name },
            format!("{}/client/register", self.server.url()),
        ).await?;

        Ok(response?)
    }

    pub async fn create_token(
        &self,
        credentials: UserCredentials,
        options: TokenCreationOptions,
    ) -> Result<CreateTokenResponse> {
        let response: AuthResult<CreateTokenResponse> = self.post(
            CreateTokenRequest { credentials, options },
            format!("{}/client/token/create", self.server.url()),
        ).await?;

        Ok(response?)
    }

    pub async fn refresh_token(
        &self,
        credentials: UserCredentials,
        device: DeviceId,
    ) -> Result<()> {
        let response: AuthResult<()> = self.post(
            RefreshTokenRequest { credentials, device },
            format!("{}/client/token/refresh", self.server.url()),
        ).await?;
        Ok(response?)
    }

    pub async fn revoke_token(
        &self,
        credentials: UserCredentials,
        device: DeviceId,
    ) -> Result<()> {
        let response: AuthResult<()> = self.post(
            RevokeTokenRequest { credentials, device },
            format!("{}/client/token/revoke", self.server.url()),
        ).await?;
        Ok(response?)
    }

    async fn post<Req, Res>(&self, request: Req, url: String) -> Result<Res>
        where Req: serde::Serialize, Res: serde::de::DeserializeOwned
    {
        let request = hyper::Request::builder()
            .uri(url.parse::<hyper::Uri>().unwrap())
            .method(hyper::Method::POST)
            .body(hyper::Body::from(serde_cbor::to_vec(&request)?))
            .unwrap();

        let response = self.client.request(request).await?;
        let bytes = hyper::body::to_bytes(response.into_body()).await?;

        Ok(serde_cbor::from_slice(&bytes)?)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Server(AuthError),
    SerdeUrlEncoded(serde_urlencoded::ser::Error),
    SerdeCbor(serde_cbor::Error),
    Net(hyper::Error),
    DidNotUpgrade,
}

impl From<AuthError> for Error {
    fn from(error: AuthError) -> Self { Error::Server(error) }
}

impl From<serde_cbor::Error> for Error {
    fn from(error: serde_cbor::Error) -> Self { Error::SerdeCbor(error) }
}

impl From<serde_urlencoded::ser::Error> for Error {
    fn from(error: serde_urlencoded::ser::Error) -> Self { Error::SerdeUrlEncoded(error) }
}

impl From<hyper::Error> for Error {
    fn from(error: hyper::Error) -> Self { Error::Net(error) }
}
