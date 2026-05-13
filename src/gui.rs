use adw::prelude::*;
use adw::{Application, ApplicationWindow};
use anyhow::Result;

const APP_ID: &str = "com.canonical.UbuntuDesktopHelp";

pub fn run() -> Result<()> {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    // Pass no CLI args to GTK — argument parsing is handled by clap before this point
    let status = app.run_with_args::<String>(&[]);
    if status == 0 {
        Ok(())
    } else {
        anyhow::bail!("GTK application exited with status {status}")
    }
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Ubuntu Desktop Help")
        .default_width(800)
        .default_height(600)
        .build();

    window.present();
}
