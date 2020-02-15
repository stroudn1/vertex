use std::env;
use std::rc::Rc;
use std::sync::Arc;

use ears::{AudioController, Sound};
use futures::{Stream, StreamExt};
use futures::lock::Mutex;

pub use community::*;
pub use message::*;
pub use room::*;
pub use user::*;
use vertex::*;

use crate::{net, SharedMut};
use crate::{Error, Result};

mod community;
mod room;
mod user;
mod message;

pub const HEARTBEAT_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_secs(2);

pub trait ClientUi: Sized + Clone + 'static {
    type CommunityEntryWidget: CommunityEntryWidget<Self>;
    type RoomEntryWidget: RoomEntryWidget<Self>;

    type MessageListWidget: MessageListWidget<Self>;
    type MessageEntryWidget: MessageEntryWidget<Self>;

    fn add_community(&self, name: String) -> Self::CommunityEntryWidget;
    fn build_message_list(&self) -> Self::MessageListWidget;
    fn window_focused(&self) -> bool;
}

async fn client_ready<S>(event_receiver: &mut S) -> Result<ClientReady>
    where S: Stream<Item = tungstenite::Result<ServerEvent>> + Unpin
{
    if let Some(result) = event_receiver.next().await {
        let event = result?;
        match event {
            ServerEvent::ClientReady(ready) => Ok(ready),
            _ => Err(Error::UnexpectedMessage),
        }
    } else {
        Err(Error::Websocket(tungstenite::Error::ConnectionClosed))
    }
}

pub struct ClientState<Ui: ClientUi> {
    pub communities: Vec<CommunityEntry<Ui>>,

    selected_room: Option<RoomEntry<Ui>>,
}

#[derive(Clone)]
pub struct Client<Ui: ClientUi> {
    request: Rc<net::RequestSender>,

    pub ui: Ui,
    pub user: User,
    pub message_list: MessageList<Ui>,

    pub notif_sound: Option<Arc<Mutex<Sound>>>,

    state: SharedMut<ClientState<Ui>>,
}

impl<Ui: ClientUi> Client<Ui> {
    pub async fn start(ws: net::AuthenticatedWs, ui: Ui) -> Result<Client<Ui>> {
        let (sender, receiver) = net::from_ws(ws.stream);

        let req_manager = net::RequestManager::new();

        let request = req_manager.sender(sender);
        let request = Rc::new(request);

        let mut event_receiver = req_manager.receive_from(receiver);

        let ready = client_ready(&mut event_receiver).await?;

        let user = User::new(
            request.clone(),
            ready.user,
            ready.username,
            ready.display_name,
            ws.device,
            ws.token,
        );

        let message_list = MessageList::new(ui.build_message_list());

        let state = SharedMut::new(ClientState {
            communities: Vec::new(),
            selected_room: None,
        });

        let notif_sound = match Sound::new("res/notification_sound_clearly.ogg") {
            Ok(s) => Some(Arc::new(Mutex::new(s))),
            Err(_) => None
        };

        let client = Client { request, ui, user, message_list, notif_sound, state };

        for community in ready.communities {
            client.add_community(community).await;
        }

        let ctx = glib::MainContext::ref_thread_default();
        ctx.spawn_local(ClientLoop {
            client: client.clone(),
            event_receiver,
        }.run());

        Ok(client)
    }

    pub async fn handle_event(&self, event: ServerEvent) {
        match event.clone() {
            ServerEvent::AddCommunity(structure) => {
                self.add_community(structure).await;
            }
            ServerEvent::AddRoom { community, structure } => {
                if let Some(community) = self.community_by_id(community).await {
                    community.add_room(structure).await;
                } else {
                    println!("received AddRoom for invalid community: {:?}", community);
                }
            }
            ServerEvent::AddMessage(message) => {
                let room = match self.community_by_id(message.community).await {
                    Some(community) => community.room_by_id(message.room).await,
                    None => None,
                };

                if let Some(room) = room {
                    room.add_message(message.author, message.content).await;

                    if !self.ui.window_focused() || self.selected_room().await != Some(room) {
                        self.system_notification(&event).await;
                    }
                } else {
                    println!("received message for invalid room: {:?}#{:?}", message.community, message.room);
                }
            }
            unexpected => println!("unhandled server event: {:?}", unexpected),
        }
    }

    pub async fn handle_network_err(&self, err: tungstenite::Error) {
        println!("network error: {:?}", err);
    }

    pub async fn create_community(&self, name: &str) -> Result<CommunityEntry<Ui>> {
        let request = ClientRequest::CreateCommunity { name: name.to_owned() };
        let request = self.request.send(request).await?;

        match request.response().await? {
            OkResponse::AddCommunity { community } => Ok(self.add_community(community).await),
            _ => Err(Error::UnexpectedMessage),
        }
    }

    pub async fn join_community(&self, invite: InviteCode) -> Result<CommunityEntry<Ui>> {
        let request = ClientRequest::JoinCommunity(invite);
        let request = self.request.send(request).await?;

        match request.response().await? {
            OkResponse::AddCommunity { community } => Ok(self.add_community(community).await),
            _ => Err(Error::UnexpectedMessage),
        }
    }

    pub async fn get_user_profile(&self, user: UserId) -> Result<UserProfile> {
        let request = ClientRequest::GetUserProfile(user);
        let request = self.request.send(request).await?;

        match request.response().await? {
            OkResponse::UserProfile(profile) => Ok(profile),
            _ => Err(Error::UnexpectedMessage),
        }
    }

    async fn add_community(&self, community: CommunityStructure) -> CommunityEntry<Ui> {
        let widget = self.ui.add_community(community.name.clone());

        let entry: CommunityEntry<Ui> = CommunityEntry::new(
            self.clone(),
            widget,
            community.id,
            community.name,
        );

        entry.widget.bind_events(&entry);

        for room in community.rooms {
            entry.add_room(room).await;
        }

        let mut state = self.state.write().await;
        state.communities.push(entry);
        state.communities.last().unwrap().clone()
    }

    pub async fn community_by_id(&self, id: CommunityId) -> Option<CommunityEntry<Ui>> {
        self.state.read().await.communities.iter()
            .find(|&community| community.id == id)
            .cloned()
    }

    pub async fn select_room(&self, room: Option<RoomEntry<Ui>>) {
        let mut state = self.state.write().await;

        match &room {
            Some(room) => self.message_list.set_stream(&room.message_stream).await,
            None => self.message_list.detach_stream().await,
        }

        state.selected_room = room;
    }

    pub async fn selected_room(&self) -> Option<RoomEntry<Ui>> {
        let state = self.state.read().await;
        state.selected_room.as_ref().cloned()
    }

    pub async fn log_out(&self) -> Result<()> {
        let request = self.request.send(ClientRequest::LogOut).await?;
        request.response().await?;
        Ok(())
    }

    pub async fn system_notification(&self, event: &ServerEvent) {
        if let ServerEvent::AddMessage(message) = event {
            // Show the system notification
            let msg = format!("{:?}: {}", message.author, message.content);

            #[cfg(windows)]
                notifica::notify("Vertex", &msg);

            #[cfg(unix)]
                {
                    let mut icon_path = env::current_dir().unwrap();
                    icon_path.push("res");
                    icon_path.push("icon.png");

                    tokio::task::spawn_blocking(move || {
                        let res = notify_rust::Notification::new()
                            .summary("Vertex")
                            .appname("Vertex")
                            .icon(&icon_path.to_str().unwrap())
                            .body(&msg)
                            .show();

                        if let Ok(handle) = res {
                            handle.on_close(|| {});
                        }
                    });
                };

            // Play the sound
            if let Some(sound) = &self.notif_sound {
                sound.lock().await.play();
            }
        }
    }
}

struct ClientLoop<Ui: ClientUi, S> {
    client: Client<Ui>,
    event_receiver: S,
}

impl<Ui: ClientUi, S> ClientLoop<Ui, S>
    where S: Stream<Item = tungstenite::Result<ServerEvent>> + Unpin
{
    // TODO: we need to be able to signal this to exit!
    async fn run(self) {
        let ClientLoop { client, event_receiver } = self;
        let request = client.request.clone();

        let receiver = Box::pin(async move {
            let mut event_receiver = event_receiver;
            while let Some(result) = event_receiver.next().await {
                match result {
                    Ok(event) => client.handle_event(event).await,
                    Err(err) => client.handle_network_err(err).await,
                }
            }
        });

        let keep_alive = Box::pin(async move {
            let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
            loop {
                if let Err(_) = request.net().ping().await {
                    break;
                }
                ticker.tick().await;
            }
        });

        futures::future::select(receiver, keep_alive).await;
    }
}
