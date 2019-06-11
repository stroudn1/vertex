use actix::dev::{MessageResponse, ResponseChannel};
use actix::prelude::*;
use ccl::dhashmap::DHashMap;
use std::fmt::Debug;
use uuid::Uuid;
use vertex_common::*;
use super::{ClientWsSession, SessionId};
use crate::SendMessage;

struct Room {
    users: Vec<UserId>,
}

impl Room {
    fn new(creator: UserId) -> Self {
        Room {
            users: vec![creator],
        }
    }

    fn add(&mut self, user: UserId) {
        self.users.push(user)
    }
}

#[derive(Message)]
pub struct Connect {
    pub session: Addr<ClientWsSession>,
    pub session_id: SessionId,
    pub login: Login,
}

#[derive(Message)]
pub struct Disconnect {
    pub session_id: SessionId,
    pub user_id: Option<UserId>,
}

#[derive(Debug, Message)]
pub struct Join {
    pub room: RoomId,
}

impl ClientMessageType for Join {}

impl MessageResponse<ClientServer, IdentifiedMessage<CreateRoom>> for RoomId {
    fn handle<R: ResponseChannel<IdentifiedMessage<CreateRoom>>>(
        self,
        _: &mut Context<ClientServer>,
        tx: Option<R>,
    ) {
        if let Some(tx) = tx {
            tx.send(self)
        }
    }
}

#[derive(Debug)]
pub struct IdentifiedMessage<T: Message + ClientMessageType + Debug> {
    pub session_id: SessionId,
    pub user_id: UserId,
    pub msg: T,
}

impl<T: Message + ClientMessageType + Debug> Message for IdentifiedMessage<T> {
    type Result = T::Result;
}

#[derive(Debug)]
pub struct CreateRoom;

impl Message for CreateRoom {
    type Result = RoomId;
}

impl ClientMessageType for CreateRoom {}

pub struct ClientServer {
    sessions: DHashMap<SessionId, Addr<ClientWsSession>>,
    user_to_sessions: DHashMap<UserId, Vec<SessionId>>,
    rooms: DHashMap<RoomId, Room>,
}

impl ClientServer {
    pub fn new() -> Self {
        ClientServer {
            sessions: DHashMap::default(),
            user_to_sessions: DHashMap::default(),
            rooms: DHashMap::default(),
        }
    }

    fn send_to_room(&mut self, room: &RoomId, message: ServerMessage, sender: &SessionId) {
        let room = self.rooms.index(room);
        for user_id in room.users.iter() {
            if let Some(sessions) = self.user_to_sessions.get_mut(user_id) {
                sessions
                    .iter()
                    .filter(|id| **id != *sender)
                    .map(|id| self.sessions.get_mut(id).unwrap())
                    .for_each(|s| s.do_send(SendMessage { message: message.clone() }));
            }
        }
    }
}

impl Actor for ClientServer {
    type Context = Context<Self>;
}

impl Handler<Connect> for ClientServer {
    type Result = ();

    fn handle(&mut self, connect: Connect, _: &mut Context<Self>) {
        if let Some(mut sessions) = self.user_to_sessions.get_mut(&connect.login.id) {
            sessions.push(connect.session_id);
        } else {
            self.user_to_sessions.insert(connect.login.id, vec![connect.session_id]);
        }

        self.sessions.insert(connect.session_id, connect.session); // TODO multiple clients per user
    }
}

impl Handler<Disconnect> for ClientServer {
    type Result = ();

    fn handle(&mut self, disconnect: Disconnect, _: &mut Context<Self>) {
        if let Some(user_id) = disconnect.user_id {
            let mut sessions = self.user_to_sessions.get_mut(&user_id).unwrap();

            let idx = sessions.iter().position(|i| *i == disconnect.session_id).unwrap();
            sessions.remove(idx);

            if sessions.len() == 0 {
                self.user_to_sessions.remove(&user_id);
            }
        }

        self.sessions.remove(&disconnect.session_id); // TODO multiple clients per user
    }
}

impl Handler<IdentifiedMessage<ClientSentMessage>> for ClientServer {
    type Result = ();

    fn handle(&mut self, m: IdentifiedMessage<ClientSentMessage>, _: &mut Context<Self>) {
        println!("msg: {:?}", m);
        let author_id = m.session_id;
        self.send_to_room(
            &m.msg.to_room.clone(),
            ServerMessage::Message(ForwardedMessage::from_message_and_author(m.msg, m.user_id)),
            &author_id,
        );
    }
}

impl Handler<IdentifiedMessage<CreateRoom>> for ClientServer {
    type Result = RoomId;

    fn handle(&mut self, m: IdentifiedMessage<CreateRoom>, _: &mut Context<Self>) -> RoomId {
        let id = RoomId(Uuid::new_v4());
        self.rooms.insert(id, Room::new(m.user_id));

        id
    }
}

impl Handler<IdentifiedMessage<Join>> for ClientServer {
    type Result = ();

    fn handle(&mut self, m: IdentifiedMessage<Join>, _: &mut Context<Self>) {
        self.rooms.get_mut(&m.msg.room).unwrap().add(m.user_id); // TODO don't unwrap
    }
}

impl Handler<IdentifiedMessage<Edit>> for ClientServer {
    type Result = ();

    fn handle(&mut self, m: IdentifiedMessage<Edit>, _: &mut Context<Self>) {
        let room_id = m.msg.room_id;
        self.send_to_room(&room_id, ServerMessage::Edit(m.msg), &m.session_id);
    }
}

impl Handler<IdentifiedMessage<Delete>> for ClientServer {
    type Result = ();

    fn handle(&mut self, m: IdentifiedMessage<Delete>, _: &mut Context<Self>) {
        let room_id = m.msg.room_id;
        self.send_to_room(&room_id, ServerMessage::Delete(m.msg), &m.session_id);
    }
}
