mod cli;
mod parser;
mod execution;
mod history;
mod persistence;
mod utility;

use anyhow::Result;
use crossbeam_channel::unbounded;
use ratatui::backend::CrosstermBackend;

use execution::{spawn_worker, UiMsg, WorkerMsg};
use history::App;


fn main() -> Result<()> {
    // channels
    let (tx_worker, rx_worker) = unbounded::<WorkerMsg>();
    let (tx_ui, rx_ui) = unbounded::<UiMsg>();
    spawn_worker(rx_worker, tx_ui);

    let mut terminal = utility::setup_terminal()?;

    let mut app = App::new();

    loop {
        terminal.draw(|f| cli::render_ui::<CrosstermBackend<std::io::Stdout>>(f, &app))?;

        // check for worker results without blocking UI
        while let Ok(msg) = rx_ui.try_recv() {
            match msg {
                UiMsg::Started(cmd) => app.begin_run(cmd),
                UiMsg::StdoutChunk(chunk) => app.append_stdout_chunk(chunk),
                UiMsg::StderrChunk(chunk) => app.append_stderr_chunk(chunk),
                UiMsg::Finished(res) => app.finish_run(res),
            }
        }

        if app.should_auto_run() {
            let cmd = app.input.clone();
            if app.prepare_run(&cmd, false) {
                tx_worker.send(WorkerMsg::Run(cmd)).ok();
            }
        }

        // handle input
        if !cli::handle_input(&mut app, &tx_worker)? {
            break;
        }
    }

    // restore terminal
    utility::restore_terminal()?;
    Ok(())
}
