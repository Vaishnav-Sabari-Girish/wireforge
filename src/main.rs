use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

use notify::{EventKind, RecursiveMode, Watcher};
use ratatui::{
    backend::CrosstermBackend,
    style::Color,
    Terminal
};

use ratatui_wireframe::WireframeWidget;
use std::{
    error::Error,
    io,
    path::PathBuf,
    sync::mpsc,
    time::Duration
};
use wrfm::WrfmModel;

#[derive(Parser, Debug)]
#[command(name = "wireforge", author, version, about = "TUI editor and viewer for .wrfm braille-based 3D models")]
struct Args {
    #[arg(required = true)]
    file: PathBuf
}


fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let target_file = args.file.clone();

    let mut wrfm_data = WrfmModel::from_file(&target_file).unwrap_or_else(|e| {
        eprintln!("Failed to load '{}': {}", target_file.display(), e);
        std::process::exit(1);
    });

    let mut status_msg = format!("Loaded {}", target_file.display());

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res && matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                let _ = tx.send(());
            }
        
    })?;

    let watch_path = target_file.canonicalize().unwrap_or_else(|_| target_file.clone());
    if let Some(parent) = watch_path.parent() {
        watcher.watch(parent, RecursiveMode::NonRecursive)?;
    } else {
        watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;
    }

    // Setup terminal
    enable_raw_mode()?;

    let mut stdout = io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Viewports state
    let mut pitch: f64 = 0.0;
    let mut yaw: f64 = 0.0;
    let mut roll: f64 = 0.0;
    let mut auto_spin: bool = true;

    loop {
        if rx.try_recv().is_ok() {
            while rx.try_recv().is_ok() {}

            std::thread::sleep(Duration::from_millis(15));

            match WrfmModel::from_file(&target_file) {
                Ok(new_data) => {
                    wrfm_data = new_data;
                    status_msg = "Hot-reload successful!".to_string();
                }
                Err(e) => {
                    status_msg = format!("Parse Error: {}", e);
                }
            }
        }

        terminal.draw(|f| {
            let title = format!(
            "Wireforge: {} | [Space] Spin | [Arrows] Rotate | [R/E] Roll | [Q] Quit | {}",
                wrfm_data.name, status_msg
            );

            let current_model = ratatui_wireframe::model::Model {
                vertices: wrfm_data.vertices.clone(),
                edges: wrfm_data.edges.clone()
            };

            let widget = WireframeWidget::new(pitch, yaw, roll)
                .title(title)
                .color(Color::Cyan)
                .model(current_model);

            f.render_widget(widget, f.area());
        })?;

        // Handle Input
        if event::poll(Duration::from_millis(16))? && let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char(' ') => auto_spin = !auto_spin,
                KeyCode::Up => pitch += 0.1,
                KeyCode::Down => pitch -= 0.1,
                KeyCode::Left => yaw -= 0.1,
                KeyCode::Right => yaw += 0.1,
                KeyCode::Char('r') => roll += 0.1,
                KeyCode::Char('e') => roll -= 0.1,
                _ => {},
            }
        }

        if auto_spin {
            yaw += 0.02;
            pitch += 0.01;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    Ok(())
}
