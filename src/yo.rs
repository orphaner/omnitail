use anyhow::Error;
use tokio::sync::mpsc;

pub enum Tail {
    File,
}

pub struct InternalConfig {
    pub log_file: String,
}

pub struct App {
    config: InternalConfig,
}

pub struct Ui {
    app: App,
    pub tails: Vec<Tail>,
}

pub struct State {

}

pub enum Action {
    AddTail(Tail),
    RemoveTail(Tail),
}

impl Ui {
    pub fn new(app: App, tails: Vec<Tail>, state: State) -> Self {
        Self {
            app,
            tails,
        }
    }

    pub async fn run() -> Result<(), Error>{
        let (action_tx, action_rx) = mpsc::unbounded_channel::<Action>();
        Ok(())
    }
}
