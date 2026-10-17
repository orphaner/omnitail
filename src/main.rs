use std::sync::mpsc;
use std::thread;
use anyhow::Result;
use crate::app::App;
use crate::ui::Ui;

mod v1;
mod ui;
mod app;
mod yo;

fn main() -> Result<()> {
    let file_paths = vec![
        "_geek/small.txt".to_string(),
        "_geek/medium.txt".to_string(),
        "_geek/large.txt".to_string(),
    ];
    // let mut app = TailApp::new(file_paths[0].clone());
    // app.run();

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        v1::run(&file_paths[1], tx).unwrap();
        // v0::run()?;
    });

    let app = App {};
    let mut ui = Ui::new(app);
    ui.run(rx)?;
    Ok(())
}
