use std::sync::{Arc, Mutex};

use adw::glib;
use adw::prelude::*;
use adw::{Application, ApplicationWindow, Clamp, HeaderBar, ToolbarView};
use anyhow::Result;
use gtk::{Box, Button, Entry, Label, Orientation, ScrolledWindow, Separator};

use crate::llm::{LlmClient, Message};
use crate::markdown;
use crate::prompts;
use crate::vectordb::{RagStore, TOP_K};

const APP_ID: &str = "com.canonical.UbuntuDesktopHelp";
const PRODUCTS: &[&str] = &["Desktop", "Server", "Core", "WSL", "Flavors"];

pub fn run(
    client: Arc<LlmClient>,
    rag: Arc<Mutex<RagStore>>,
    conversation: Arc<Mutex<Vec<Message>>>,
    tokio_handle: tokio::runtime::Handle,
) -> Result<()> {
    let app = Application::builder().application_id(APP_ID).build();

    app.connect_activate({
        let (client, rag, conversation, tokio_handle) = (
            client.clone(),
            rag.clone(),
            conversation.clone(),
            tokio_handle.clone(),
        );
        move |app| build_ui(app, client.clone(), rag.clone(), conversation.clone(), tokio_handle.clone())
    });

    // Pass no CLI args to GTK — argument parsing is handled by clap before this point
    let status = app.run_with_args::<String>(&[]);
    if status == 0.into() {
        Ok(())
    } else {
        anyhow::bail!("GTK application exited with status {:?}", status)
    }
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".user-bubble {
            background: @accent_bg_color;
            color: @accent_fg_color;
            border-radius: 12px;
            padding: 8px 12px;
        }
        .assistant-bubble {
            background: @card_bg_color;
            border-radius: 12px;
            padding: 8px 12px;
        }",
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("could not get display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn build_ui(
    app: &Application,
    client: Arc<LlmClient>,
    rag: Arc<Mutex<RagStore>>,
    conversation: Arc<Mutex<Vec<Message>>>,
    tokio_handle: tokio::runtime::Handle,
) {
    load_css();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Ubuntu Desktop Help")
        .default_width(800)
        .default_height(600)
        .build();

    // Message list — new bubbles are appended here
    let message_list = Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let scroll = ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&message_list)
        .build();

    let messages_clamp = Clamp::builder()
        .maximum_size(700)
        .vexpand(true)
        .child(&scroll)
        .build();

    // Input row
    let input = Entry::builder()
        .placeholder_text("Ask a question…")
        .hexpand(true)
        .build();

    let send_btn = Button::builder()
        .label("Send")
        .css_classes(["suggested-action"])
        .build();

    let input_row = Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    input_row.append(&input);
    input_row.append(&send_btn);

    let input_clamp = Clamp::builder()
        .maximum_size(700)
        .child(&input_row)
        .build();

    // Main layout: messages on top, separator, input row at bottom
    let main_box = Box::new(Orientation::Vertical, 0);
    main_box.append(&messages_clamp);
    main_box.append(&Separator::new(Orientation::Horizontal));
    main_box.append(&input_clamp);

    let toolbar_view = ToolbarView::new();
    let header = HeaderBar::new();
    let system_list = gtk::StringList::new(&["Desktop", "Server", "Core", "WSL", "Flavors"]);
    let system_dropdown = gtk::DropDown::builder()
        .model(&system_list)
        .selected(0)
        .css_classes(["flat"])
        .build();
    header.pack_end(&system_dropdown);
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&main_box));
    window.set_content(Some(&toolbar_view));

    // Connect send button
    {
        let (ml, sc, inp, btn, cl, ra, cv, th, sd) = (
            message_list.clone(), scroll.clone(), input.clone(), send_btn.clone(),
            client.clone(), rag.clone(), conversation.clone(), tokio_handle.clone(),
            system_dropdown.clone(),
        );
        send_btn.connect_clicked(move |_| on_send(&inp, &btn, &ml, &sc, &cl, &ra, &cv, &th, &sd));
    }
    // Connect Enter key in the input field
    input.connect_activate(move |inp| on_send(inp, &send_btn, &message_list, &scroll, &client, &rag, &conversation, &tokio_handle, &system_dropdown));

    window.present();
}

fn on_send(
    input: &Entry,
    send_btn: &Button,
    message_list: &Box,
    scroll: &ScrolledWindow,
    client: &Arc<LlmClient>,
    rag: &Arc<Mutex<RagStore>>,
    conversation: &Arc<Mutex<Vec<Message>>>,
    tokio_handle: &tokio::runtime::Handle,
    system_dropdown: &gtk::DropDown,
) {
    let text = input.text();
    if text.trim().is_empty() {
        return;
    }

    // Read the selected product and its system prompt on the GTK thread before spawning.
    let product = PRODUCTS
        .get(system_dropdown.selected() as usize)
        .copied()
        .unwrap_or("Desktop");
    let system_prompt = prompts::get_prompt(product).to_string();
    let doc_prefix = prompts::source_prefix(product).map(str::to_string);
    input.set_text("");
    input.set_sensitive(false);
    send_btn.set_sensitive(false);

    // User bubble
    append_bubble(message_list, &text, true);

    // Empty assistant bubble; kept for incremental token updates
    let reply_label = Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .selectable(true)
        .build();
    wire_links(&reply_label);
    reply_label.add_css_class("assistant-bubble");
    message_list.append(&reply_label);
    scroll_to_bottom(scroll);

    // Channel: None signals completion, Some(token) is a streamed token.
    // async-channel works with both tokio (sender side) and glib (receiver side).
    let (sender, receiver) = async_channel::bounded::<Option<String>>(64);

    // Receive tokens on the GTK main thread via glib's async executor
    {
        let reply_label = reply_label.clone();
        let input = input.clone();
        let send_btn = send_btn.clone();
        let scroll = scroll.clone();

        glib::MainContext::default().spawn_local(async move {
            let mut accumulated = String::new();
            while let Ok(msg) = receiver.recv().await {
                match msg {
                    Some(token) => {
                        accumulated.push_str(&token);
                        reply_label.set_markup(&markdown::to_pango(&accumulated));
                        scroll_to_bottom(&scroll);
                    }
                    None => {
                        input.set_sensitive(true);
                        send_btn.set_sensitive(true);
                        input.grab_focus();
                        break;
                    }
                }
            }
        });
    }

    // Spawn the RAG search + LLM streaming on the tokio runtime
    let (query, client, rag, conversation) = (
        text.to_string(),
        Arc::clone(client),
        Arc::clone(rag),
        Arc::clone(conversation),
    );
    tokio_handle.spawn(async move {
        // RAG search is CPU-bound; block_in_place prevents starving other tokio tasks
        let relevant = tokio::task::block_in_place(|| {
            rag.lock().unwrap()
                .search(&query, TOP_K, doc_prefix.as_deref())
                .unwrap_or_default()
        });

        // RAG context is injected into the LLM call for this turn only.
        // The bare query (without doc chunks) is what gets stored in history,
        // so doc chunks don't accumulate and re-inflate the prompt on every turn.
        let llm_user_content = if relevant.is_empty() {
            query.clone()
        } else {
            let ctx = relevant
                .iter()
                .map(|(src, txt)| format!("[Source: {src}]\n{txt}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            format!("Context from documentation:\n{ctx}\n\nQuestion: {query}")
        };

        // Build message list: optional system prompt prepended, then conversation history,
        // then the current turn's augmented user message.
        // The system message is NOT stored in `conversation` to avoid it repeating each turn.
        let messages = {
            let conv = conversation.lock().unwrap();
            let mut msgs = Vec::with_capacity(conv.len() + 2);
            if !system_prompt.is_empty() {
                msgs.push(Message { role: "system".to_string(), content: system_prompt });
            }
            msgs.extend_from_slice(&conv);
            msgs.push(Message { role: "user".to_string(), content: llm_user_content });
            msgs
        };

        let sender_tok = sender.clone();
        let result = client
            .chat_streaming(&messages, || {}, move |t| {
                sender_tok.send_blocking(Some(t.to_string())).ok();
            })
            .await;

        match result {
            Ok(reply) => {
                let mut conv = conversation.lock().unwrap();
                conv.push(Message { role: "user".to_string(), content: query });
                conv.push(Message { role: "assistant".to_string(), content: reply });
            }
            Err(e) => {
                sender.send_blocking(Some(format!("\n\n*Error: {e}*"))).ok();
            }
        }

        sender.send_blocking(None).ok();
    });
}

fn append_bubble(message_list: &Box, text: &str, is_user: bool) {
    let label = Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(if is_user { 1.0 } else { 0.0 })
        .halign(if is_user { gtk::Align::End } else { gtk::Align::Start })
        .selectable(true) // both bubble types are selectable for copy-paste
        .build();

    if is_user {
        label.set_markup(&markdown::linkify_plain(text)); // auto-link URLs; plain text otherwise
        label.add_css_class("user-bubble");
    } else {
        wire_links(&label);
        label.set_markup(&markdown::to_pango(text));
        label.add_css_class("assistant-bubble");
    }

    message_list.append(&label);
}

// Opens links in the system browser when clicked. Attached to assistant labels so that
// link activation takes priority over text selection (the two conflict in gtk::Label).
fn wire_links(label: &Label) {
    label.connect_activate_link(|_, uri| {
        gtk::UriLauncher::new(uri).launch(
            None::<&gtk::Window>,
            gtk::gio::Cancellable::NONE,
            |_| {},
        );
        glib::Propagation::Stop
    });
}

fn scroll_to_bottom(scroll: &ScrolledWindow) {
    // Defer until after the new widgets have been laid out
    adw::glib::idle_add_local_once({
        let scroll = scroll.clone();
        move || {
            let adj = scroll.vadjustment();
            adj.set_value(adj.upper() - adj.page_size());
        }
    });
}

