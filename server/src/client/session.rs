use std::io::Cursor;
use actix::prelude::*;
use actix_web::web::Bytes;
use actix_web_actors::ws::{self, WebsocketContext};
use uuid::Uuid;
use vertex_common::*;
use super::*;
use crate::SendMessage;
use crate::federation::FederationServer;

#[derive(Eq, PartialEq)]
enum SessionState {
    WaitingForLogin,
    Ready(Uuid),
}

pub struct ClientWsSession {
    client_server: Addr<ClientServer>,
    federation_server: Addr<FederationServer>,
    state: SessionState,
}

impl ClientWsSession {
    pub fn new(client_server: Addr<ClientServer>, federation_server: Addr<FederationServer>) -> Self {
        ClientWsSession {
            client_server,
            federation_server,
            state: SessionState::WaitingForLogin,
        }
    }
}

impl ClientWsSession {
    fn handle_message(&mut self, msg: ClientMessage, ctx: &mut WebsocketContext<Self>) {
        let response: ServerMessage = match msg {
            ClientMessage::Login(login) => {
                // Register with the server
                self.client_server.do_send(Connect {
                    session: ctx.address(),
                    login: login.clone(),
                });
                self.state = SessionState::Ready(login.id);
                ServerMessage::success()
            },
            _ => self.handle_authenticated_message(msg, ctx),
        };

        let binary: Bytes = response.into();
        ctx.binary(binary);
    }

    fn handle_authenticated_message(
        &mut self,
        msg: ClientMessage,
        ctx: &mut WebsocketContext<Self>,
    ) -> ServerMessage {
        match self.state {
            SessionState::WaitingForLogin => ServerMessage::Error(ServerError::NotLoggedIn),
            SessionState::Ready(id) => {
                match msg {
                    ClientMessage::SendMessage(msg) => {
                        self.client_server.do_send(IdentifiedMessage { id, msg });
                        ServerMessage::success()
                    },
                    ClientMessage::EditMessage(edit) => {
                        self.client_server.do_send(IdentifiedMessage { id, msg: edit });
                        ServerMessage::success()
                    },
                    ClientMessage::JoinRoom(room) => { // TODO check that it worked lol
                        self.client_server.do_send(IdentifiedMessage { id, msg: Join { room } });
                        ServerMessage::success()
                    },
                    ClientMessage::CreateRoom => {
                        let id = self.client_server.send(IdentifiedMessage { id, msg: CreateRoom })
                            .wait()
                            .unwrap();
                        ServerMessage::Success(Success::Room { id: *id })
                    }
                    ClientMessage::PublishInitKey(publish) => {
                        self.client_server.do_send(publish);
                        ServerMessage::success()
                    },
                    ClientMessage::RequestInitKey(request) => {
                        match self.client_server.send(request).wait() {
                            // Key returned
                            Ok(Some(key)) => ServerMessage::Success(Success::Key(key)),
                            // No key
                            Ok(None) => ServerMessage::Error(ServerError::IdNotFound),
                            // Internal error (with actor?)
                            Err(_) => ServerMessage::Error(ServerError::Internal),
                        }
                    },
                    _ => unimplemented!(),
                }
            }
        }
    }
}

impl Actor for ClientWsSession {
    type Context = WebsocketContext<Self>;
}

impl StreamHandler<ws::Message, ws::ProtocolError> for ClientWsSession {
    fn handle(&mut self, msg: ws::Message, ctx: &mut WebsocketContext<Self>) {
        match msg {
            ws::Message::Ping(msg) => ctx.pong(&msg),
            ws::Message::Text(_) => {
                let error = serde_cbor::to_vec(&ServerMessage::Error(ServerError::UnexpectedTextFrame))
                    .unwrap();
                ctx.binary(error);
            },
            ws::Message::Binary(bin) => {
                let mut bin = Cursor::new(bin);
                let msg = match serde_cbor::from_reader(&mut bin) {
                    Ok(m) => m,
                    Err(_) => {
                        let error = serde_cbor::to_vec(&ServerMessage::Error(ServerError::InvalidMessage))
                            .unwrap();
                        return ctx.binary(error);
                    }
                };

                self.handle_message(msg, ctx);
            },
            _ => (),
        }
    }
}

impl Handler<SendMessage<ServerMessage>> for ClientWsSession {
    type Result = ();

    fn handle(&mut self, msg: SendMessage<ServerMessage>, ctx: &mut WebsocketContext<Self>) {
        ctx.binary(msg);
    }
}
