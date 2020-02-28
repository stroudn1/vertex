//! Methods that can be executed by regular users

use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use xtra::Context;

use crate::{auth, handle_disconnected, IdentifiedMessage};
use crate::client::ActiveSession;
use crate::client::session::{manager, UserCommunity, UserRoom};
use crate::community::COMMUNITIES;
use crate::community::CommunityActor;

use super::*;

pub struct RequestHandler<'a> {
    pub session: &'a mut ActiveSession,
    pub ctx: &'a mut Context<ActiveSession>,
    pub user: UserId,
    pub device: DeviceId,
    pub perms: TokenPermissionFlags,
}

impl<'a> RequestHandler<'a> {
    pub async fn handle_request(self, request: ClientRequest) -> ResponseResult {
        match request {
            ClientRequest::SendMessage(message) => self.send_message(message).await,
            ClientRequest::EditMessage(edit) => self.edit_message(edit).await,
            ClientRequest::JoinCommunity(code) => self.join_community(code).await,
            ClientRequest::CreateCommunity { name } => self.create_community(name).await,
            ClientRequest::LogOut => self.log_out().await,
            ClientRequest::GetUserProfile(id) => self.get_user_profile(id).await,
            ClientRequest::ChangeUsername { new_username } => {
                self.change_username(new_username).await
            }
            ClientRequest::ChangeDisplayName { new_display_name } => {
                self.change_display_name(new_display_name).await
            }
            ClientRequest::ChangePassword {
                old_password,
                new_password,
            } => self.change_password(old_password, new_password).await,
            ClientRequest::CreateRoom { name, community } => {
                self.create_room(name, community).await
            }
            ClientRequest::CreateInvite {
                community,
                expiration_date,
            } => self.create_invite(community, expiration_date).await,
            ClientRequest::GetRoomUpdate { community, room, last_received, message_count } => {
                self.get_room_update(community, room, last_received, message_count).await
            }
            ClientRequest::SelectRoom { community, room } => {
                self.select_room(community, room).await
            }
            ClientRequest::DeselectRoom => self.deselect_room().await,
            ClientRequest::GetMessages {
                community,
                room,
                selector,
                count,
            } => self.get_messages(community, room, selector, count).await,
            ClientRequest::SetAsRead { community, room } => self.set_as_read(community, room).await,
            _ => unimplemented!(),
        }
    }

    async fn verify_password(&mut self, password: String) -> Result<(), ErrResponse> {
        let user = match self
            .session
            .global
            .database
            .get_user_by_id(self.user)
            .await?
        {
            Some(user) => user,
            None => return Err(ErrResponse::InvalidUser),
        };

        if auth::verify_user(user, password).await {
            Ok(())
        } else {
            Err(ErrResponse::IncorrectUsernameOrPassword)
        }
    }

    async fn send_message(self, message: ClientSentMessage) -> ResponseResult {
        if !self.perms.has_perms(TokenPermissionFlags::SEND_MESSAGES) {
            return Err(ErrResponse::AccessDenied);
        }

        if !self.session.in_community(&message.to_community) {
            return Err(ErrResponse::InvalidCommunity);
        }

        match COMMUNITIES.get(&message.to_community) {
            Some(community) => {
                let message = IdentifiedMessage {
                    user: self.user,
                    device: self.device,
                    message,
                };
                let confirmation = community
                    .actor
                    .send(message)
                    .await
                    .map_err(handle_disconnected("Community"))??;

                Ok(OkResponse::ConfirmMessage(confirmation))
            }
            _ => Err(ErrResponse::InvalidCommunity),
        }
    }

    async fn edit_message(self, edit: Edit) -> ResponseResult {
        if !self.perms.has_perms(TokenPermissionFlags::SEND_MESSAGES) {
            return Err(ErrResponse::AccessDenied);
        }

        if !self.session.in_community(&edit.community) {
            return Err(ErrResponse::InvalidCommunity);
        }

        if let Some(community) = COMMUNITIES.get(&edit.community) {
            let message = IdentifiedMessage {
                user: self.user,
                device: self.device,
                message: edit,
            };
            community
                .actor
                .send(message)
                .await
                .map_err(handle_disconnected("Community"))??;
            Ok(OkResponse::NoData)
        } else {
            Err(ErrResponse::InvalidCommunity)
        }
    }

    async fn log_out(self) -> ResponseResult {
        if let Err(NonexistentDevice) = self
            .session
            .global
            .database
            .revoke_token(self.device)
            .await?
        {
            return Err(ErrResponse::DeviceDoesNotExist);
        }

        self.ctx.notify_immediately(LogoutThisSession);

        Ok(OkResponse::NoData)
    }

    async fn get_user_profile(self, id: UserId) -> ResponseResult {
        match self.session.global.database.get_user_profile(id).await? {
            Some(profile) => Ok(OkResponse::Profile(profile)),
            None => Err(ErrResponse::InvalidUser),
        }
    }

    async fn change_username(self, new_username: String) -> ResponseResult {
        if !self.perms.has_perms(TokenPermissionFlags::CHANGE_USERNAME) {
            return Err(ErrResponse::AccessDenied);
        }

        let new_username = match auth::prepare_username(&new_username, &self.session.global.config)
        {
            Ok(name) => name,
            Err(auth::TooShort) => return Err(ErrResponse::InvalidUsername),
        };

        let database = &self.session.global.database;
        match database.change_username(self.user, new_username).await? {
            Ok(()) => Ok(OkResponse::NoData),
            Err(ChangeUsernameError::UsernameConflict) => Err(ErrResponse::UsernameAlreadyExists),
            Err(ChangeUsernameError::NonexistentUser) => {
                self.ctx.stop(); // The user did not exist at the time of request
                Err(ErrResponse::UserDeleted)
            }
        }
    }

    async fn change_display_name(self, new_display_name: String) -> ResponseResult {
        if !self
            .perms
            .has_perms(TokenPermissionFlags::CHANGE_DISPLAY_NAME)
        {
            return Err(ErrResponse::AccessDenied);
        }

        if !auth::valid_display_name(&new_display_name, &self.session.global.config) {
            return Err(ErrResponse::InvalidDisplayName);
        }

        let database = &self.session.global.database;
        match database
            .change_display_name(self.user, new_display_name)
            .await?
        {
            Ok(()) => Ok(OkResponse::NoData),
            Err(_) => {
                self.ctx.stop(); // The user did not exist at the time of request
                Err(ErrResponse::UserDeleted)
            }
        }
    }

    async fn change_password(
        mut self,
        old_password: String,
        new_password: String,
    ) -> ResponseResult {
        if !auth::valid_password(&new_password, &self.session.global.config) {
            return Err(ErrResponse::InvalidPassword);
        }

        self.verify_password(old_password).await?;

        let (new_password_hash, hash_version) = auth::hash(new_password).await;

        let database = &self.session.global.database;
        let res = database
            .change_password(self.user, new_password_hash, hash_version)
            .await?;

        match res {
            Ok(()) => Ok(OkResponse::NoData),
            Err(_) => {
                self.ctx.stop(); // The user did not exist at the time of request
                Err(ErrResponse::UserDeleted)
            }
        }
    }

    async fn create_community(self, name: String) -> ResponseResult {
        if !self
            .perms
            .has_perms(TokenPermissionFlags::CREATE_COMMUNITIES)
        {
            return Err(ErrResponse::AccessDenied);
        }

        let db = &self.session.global.database;
        let id = db.create_community(name.clone()).await?;
        let res = db
            .create_default_user_room_states_for_user(id, self.user)
            .await?;

        match res {
            Ok(_) => {
                CommunityActor::create_and_spawn(name, id, db.clone(), self.user);
                self.join_community_by_id(id).await
            }
            Err(_) => {
                self.ctx.stop(); // The user did not exist at the time of request
                Err(ErrResponse::UserDeleted)
            }
        }
    }

    async fn join_community(self, code: InviteCode) -> ResponseResult {
        if !self.perms.has_perms(TokenPermissionFlags::JOIN_COMMUNITIES) {
            return Err(ErrResponse::AccessDenied);
        }

        if code.0.len() > 11 {
            return Err(ErrResponse::InvalidInviteCode);
        }

        let database = &self.session.global.database;
        let id = match database.get_community_from_invite_code(code).await? {
            Ok(Some(id)) => id,
            Ok(None) | Err(_) => return Err(ErrResponse::InvalidInviteCode),
        };

        self.join_community_by_id(id).await
    }

    async fn join_community_by_id(self, id: CommunityId) -> ResponseResult {
        if let Some(community) = COMMUNITIES.get(&id) {
            let join = Join {
                user: self.user,
                device_id: self.device,
                session: self.ctx.address().unwrap(),
            };

            let res = community
                .actor
                .send(join)
                .await
                .map_err(handle_disconnected("Community"))??;

            match res {
                Ok(community) => {
                    if let Some(mut user) = manager::get_active_user_mut(self.user) {
                        let db = &self.session.global.database;
                        let user_community = UserCommunity::load(db, self.user, id).await?;
                        user.communities.insert(community.id, user_community);

                        let community = community.clone();
                        let send = ServerMessage::Event(ServerEvent::AddCommunity(community));
                        let sessions = user.sessions.iter();

                        sessions
                            .filter(|(id, _)| **id != self.device)
                            .filter_map(|(_, session)| session.as_active_actor())
                            .for_each(|addr| {
                                let _ = addr.do_send(SendMessage(send.clone()));
                            });
                    }

                    Ok(OkResponse::AddCommunity(community))
                }
                Err(AddToCommunityError::AlreadyInCommunity) => {
                    Err(ErrResponse::AlreadyInCommunity)
                }
                Err(AddToCommunityError::InvalidCommunity) => Err(ErrResponse::InvalidCommunity),
                Err(AddToCommunityError::InvalidUser) => Err(ErrResponse::InvalidUser),
            }
        } else {
            Err(ErrResponse::InvalidCommunity)
        }
    }

    async fn create_room(self, name: String, community: CommunityId) -> ResponseResult {
        if !self.perms.has_perms(TokenPermissionFlags::CREATE_ROOMS) {
            return Err(ErrResponse::AccessDenied);
        }

        if !self.session.in_community(&community) {
            return Err(ErrResponse::InvalidCommunity);
        }

        let community_id = community;

        if let Some(community) = COMMUNITIES.get(&community) {
            let create = CreateRoom {
                creator: self.device,
                name: name.clone(),
            };
            let id = community
                .actor
                .send(create)
                .await
                .map_err(handle_disconnected("Community"))??;

            let mut user = manager::get_active_user_mut(self.user).unwrap();

            if let Some(community) = user.communities.get_mut(&community_id) {
                let room = RoomStructure {
                    id,
                    name,
                    unread: true,
                };

                community.rooms.insert(
                    room.id,
                    UserRoom {
                        watch_level: WatchLevel::default(),
                        unread: true,
                    },
                );

                return Ok(OkResponse::AddRoom {
                    community: community_id,
                    room,
                });
            }
        }

        Err(ErrResponse::InvalidCommunity)
    }

    async fn create_invite(
        self,
        id: CommunityId,
        expiration_date: Option<DateTime<Utc>>,
    ) -> ResponseResult {
        if !self.perms.has_perms(TokenPermissionFlags::CREATE_INVITES) {
            return Err(ErrResponse::AccessDenied);
        }

        if !self.session.in_community(&id) {
            return Err(ErrResponse::InvalidCommunity);
        }

        if COMMUNITIES.contains_key(&id) {
            let db = &self.session.global.database;
            let max = self.session.global.config.max_invite_codes_per_community as i64;
            let res = db.create_invite_code(id, expiration_date, max).await?;

            match res {
                Ok(code) => Ok(OkResponse::NewInvite(code)),
                Err(_) => Err(ErrResponse::TooManyInviteCodes),
            }
        } else {
            Err(ErrResponse::InvalidCommunity)
        }
    }

    async fn get_room_update(
        self,
        community: CommunityId,
        room: RoomId,
        last_received: Option<MessageId>,
        message_count: usize,
    ) -> ResponseResult {
        if !self.session.in_room(&community, &room) {
            return Err(ErrResponse::InvalidRoom);
        }

        let db = &self.session.global.database;

        let newest_message = db.get_newest_message(community, room).await?;
        let last_read = db.get_last_read(self.user, room).await?;

        let selector = match (last_received, newest_message) {
            (Some(last_received), _) => Some(
                MessageSelector::After(
                    Bound::Exclusive(last_received)
                )
            ),
            (_, Some(newest_message)) => Some(
                MessageSelector::Before(
                    Bound::Inclusive(newest_message)
                )
            ),
            _ => None,
        };

        let new_messages = match selector {
            Some(selector) => {
                let messages = db.get_messages(community, room, selector, message_count)
                    .await?
                    .map_err(|_| ErrResponse::InvalidMessageSelector)?;
                messages.map_messages().try_collect().await?
            },
            None => Vec::new(),
        };

        let continuous = new_messages.len() < message_count;

        let new_messages = MessageHistory::from_newest_to_oldest(new_messages);

        Ok(OkResponse::RoomUpdate(RoomUpdate {
            last_read,
            continuous,
            new_messages,
        }))
    }

    async fn select_room(self, community: CommunityId, room: RoomId) -> ResponseResult {
        if !self.session.in_room(&community, &room) {
            return Err(ErrResponse::InvalidRoom);
        }

        self.set_looking_at(Some((community, room))).await;
        Ok(OkResponse::NoData)
    }

    async fn deselect_room(self) -> ResponseResult {
        self.set_looking_at(None).await;
        Ok(OkResponse::NoData)
    }

    async fn set_looking_at(self, looking_at: Option<(CommunityId, RoomId)>) {
        let mut active_user = manager::get_active_user_mut(self.user).unwrap();
        let session = active_user.sessions.get_mut(&self.device).unwrap();
        session.set_looking_at(looking_at).unwrap();
    }

    async fn get_messages(
        self,
        community: CommunityId,
        room: RoomId,
        selector: MessageSelector,
        count: usize,
    ) -> ResponseResult {
        if !self.session.in_room(&community, &room) {
            return Err(ErrResponse::InvalidRoom);
        }

        let db = &self.session.global.database;
        let stream = db
            .get_messages(community, room, selector, count)
            .await?
            .map_err(|_| ErrResponse::InvalidMessageSelector)?;

        let messages = stream.map_messages().try_collect().await?;
        Ok(OkResponse::MessageHistory(MessageHistory::from_newest_to_oldest(messages)))
    }

    async fn set_as_read(self, community: CommunityId, room: RoomId) -> ResponseResult {
        let mut active_user = manager::get_active_user_mut(self.user).unwrap();
        if let Some(user_community) = active_user.communities.get_mut(&community) {
            if let Some(user_room) = user_community.rooms.get_mut(&room) {
                user_room.unread = false;
            } else {
                return Err(ErrResponse::InvalidRoom);
            }
        } else {
            return Err(ErrResponse::InvalidCommunity);
        }

        let db = &self.session.global.database;
        let res = db.set_room_read(room, self.user).await?;

        match res {
            Ok(_) => Ok(OkResponse::NoData),
            Err(SetUserRoomStateError::InvalidRoom) => Err(ErrResponse::InvalidRoom),
            Err(SetUserRoomStateError::InvalidUser) => {
                self.ctx.stop(); // The user did not exist at the time of request
                Err(ErrResponse::UserDeleted)
            }
        }
    }
}
