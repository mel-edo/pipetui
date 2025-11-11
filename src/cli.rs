use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::Duration;
use crate::execution::WorkerMsg;
use crate::history::App;
use crossbeam_channel::Sender;

pub fn handle_input(app: &mut App, tx_worker: &Sender<WorkerMsg>) -> anyhow::Result<bool> {
    if crossterm::event::poll(Duration::from_millis(100))? {
        if let Event::Key(key) = crossterm::event::read()? {
            // ignore repeats
            if key.kind == KeyEventKind::Repeat {
                return Ok(true);
            }
            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(false);
                }
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.move_cursor_home();
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.move_cursor_end();
                }
                KeyCode::Esc => return Ok(false),
                KeyCode::Enter => {
                    let cmd = app.input.clone();
                    if app.prepare_run(&cmd, true) {
                        tx_worker.send(WorkerMsg::Run(cmd)).ok();
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.clear_input();
                }
                KeyCode::Backspace => {
                    app.delete_backward();
                }
                KeyCode::Delete => {
                    app.delete_forward();
                }
                KeyCode::Left => {
                    app.move_cursor_left();
                }
                KeyCode::Right => {
                    app.move_cursor_right();
                }
                KeyCode::Home => {
                    app.move_cursor_home();
                }
                KeyCode::End => {
                    app.move_cursor_end();
                }
                KeyCode::Up => {
                    app.history_prev();
                }
                KeyCode::Down => {
                    app.history_next();
                }
                KeyCode::Char(ch) => {
                    if key.modifiers.contains(KeyModifiers::ALT)
                        || key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return Ok(true);
                    }
                    app.insert_char(ch);
                }
                _ => {}
            }
        }
    }
    Ok(true)
}

pub fn render_ui<B: ratatui::backend::Backend>(f: &mut ratatui::Frame, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(6),
            ]
            .as_ref(),
        )
        .split(f.size());

    // Input
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().title("pipeline").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    f.render_widget(input, chunks[0]);

    // Output
    let out_block = Block::default().title("stdout").borders(Borders::ALL);
    let out_area = chunks[1];
    let stdout_lines = app.stdout_view(out_area);
    let out = if stdout_lines.is_empty() {
        Paragraph::new(Line::from("(waiting for output...)"))
            .block(out_block)
            .wrap(Wrap { trim: false })
    } else {
        Paragraph::new(stdout_lines)
            .block(out_block)
            .wrap(Wrap { trim: false })
    };
    f.render_widget(out, out_area);

    // Stderr + Status
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(1)].as_ref())
        .split(chunks[2]);

    let err_block = Block::default().title("stderr").borders(Borders::ALL);
    let stderr_lines = app.stderr_view(bottom_chunks[0]);
    let err = if stderr_lines.is_empty() {
        Paragraph::new(Line::from("<no stderr>")).block(err_block)
    } else {
        Paragraph::new(stderr_lines)
            .block(err_block)
            .wrap(Wrap { trim: false })
    };
    f.render_widget(err, bottom_chunks[0]);

    let status = Paragraph::new(Line::from(vec![
        Span::styled("Status: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(&app.status_line),
        Span::raw("   "),
        Span::raw("Keys: Enter=run  Esc=quit  Ctrl+u=clear  ↑/↓=history  ←/→=move  Home/End"),
    ]));
    f.render_widget(status, bottom_chunks[1]);

    // Set cursor to input box
    let cursor_x =
        chunks[0].x + 1 + unicode_width::UnicodeWidthStr::width(&app.input[..app.cursor]) as u16;
    let y = chunks[0].y + 1;
    f.set_cursor(cursor_x, y);
}
