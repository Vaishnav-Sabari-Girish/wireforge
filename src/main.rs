use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

use notify::{EventKind, RecursiveMode, Watcher};
use ratatui::{Terminal, backend::CrosstermBackend, style::Color};

use ratatui_wireframe::{model::Model, WireframeWidget};
use std::{error::Error, io, path::{Path, PathBuf}, sync::mpsc, time::Duration};
use wrfm::WrfmModel;

#[cfg(feature = "ratty")]
use ratatui_ratty::{ObjectFormat, RattyGraphic, RattyGraphicSettings};

#[derive(Parser, Debug)]
#[command(
    name = "wireforge",
    author,
    version,
    about = "TUI editor and viewer for .wrfm and .obj 3D models"
)]
struct Args {
    #[arg(required = true)]
    file: PathBuf,
}

/// State enum to track which rendering engine we are using
enum RenderMode {
    Braille(Model),
    #[cfg(feature = "ratty")]
    Hardware3D(RattyGraphic<'static>),
}

/// Helper to automatically route .obj and .wrfm files to the correct parser or widget
fn load_model(target_file: &Path) -> Result<(RenderMode, String), String> {
    let ext = target_file.extension().and_then(|s| s.to_str()).unwrap_or("");

    if ext.eq_ignore_ascii_case("obj") {
        let name = target_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("OBJ Model")
            .to_string();

        #[cfg(feature = "ratty")]
        {
            // Convert to absolute path so Ratty can locate it from anywhere
            let abs_path = std::fs::canonicalize(target_file).unwrap_or_else(|_| target_file.to_path_buf());
            let path_str = abs_path.to_string_lossy().into_owned();

            let settings = RattyGraphicSettings::new(path_str)
                .id(1)
                .format(ObjectFormat::Obj)
                .scale(0.30) // Adjust this up or down depending on your specific model's export scale
                .brightness(1.5);
            
            let graphic = RattyGraphic::new(settings);
            
            // Register the payload with the terminal emulator
            if let Err(e) = graphic.register() {
                return Err(format!("Ratty Registration Error: {}", e));
            }

            Ok((RenderMode::Hardware3D(graphic), name))
        }
        #[cfg(not(feature = "ratty"))]
        {
            // Fallback: ratatui-wireframe feature is off
            Err("OBJ parsing requires the 'ratty' feature. Recompile with --features ratty".to_string())
        }
    } else {
        // Default to .wrfm legacy parser
        let wrfm_data = WrfmModel::from_file(target_file).map_err(|e| e.to_string())?;
        let model = Model {
            vertices: wrfm_data.vertices,
            edges: wrfm_data.edges,
        };
        Ok((RenderMode::Braille(model), wrfm_data.name))
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let target_file = args.file.clone();

    // Initial Load
    let (mut current_render, mut model_name) = load_model(&target_file).unwrap_or_else(|e| {
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

    let watch_path = target_file
        .canonicalize()
        .unwrap_or_else(|_| target_file.clone());
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
        // Hot-reloading logic
        if rx.try_recv().is_ok() {
            while rx.try_recv().is_ok() {} // Clear queue
            std::thread::sleep(Duration::from_millis(15));

            match load_model(&target_file) {
                Ok((new_render, new_name)) => {
                    current_render = new_render;
                    model_name = new_name;
                    status_msg = "Hot-reload successful!".to_string();
                }
                Err(e) => {
                    status_msg = format!("Parse Error: {}", e);
                }
            }
        }

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
                _ => {}
            }
        }

        if auto_spin {
            yaw += 0.02;
            pitch += 0.01;
        }

        // RGP Update Step: Push mutated rotation directly to the emulator
        #[cfg(feature = "ratty")]
        if let RenderMode::Hardware3D(ref mut graphic) = current_render {
            graphic.settings_mut().rotation = [pitch as f32, yaw as f32, roll as f32];
            let _ = graphic.update(); // Fire escape sequence
        }

        terminal.draw(|f| {
            let title = format!(
                "Wireforge: {} | [Space] Spin | [Arrows] Rotate | [R/E] Roll | [Q] Quit | {}",
                model_name, status_msg
            );

            match &current_render {
                RenderMode::Braille(model) => {
                    let widget = WireframeWidget::new(pitch, yaw, roll)
                        .title(title)
                        .color(Color::Cyan)
                        .model(model);
                    f.render_widget(widget, f.area());
                }
                #[cfg(feature = "ratty")]
                RenderMode::Hardware3D(graphic) => {
                    // Draw our custom UI frame
                    let block = ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::ALL)
                        .title(title);
                    
                    let inner_area = block.inner(f.area());
                    f.render_widget(block, f.area());

                    // Render the pre-registered 3D graphic strictly inside the frame
                    f.render_widget(graphic, inner_area);
                }
            }
        })?;
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}
