use gtk::prelude::*;

use vertex::prelude::*;

use crate::{Client, Result, TryGetText, client};
use crate::connect::AsConnector;
use crate::window;

use gtk::{DialogFlags, ResponseType, Label, EntryBuilder, WidgetExt, TextBufferBuilder, ScrolledWindowBuilder};
use atk::{RelationType, AtkObjectExt, RelationSetExt};
use futures::Future;

pub fn show_add_community(client: Client) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[
                ("Create", ResponseType::Other(0)),
                ("Join", ResponseType::Other(1))
            ],
        );

        let label = Label::new(Some("Add a Community"));
        label.get_style_context().add_class("title");
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&label)
            .build();
        dialog.get_content_area().add(&title_box);

        let client = client.clone();
        dialog.connect_response(move |dialog, response_ty| {
            match response_ty {
                ResponseType::Other(x) => {
                    match x {
                        0 => show_create_community(client.clone()),
                        1 => show_join_community(client.clone()),
                        _ => {}
                    }
                    dialog.emit_close();
                }
                _ => dialog.emit_close(),
            }
        });

        (dialog, title_box)
    });
}

async fn create_community(client: Client, name: &str) -> Result<()> {
    let community = client.create_community(name).await?;
    community.create_room("General").await?;
    community.create_room("Off Topic").await?;
    Ok(())
}

pub fn show_create_community(client: Client) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("Create", ResponseType::Apply)],
        );

        let label = Label::new(Some("Create A Community"));
        label.get_style_context().add_class("title");
        let entry = EntryBuilder::new()
            .placeholder_text("Community name...")
            .build();
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&label)
            .build();

        entry.clone().connect_activate(
            dialog.connector()
                .do_sync(|dialog, _| dialog.response(ResponseType::Apply))
                .build_cloned_consumer()
        );

        let content = dialog.get_content_area();
        content.add(&title_box);
        content.add(&entry);

        let client = client.clone();
        dialog.connect_response(
            client.connector()
                .do_async(move |client, (dialog, response_type): (gtk::Dialog, ResponseType)| {
                    let entry = entry.clone();
                    async move {
                        if response_type != ResponseType::Apply {
                            dialog.emit_close();
                            return;
                        }

                        if let Ok(name) = entry.try_get_text() {
                            if let Err(err) = create_community(client, &name).await {
                                show_generic_error(&err);
                            }
                        }

                        dialog.emit_close();
                    }
                })
                .build_widget_and_owned_listener()
        );

        (dialog, title_box)
    });
}

pub fn show_join_community(client: Client) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("Join", ResponseType::Apply)],
        );

        let label = Label::new(Some("Join A Community"));
        label.get_style_context().add_class("title");
        let entry = EntryBuilder::new()
            .placeholder_text("Invite code...")
            .build();
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&label)
            .build();

        entry.clone().connect_activate(
            dialog.connector()
                .do_sync(|dialog, _| dialog.response(ResponseType::Apply))
                .build_cloned_consumer()
        );

        let content = dialog.get_content_area();
        content.add(&title_box);
        content.add(&entry);

        let client = client.clone();
        dialog.connect_response(
            client.connector()
                .do_async(move |client, (dialog, response_type): (gtk::Dialog, ResponseType)| {
                    let entry = entry.clone();
                    async move {
                        if response_type != ResponseType::Apply {
                            dialog.emit_close();
                            return;
                        }

                        let code_entry = entry.clone();
                        if let Ok(code) = code_entry.try_get_text() {
                            let code = InviteCode(code);
                            if let Err(err) = client.join_community(code).await {
                                show_generic_error(&err);
                            }
                        }
                        dialog.emit_close();
                    }
                })
                .build_widget_and_owned_listener()
        );

        (dialog, title_box)
    });
}

pub fn show_invite_dialog(invite: InviteCode) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("Ok", ResponseType::Ok)],
        );

        let label = Label::new(Some("Invite Code"));
        label.get_style_context().add_class("title");
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&label)
            .build();

        let code_view: gtk::TextView = gtk::TextViewBuilder::new()
            .editable(false)
            .name("Invite code")
            .buffer(&gtk::TextBufferBuilder::new().text(&invite.0).build())
            .build();

        let objs = (code_view.get_accessible(), label.get_accessible());
        if let (Some(code_view), Some(label)) = objs {
            let relations = code_view.ref_relation_set().expect("Error getting relations set");
            relations.add_relation_by_type(RelationType::LabelledBy, &label);
        }

        code_view.get_style_context().add_class("invite_code_text");

        let content = dialog.get_content_area();
        content.add(&title_box);
        content.add(&code_view);

        code_view.connect_button_release_event(|code_view, _| {
            if let Some(buf) = code_view.get_buffer() {
                let (start, end) = (buf.get_start_iter(), buf.get_end_iter());
                buf.select_range(&start, &end);
            }
            gtk::Inhibit(false)
        });

        dialog.connect_response(|dialog, _| dialog.emit_close());
        (dialog, title_box)
    });
}

pub fn show_create_room(community: client::CommunityEntry) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("Create", ResponseType::Apply)],
        );

        let label = Label::new(Some("Create A Channel"));
        label.get_style_context().add_class("title");
        let entry = EntryBuilder::new()
            .placeholder_text("Channel name...")
            .build();
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&label)
            .build();

        entry.clone().connect_activate(
            dialog.connector()
                .do_sync(|dialog, _| dialog.response(ResponseType::Apply))
                .build_cloned_consumer()
        );

        let content = dialog.get_content_area();
        content.add(&title_box);
        content.add(&entry);

        dialog.connect_response(
            community.connector()
                .do_async(move |community, (dialog, response_type): (gtk::Dialog, ResponseType)| {
                    let entry = entry.clone();
                    async move {
                        if response_type != ResponseType::Apply {
                            dialog.emit_close();
                            return;
                        }

                        if let Ok(name) = entry.try_get_text() {
                            if let Err(err) = community.create_room(&name).await {
                                show_generic_error(&err);
                            }
                        }

                        dialog.emit_close();
                    }
                })
                .build_widget_and_owned_listener()
        );

        (dialog, title_box)
    });
}

pub fn show_report_message(client: Client, msg: MessageId) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("Report", ResponseType::Apply)],
        );

        let label = Label::new(Some("Report A Message"));
        label.get_style_context().add_class("title");
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&label)
            .build();

        let short = EntryBuilder::new()
            .placeholder_text("Short description...")
            .build();

        let buf = TextBufferBuilder::new()
            .text("Describe other details and reasoning related to the report.")
            .build();
        let long = gtk::TextViewBuilder::new()
            .buffer(&buf)
            .build();
        let long_scroll = ScrolledWindowBuilder::new()
            .child(&long)
            .name("extended_desc_scroll")
            .max_content_width(380)
            .min_content_width(380)
            .max_content_height(200)
            .min_content_height(200)
            .build();
        let long_box = gtk::BoxBuilder::new()
            .child(&long_scroll)
            .name("extended_desc_box")
            .build();

        let content = dialog.get_content_area();
        content.add(&title_box);
        content.add(&short);
        content.add(&long_box);

        let client = client.clone();
        dialog.connect_response(
            client.connector()
                .do_async(move |client, (dialog, response_type): (gtk::Dialog, ResponseType)| {
                    let short = short.clone();
                    let long = long.clone();

                    async move {
                        if response_type != ResponseType::Apply {
                            dialog.emit_close();
                            return;
                        }

                        let buf = long.get_buffer().unwrap();
                        let (begin, end) = &buf.get_bounds();
                        let long_desc = buf.get_text(begin, end, false);
                        let long_desc = long_desc.as_ref().map(|c| c.as_str()).unwrap_or_default();

                        if let Ok(short_desc) = short.try_get_text() {
                            let res = client.report_message(msg, &short_desc, long_desc).await;
                            if let Err(e) = res {
                                show_generic_error(&e);
                            }
                        }
                        dialog.emit_close();
                    }
                })
                .build_widget_and_owned_listener()
        );

        (dialog, title_box)
    });
}

pub fn show_choose_report_action(client: Client, user: UserId) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("None", ResponseType::Other(0)), ("Ban", ResponseType::Other(1))],
        );

        let heading = Label::new(Some("Choose an action"));
        heading.get_style_context().add_class("title");
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&heading)
            .build();

        let content = dialog.get_content_area();
        content.add(&title_box);

        dialog.connect_response(
            client.connector()
                .do_async(move |client, (dialog, response_type): (gtk::Dialog, ResponseType)| {
                    async move {
                        if let ResponseType::Other(1) = response_type {
                            match client.ban_users(vec![user]).await.map(|mut v| v.pop()) {
                                Err(ref e) | Ok(Some((_, ref e))) => show_generic_error(&e),
                                _ => {}
                            }
                        }

                        dialog.emit_close();
                    }
                })
                .build_widget_and_owned_listener()
        );
        (dialog, title_box)
    });
}

pub fn show_confirm<C, F, D>(
    heading: &str,
    body: &str,
    connector: D,
    if_yes: C,
) where C: FnMut(D) -> F + Clone + 'static,
        F: Future<Output = ()> + 'static,
        D: Clone + 'static,
{
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("Ok", ResponseType::Ok), ("Cancel", ResponseType::Cancel)],
        );

        let heading = Label::new(Some(heading));
        heading.get_style_context().add_class("title");
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&heading)
            .build();

        let description: gtk::Label = gtk::Label::new(Some(body));

        let content = dialog.get_content_area();
        content.add(&title_box);
        content.add(&description);

        dialog.connect_response(
            (if_yes, connector).connector()
                .do_async(|(mut c, d), (dialog, response): (gtk::Dialog, gtk::ResponseType)| {
                    async move {
                        if response == ResponseType::Ok {
                            c(d).await;
                        }
                        dialog.emit_close()
                    }
                })
                .build_widget_and_owned_listener()
        );

        (dialog, title_box)
    });
}

pub fn show_generic_error<E: std::fmt::Display>(error: &E) {
    window::show_dialog(|window| {
        let dialog = gtk::Dialog::new_with_buttons(
            None,
            Some(&window.window),
            DialogFlags::MODAL | DialogFlags::DESTROY_WITH_PARENT,
            &[("Ok", ResponseType::Ok)],
        );

        let heading = Label::new(Some("Error"));
        heading.get_style_context().add_class("title");
        heading.set_widget_name("error_title");
        let title_box = gtk::BoxBuilder::new()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .child(&heading)
            .build();

        let description: gtk::Label = gtk::Label::new(Some(&format!("{}", error)));
        description.get_style_context().add_class("error_description");

        let content = dialog.get_content_area();
        content.add(&title_box);
        content.add(&description);

        dialog.connect_response(|dialog, _| dialog.emit_close());
        (dialog, title_box)
    });
}
