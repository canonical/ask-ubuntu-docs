use adw::prelude::*;
use adw::{Application, ApplicationWindow, Clamp, HeaderBar, ToolbarView};
use anyhow::Result;
use gtk::{Box, Button, Entry, Label, Orientation, ScrolledWindow, Separator};

use crate::markdown;

const APP_ID: &str = "com.canonical.UbuntuDesktopHelp";

pub fn run() -> Result<()> {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
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

fn build_ui(app: &Application) {
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
    toolbar_view.add_top_bar(&HeaderBar::new());
    toolbar_view.set_content(Some(&main_box));
    window.set_content(Some(&toolbar_view));

    // Connect send button
    {
        let (message_list, input, scroll) =
            (message_list.clone(), input.clone(), scroll.clone());
        send_btn.connect_clicked(move |_| on_send(&input, &message_list, &scroll));
    }
    // Connect Enter key in the input field
    input.connect_activate(move |input| on_send(input, &message_list, &scroll));

    window.present();
}

fn on_send(input: &Entry, message_list: &Box, scroll: &ScrolledWindow) {
    let text = input.text();
    if text.trim().is_empty() {
        return;
    }
    input.set_text("");

    append_bubble(message_list, &text, true);
    append_bubble(message_list, "Hello", false);
    scroll_to_bottom(scroll);
}

fn append_bubble(message_list: &Box, text: &str, is_user: bool) {
    let label = Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(if is_user { 1.0 } else { 0.0 })
        .halign(if is_user { gtk::Align::End } else { gtk::Align::Start })
        .selectable(true)
        .build();

    if is_user {
        label.set_text(text); // user input is plain text, never parsed as markup
        label.add_css_class("user-bubble");
    } else {
        label.set_markup(&markdown::to_pango(text));
        label.add_css_class("assistant-bubble");
    }

    message_list.append(&label);
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
