use eframe::egui;
use std::path::{Path, PathBuf};
use std::fs;
use chrono::{DateTime, Local};
use humansize::{format_size, BINARY};

#[derive(Clone, PartialEq)]
enum VfsType {
    Local,
    Ftp,
    Sftp,
    WebDav,
}

impl VfsType {
    fn name(&self) -> &'static str {
        match self {
            VfsType::Local => "Local",
            VfsType::Ftp => "FTP",
            VfsType::Sftp => "SFTP",
            VfsType::WebDav => "WebDAV",
        }
    }
}

struct FileEntry {
    name: String,
    ext: String,
    is_dir: bool,
    size: u64,
    modified: Option<DateTime<Local>>,
}

struct PaneState {
    vfs_type: VfsType,
    current_path: PathBuf, // Will represent URI or path
    files: Vec<FileEntry>,
    selected_index: usize,
    connection_string: String,
}

impl PaneState {
    fn new(path: impl AsRef<Path>) -> Self {
        let mut state = Self {
            vfs_type: VfsType::Local,
            current_path: path.as_ref().to_path_buf(),
            files: Vec::new(),
            selected_index: 0,
            connection_string: "".to_string(),
        };
        state.refresh();
        state
    }

    fn refresh(&mut self) {
        if self.vfs_type != VfsType::Local {
            // Placeholder for remote VFS refresh
            self.files.clear();
            self.files.push(FileEntry {
                name: "..".to_string(),
                ext: "".to_string(),
                is_dir: true,
                size: 0,
                modified: None,
            });
            return;
        }

        self.files.clear();
        
        if self.current_path.parent().is_some() {
            self.files.push(FileEntry {
                name: "..".to_string(),
                ext: "".to_string(),
                is_dir: true,
                size: 0,
                modified: None,
            });
        }

        if let Ok(entries) = fs::read_dir(&self.current_path) {
            let mut dirs = Vec::new();
            let mut files = Vec::new();

            for entry in entries.flatten() {
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified = metadata.and_then(|m| m.modified().ok())
                    .map(|sys_time| DateTime::from(sys_time));

                let path = entry.path();
                let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let ext = if is_dir {
                    "".to_string()
                } else {
                    path.extension().unwrap_or_default().to_string_lossy().to_string()
                };
                
                let full_name = if is_dir {
                    entry.file_name().to_string_lossy().to_string()
                } else {
                    name.clone()
                };

                let file_entry = FileEntry {
                    name: full_name,
                    ext,
                    is_dir,
                    size,
                    modified,
                };

                if is_dir {
                    dirs.push(file_entry);
                } else {
                    files.push(file_entry);
                }
            }

            dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

            self.files.extend(dirs);
            self.files.extend(files);
        }
    }
}

struct FaroCommander {
    left_pane: PaneState,
    right_pane: PaneState,
    active_pane_is_left: bool,
    show_copy_dialog: bool,
    copy_target_path: String,
    copy_source_path: String,
    
    show_move_dialog: bool,
    move_target_path: String,
    move_source_path: String,
    
    show_mkdir_dialog: bool,
    mkdir_path: String,
    
    show_delete_dialog: bool,
    delete_path: String,
}

impl Default for FaroCommander {
    fn default() -> Self {
        Self {
            left_pane: PaneState::new("C:\\"),
            right_pane: PaneState::new("C:\\"),
            active_pane_is_left: true,
            show_copy_dialog: false,
            copy_target_path: "".to_string(),
            copy_source_path: "".to_string(),
            show_move_dialog: false,
            move_target_path: "".to_string(),
            move_source_path: "".to_string(),
            show_mkdir_dialog: false,
            mkdir_path: "".to_string(),
            show_delete_dialog: false,
            delete_path: "".to_string(),
        }
    }
}

impl eframe::App for FaroCommander {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dialog_open = self.show_copy_dialog || self.show_move_dialog || self.show_mkdir_dialog || self.show_delete_dialog;

        // Handle shortcuts (Total Commander style)
        if !dialog_open {
            if ctx.input(|i| i.key_pressed(egui::Key::F3)) {
                self.action_view();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F4)) {
                self.action_edit();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                self.action_copy();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F6)) {
                if ctx.input(|i| i.modifiers.shift) {
                    println!("Shift+F6 Inline Rename triggered");
                } else {
                    self.action_move();
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F7)) {
                self.action_mkdir();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F8)) || ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.action_delete();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
                self.active_pane_is_left = !self.active_pane_is_left;
            }
        }

        if self.show_copy_dialog {
            egui::Window::new("Copy (F5)")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("Copy {} to:", self.copy_source_path));
                    let response = ui.add(egui::TextEdit::singleline(&mut self.copy_target_path).desired_width(400.0));
                    response.request_focus();
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let _ = fs::copy(&self.copy_source_path, &self.copy_target_path);
                            self.show_copy_dialog = false;
                            self.left_pane.refresh();
                            self.right_pane.refresh();
                        }
                        if ui.button("Cancel").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_copy_dialog = false;
                        }
                    });
                });
        }
        
        if self.show_move_dialog {
            egui::Window::new("Move/Rename (F6)")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("Move {} to:", self.move_source_path));
                    let response = ui.add(egui::TextEdit::singleline(&mut self.move_target_path).desired_width(400.0));
                    response.request_focus();
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let _ = fs::rename(&self.move_source_path, &self.move_target_path);
                            self.show_move_dialog = false;
                            self.left_pane.refresh();
                            self.right_pane.refresh();
                        }
                        if ui.button("Cancel").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_move_dialog = false;
                        }
                    });
                });
        }
        
        if self.show_mkdir_dialog {
            egui::Window::new("New Folder (F7)")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Folder name:");
                    let response = ui.add(egui::TextEdit::singleline(&mut self.mkdir_path).desired_width(400.0));
                    response.request_focus();
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let _ = fs::create_dir_all(&self.mkdir_path);
                            self.show_mkdir_dialog = false;
                            self.left_pane.refresh();
                            self.right_pane.refresh();
                        }
                        if ui.button("Cancel").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_mkdir_dialog = false;
                        }
                    });
                });
        }
        
        if self.show_delete_dialog {
            egui::Window::new("Delete (F8)")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("Do you really want to delete {}?", self.delete_path));
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let path = PathBuf::from(&self.delete_path);
                            if path.is_dir() {
                                let _ = fs::remove_dir_all(&path);
                            } else {
                                let _ = fs::remove_file(&path);
                            }
                            self.show_delete_dialog = false;
                            self.left_pane.refresh();
                            self.right_pane.refresh();
                        }
                        if ui.button("No").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_delete_dialog = false;
                        }
                    });
                });
        }

        // Bottom bar
        let mut trigger_f3 = false;
        let mut trigger_f4 = false;
        let mut trigger_f5 = false;
        let mut trigger_f6 = false;
        let mut trigger_f7 = false;
        let mut trigger_f8 = false;
        let mut trigger_exit = false;

        let bottom_frame = egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(4.0, 6.0))
            .fill(ctx.style().visuals.panel_fill);

        egui::TopBottomPanel::bottom("bottom_panel")
            .frame(bottom_frame)
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_justify(true), |ui| {
                    let h = 28.0;
                    if ui.add_sized([0.0, h], egui::Button::new("F3 View")).clicked() { trigger_f3 = true; }
                    if ui.add_sized([0.0, h], egui::Button::new("F4 Edit")).clicked() { trigger_f4 = true; }
                    if ui.add_sized([0.0, h], egui::Button::new("F5 Copy")).clicked() { trigger_f5 = true; }
                    if ui.add_sized([0.0, h], egui::Button::new("F6 RenMov")).clicked() { trigger_f6 = true; }
                    if ui.add_sized([0.0, h], egui::Button::new("F7 Mkdir")).clicked() { trigger_f7 = true; }
                    if ui.add_sized([0.0, h], egui::Button::new("F8 Delete")).clicked() { trigger_f8 = true; }
                    if ui.add_sized([0.0, h], egui::Button::new("Alt+F4 Exit")).clicked() { trigger_exit = true; }
                });
            });

        if trigger_f3 { self.action_view(); }
        if trigger_f4 { self.action_edit(); }
        if trigger_f5 { self.action_copy(); }
        if trigger_f6 { self.action_move(); }
        if trigger_f7 { self.action_mkdir(); }
        if trigger_f8 { self.action_delete(); }
        if trigger_exit { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }

        // Top bar (Menus etc)
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Files", |ui| {
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Commands", |ui| {
                    let _ = ui.button("Search... (Alt+F7)");
                    let _ = ui.button("Synchronize Dirs...");
                });
                ui.menu_button("Net", |ui| {
                    let _ = ui.button("FTP Connect...");
                    let _ = ui.button("FTP New Connection...");
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let half_width = ui.available_width() / 2.0;
            let available_height = ui.available_height();

            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(half_width, available_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.render_pane(ui, true, ctx);
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), available_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.render_pane(ui, false, ctx);
                    },
                );
            });
        });
    }
}

impl FaroCommander {
    fn action_view(&mut self) {
        println!("F3 View triggered");
    }
    
    fn action_edit(&mut self) {
        println!("F4 Edit triggered");
    }
    
    fn action_copy(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        let target_pane = if self.active_pane_is_left { &self.right_pane } else { &self.left_pane };
        if let Some(file) = active_pane.files.get(active_pane.selected_index) {
            if file.name != ".." {
                self.copy_source_path = active_pane.current_path.join(&file.name).to_string_lossy().to_string();
                self.copy_target_path = target_pane.current_path.join(&file.name).to_string_lossy().to_string();
                self.show_copy_dialog = true;
            }
        }
    }
    
    fn action_move(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        let target_pane = if self.active_pane_is_left { &self.right_pane } else { &self.left_pane };
        if let Some(file) = active_pane.files.get(active_pane.selected_index) {
            if file.name != ".." {
                self.move_source_path = active_pane.current_path.join(&file.name).to_string_lossy().to_string();
                self.move_target_path = target_pane.current_path.join(&file.name).to_string_lossy().to_string();
                self.show_move_dialog = true;
            }
        }
    }
    
    fn action_mkdir(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        self.mkdir_path = active_pane.current_path.join("Neuer Ordner").to_string_lossy().to_string();
        self.show_mkdir_dialog = true;
    }
    
    fn action_delete(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        if let Some(file) = active_pane.files.get(active_pane.selected_index) {
            if file.name != ".." {
                self.delete_path = active_pane.current_path.join(&file.name).to_string_lossy().to_string();
                self.show_delete_dialog = true;
            }
        }
    }

    fn get_windows_drives() -> Vec<String> {
        let mut drives = Vec::new();
        for drive_letter in b'A'..=b'Z' {
            let path = format!("{}:\\", drive_letter as char);
            if std::path::Path::new(&path).exists() {
                drives.push(path);
            }
        }
        drives
    }

    fn render_pane(&mut self, ui: &mut egui::Ui, is_left: bool, ctx: &egui::Context) {
        let is_active = self.active_pane_is_left == is_left;
        
        let pane = if is_left {
            &mut self.left_pane
        } else {
            &mut self.right_pane
        };

        // Draw a frame around the pane to fill the area completely
        let frame = egui::Frame::none()
            .fill(ui.visuals().panel_fill)
            .inner_margin(4.0)
            .stroke(if is_active {
                egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE)
            } else {
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
            });

        frame.show(ui, |ui| {
            ui.vertical(|ui| {
                // Drive / VFS selector
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_source(if is_left { "left_vfs" } else { "right_vfs" })
                        .selected_text(pane.vfs_type.name())
                        .show_ui(ui, |ui| {
                            let mut changed = false;
                            changed |= ui.selectable_value(&mut pane.vfs_type, VfsType::Local, "Local").changed();
                            changed |= ui.selectable_value(&mut pane.vfs_type, VfsType::Ftp, "FTP").changed();
                            changed |= ui.selectable_value(&mut pane.vfs_type, VfsType::Sftp, "SFTP").changed();
                            changed |= ui.selectable_value(&mut pane.vfs_type, VfsType::WebDav, "WebDAV").changed();
                            if changed {
                                pane.refresh();
                            }
                        });

                    if pane.vfs_type != VfsType::Local {
                        ui.add(egui::TextEdit::singleline(&mut pane.connection_string).hint_text("user:pass@host:port/path"));
                        if ui.button("Connect").clicked() {
                            // TODO: Implement connection via OpenDAL
                            pane.refresh();
                        }
                    } else {
                        // Dynamic Windows Drives
                        for drive in Self::get_windows_drives() {
                            if ui.button(&drive[0..2]).on_hover_text(&drive).clicked() {
                                pane.current_path = PathBuf::from(&drive);
                                pane.refresh();
                            }
                        }
                    }
                });
                ui.separator();

                let path_str = pane.current_path.to_string_lossy().into_owned();
                
                // Current Path
                if is_active {
                    ui.colored_label(egui::Color32::LIGHT_BLUE, egui::RichText::new(&path_str).strong());
                } else {
                    ui.label(egui::RichText::new(&path_str).strong());
                }
                ui.separator();
                
                // Handle keyboard navigation
                if is_active {
                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        if pane.selected_index + 1 < pane.files.len() {
                            pane.selected_index += 1;
                        }
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        if pane.selected_index > 0 {
                            pane.selected_index -= 1;
                        }
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        if let Some(file) = pane.files.get(pane.selected_index) {
                            if file.is_dir {
                                let nav_name = file.name.clone();
                                if nav_name == ".." {
                                    if let Some(parent) = pane.current_path.parent() {
                                        pane.current_path = parent.to_path_buf();
                                    }
                                } else {
                                    pane.current_path.push(nav_name);
                                }
                                pane.refresh();
                                pane.selected_index = 0;
                            }
                        }
                    }
                }

                // File List
                let mut action_navigate = None;
                
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .vscroll(true)
                    .auto_shrink([false, false])
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(egui_extras::Column::initial(150.0).at_least(100.0).clip(true).resizable(true)) // Name
                .column(egui_extras::Column::initial(50.0).at_least(30.0).clip(true).resizable(true))  // Ext
                .column(egui_extras::Column::initial(80.0).at_least(50.0).clip(true).resizable(true))  // Size
                .column(egui_extras::Column::remainder().at_least(100.0).clip(true)) // Date
                .header(20.0, |mut header| {
                    header.col(|ui| { ui.strong("Name"); });
                    header.col(|ui| { ui.strong("Ext"); });
                    header.col(|ui| { ui.strong("Size"); });
                    header.col(|ui| { ui.strong("Date"); });
                })
                .body(|mut body| {
                    for (idx, file) in pane.files.iter().enumerate() {
                        let is_selected = is_active && pane.selected_index == idx;
                        
                        let (name_text, ext_text, size_text, date_text) = if file.name == ".." {
                            ("[..]".to_string(), "".to_string(), "<DIR>".to_string(), "".to_string())
                        } else if file.is_dir {
                            (format!("[{}]", file.name), "".to_string(), "<DIR>".to_string(), "".to_string())
                        } else {
                            let date_str = file.modified.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                            (file.name.clone(), file.ext.clone(), format_size(file.size, BINARY), date_str)
                        };

                        body.row(20.0, |mut row| {
                            // Make the row selectable/clickable
                            row.set_selected(is_selected);
                            
                            row.col(|ui| {
                                let label = ui.selectable_label(is_selected, &name_text);
                                if label.clicked() {
                                    self.active_pane_is_left = is_left;
                                    pane.selected_index = idx;
                                }
                                if label.double_clicked() {
                                    if file.is_dir {
                                        action_navigate = Some(file.name.clone());
                                    }
                                }
                            });
                            row.col(|ui| { ui.label(&ext_text); });
                            row.col(|ui| { ui.label(&size_text); });
                            row.col(|ui| { ui.label(&date_text); });
                        });
                    }
                });

            if let Some(nav_dir) = action_navigate {
                if nav_dir == ".." {
                    if let Some(parent) = pane.current_path.parent() {
                        pane.current_path = parent.to_path_buf();
                    }
                } else {
                    pane.current_path.push(nav_dir);
                }
                pane.refresh();
                pane.selected_index = 0;
            }
            });
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    // Initialize tokio runtime for background network tasks
    let rt = tokio::runtime::Runtime::new().expect("Unable to create tokio runtime");
    let _guard = rt.enter();
    
    // We leak the runtime so it stays alive for the duration of the app, 
    // allowing us to call tokio::spawn from anywhere.
    std::mem::forget(rt);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("FaroCommander"),
        ..Default::default()
    };
    eframe::run_native(
        "FaroCommander",
        options,
        Box::new(|_cc| Box::<FaroCommander>::default()),
    )
}
