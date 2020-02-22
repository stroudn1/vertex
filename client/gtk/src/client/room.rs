use vertex::*;

use crate::{Client, Error, Result, SharedMut};

use super::ClientUi;
use super::message::*;

pub trait RoomEntryWidget<Ui: ClientUi>: Clone {
    fn bind_events(&self, room: &RoomEntry<Ui>);
}

pub struct RoomState {
    pub message_buffer: MessageRingBuffer,
    pub last_read: Option<MessageId>,
}

#[derive(Clone)]
pub struct RoomEntry<Ui: ClientUi> {
    pub client: Client<Ui>,

    pub widget: Ui::RoomEntryWidget,

    pub community: CommunityId,
    pub id: RoomId,

    pub name: String,

    pub state: SharedMut<RoomState>,
}

impl<Ui: ClientUi> RoomEntry<Ui> {
    pub(super) fn new(
        client: Client<Ui>,
        widget: Ui::RoomEntryWidget,
        community: CommunityId,
        id: RoomId,
        name: String,
    ) -> Self {
        let state = SharedMut::new(RoomState {
            message_buffer: MessageRingBuffer::new(MESSAGE_PAGE_SIZE),
            last_read: None,
        });

        RoomEntry { client, widget, community, id, name, state }
    }

    pub(crate) async fn get_updates(&self) -> Result<RoomUpdate> {
        let last_received = self.state.read().await.message_buffer.last();

        let request = self.client.request.send(ClientRequest::GetRoomUpdate {
            community: self.community,
            room: self.id,
            last_received,
            message_count: MESSAGE_PAGE_SIZE,
        }).await;

        match request.response().await? {
            OkResponse::RoomUpdate(update) => Ok(update),
            _ => Err(Error::UnexpectedMessage),
        }
    }

    pub async fn send_message(&self, content: String) {
        let user = self.client.user.id;
        let profile = self.client.user.profile().await;
        let profile_version = profile.version;

        if let Some(chat) = self.client.chat_for(self.id).await {
            let widget = chat.push_raw(
                MessageContent {
                    author: user,
                    profile,
                    text: content.clone(),
                }
            ).await;

            widget.set_status(MessageStatus::Pending);

            let result = self.send_message_request(content.clone()).await;
            match result {
                Ok(confirmation) => {
                    widget.set_status(MessageStatus::Ok);
                    self.push_message(Message {
                        id: confirmation.id,
                        author: user,
                        author_profile_version: profile_version,
                        sent: confirmation.time,
                        content,
                    }).await;
                }
                Err(_) => widget.set_status(MessageStatus::Err),
            }
        }
    }

    async fn send_message_request(&self, content: String) -> Result<MessageConfirmation> {
        let request = ClientRequest::SendMessage(ClientSentMessage {
            to_community: self.community,
            to_room: self.id,
            content,
        });

        let request = self.client.request.send(request).await;
        match request.response().await? {
            OkResponse::ConfirmMessage(confirmation) => Ok(confirmation),
            _ => Err(Error::UnexpectedMessage),
        }
    }

    pub async fn push_message(&self, message: Message) {
        let mut state = self.state.write().await;
        state.message_buffer.push(message);
    }

    pub async fn update(&self, update: &RoomUpdate) {
        let mut state = self.state.write().await;
        state.last_read = update.last_read;

        if !update.continuous {
            state.message_buffer.clear();
        }

        // TODO: potentially wasteful; we only need to look at last 50 messages
        for message in &update.new_messages.messages {
            state.message_buffer.push(message.clone());
        }
    }

    pub async fn mark_as_read(&self) {
        // only mark as read if we had unread messages
        if !self.has_unread_messages().await {
            return;
        }

        let mut state = self.state.write().await;
        state.last_read = state.message_buffer.last();

        self.client.request.send(ClientRequest::SetAsRead {
            community: self.community,
            room: self.id,
        }).await;
    }

    pub async fn has_unread_messages(&self) -> bool {
        let state = self.state.read().await;
        match state.message_buffer.last() {
            Some(last) => Some(last) != state.last_read,
            None => false,
        }
    }

    pub async fn push(&self, message: Message) {
        let mut state = self.state.write().await;
        state.message_buffer.push(message);
    }

    pub async fn collect_recent_history(&self) -> Vec<Message> {
        let state = self.state.read().await;
        state.message_buffer.iter().cloned().collect()
    }

    pub async fn request_messages(&self, selector: MessageSelector, count: usize) -> Result<MessageHistory> {
        let request = ClientRequest::GetMessages {
            community: self.community,
            room: self.id,
            selector,
            count,
        };
        let request = self.client.request.send(request).await;

        match request.response().await? {
            OkResponse::MessageHistory(messages) => Ok(messages),
            _ => Err(Error::UnexpectedMessage),
        }
    }
}

impl<Ui: ClientUi> PartialEq<RoomEntry<Ui>> for RoomEntry<Ui> {
    fn eq(&self, other: &RoomEntry<Ui>) -> bool {
        self.id == other.id && self.community == other.community
    }
}

impl<Ui: ClientUi> Eq for RoomEntry<Ui> {}
