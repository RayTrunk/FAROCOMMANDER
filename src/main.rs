#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use eframe::egui;
use std::path::{Path, PathBuf};
use std::fs;
use chrono::{DateTime, Local};
use std::sync::Arc;

mod sftp_client;
mod ssh_client;
mod rdp_client;
mod net_scanner;
mod file_ops;
mod archive_ops;
mod tools;

pub mod messages {
    pub enum PaneMessage {
        SftpConnected(russh_sftp::client::SftpSession),
        OpendalConnected(opendal::Operator),
        SftpError(String),
        FilesFetched(Vec<crate::FileEntry>),
        DirSizeCalculated(String, u64),
        Refresh,
    }
    pub enum SshMessage {
        Output(String),
        Error(String),
        Disconnected,
    }
    pub enum EditorMessage {
        FileLoaded(String, String, String),
        ImageLoaded(String, [usize; 2], Vec<u8>),
        Error(String),
    }
}

pub fn format_size_binary(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut val = bytes as f64;
    let mut idx = 0;
    while val >= 1024.0 && idx + 1 < UNITS.len() {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.2} {}", val, UNITS[idx])
    }
}

struct ReadOnlyTextBuffer<'a>(&'a str);
impl<'a> eframe::egui::TextBuffer for ReadOnlyTextBuffer<'a> {
    fn is_mutable(&self) -> bool { false }
    fn as_str(&self) -> &str { self.0 }
    fn insert_text(&mut self, _text: &str, _char_index: usize) -> usize { 0 }
    fn delete_char_range(&mut self, _char_range: std::ops::Range<usize>) {}
}

#[derive(Clone, PartialEq)]
enum VfsType {
    Local,
    Ftp,
    Sftp,
    WebDav,
    Zip(String),
}

impl VfsType {
    fn name(&self) -> String {
        match self {
            VfsType::Local => "Local".to_string(),
            VfsType::Ftp => "FTP".to_string(),
            VfsType::Sftp => "SFTP".to_string(),
            VfsType::WebDav => "WebDAV".to_string(),
            VfsType::Zip(p) => format!("ARCHIVE: {}", p),
        }
    }
}

pub struct FileEntry {
    pub name: String,
    pub ext: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<DateTime<Local>>,
}

impl FileEntry {
    pub fn parent_dir() -> Self {
        Self {
            name: "..".to_string(),
            ext: String::new(),
            is_dir: true,
            size: 0,
            modified: None,
        }
    }

    pub fn from_filename(
        filename: String,
        is_dir: bool,
        size: u64,
        modified: Option<DateTime<Local>>,
    ) -> Self {
        let (name, ext) = if is_dir {
            (filename, String::new())
        } else {
            let p = Path::new(&filename);
            let n = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let e = p.extension().unwrap_or_default().to_string_lossy().to_string();
            (n, e)
        };
        Self {
            name,
            ext,
            is_dir,
            size,
            modified,
        }
    }

    pub fn full_name(&self) -> String {
        if self.is_dir || self.ext.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.name, self.ext)
        }
    }
}

#[derive(Clone, PartialEq)]
enum SortColumn {
    Name,
    Ext,
    Size,
    Date,
}

#[derive(Clone, PartialEq)]
enum SortOrder {
    Ascending,
    Descending,
}

struct PaneState {
    vfs_type: VfsType,
    current_path: PathBuf, // Will represent URI or path
    files: Vec<FileEntry>,
    selected_index: usize,
    selected_files: std::collections::HashSet<String>,
    calculated_dir_sizes: std::collections::HashMap<String, u64>,
    history: Vec<PathBuf>,
    connection_string: String,
    
    sort_column: SortColumn,
    sort_order: SortOrder,
    
    // SFTP State
    sftp_session: Option<Arc<tokio::sync::Mutex<russh_sftp::client::SftpSession>>>,
    opendal_op: Option<opendal::Operator>,
    loading: bool,
    error_msg: Option<String>,
    
    // Communication
    tx: std::sync::mpsc::Sender<messages::PaneMessage>,
    rx: std::sync::mpsc::Receiver<messages::PaneMessage>,
}

impl PaneState {
    fn new(path: impl AsRef<Path>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = Self {
            vfs_type: VfsType::Local,
            current_path: path.as_ref().to_path_buf(),
            files: Vec::new(),
            selected_index: 0,
            selected_files: std::collections::HashSet::new(),
            calculated_dir_sizes: std::collections::HashMap::new(),
            history: Vec::new(),
            connection_string: "".to_string(),
            sort_column: SortColumn::Name,
            sort_order: SortOrder::Ascending,
            sftp_session: None,
            opendal_op: None,
            loading: false,
            error_msg: None,
            tx,
            rx,
        };
        state.refresh();
        state
    }

    fn navigate_to(&mut self, new_path: PathBuf) {
        if self.current_path != new_path {
            self.history.push(self.current_path.clone());
            self.current_path = new_path;
            self.selected_index = 0;
            self.refresh();
        }
    }

    fn navigate_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.current_path = prev;
            self.selected_index = 0;
            self.refresh();
        }
    }

    fn get_vfs_handle(&self) -> crate::file_ops::VfsHandle {
        if self.vfs_type == VfsType::Sftp {
            if let Some(session) = &self.sftp_session {
                return crate::file_ops::VfsHandle::Sftp(Arc::clone(session));
            }
        } else if self.vfs_type == VfsType::Ftp || self.vfs_type == VfsType::WebDav {
            if let Some(op) = &self.opendal_op {
                return crate::file_ops::VfsHandle::Opendal(op.clone());
            }
        } else if let VfsType::Zip(z) = &self.vfs_type {
            return crate::file_ops::VfsHandle::Zip(z.clone());
        }
        crate::file_ops::VfsHandle::Local
    }

    fn refresh(&mut self) {
        self.selected_files.clear();
        self.calculated_dir_sizes.clear();
        if self.vfs_type == VfsType::Sftp {
            if let Some(session) = &self.sftp_session {
                self.loading = true;
                let tx = self.tx.clone();
                let path = self.current_path.to_string_lossy().into_owned();
                let path = if path.is_empty() { ".".to_string() } else { path };
                
                let session = Arc::clone(session);
                tokio::spawn(async move {
                    let sftp = session.lock().await;
                    if let Ok(files) = crate::sftp_client::list_sftp_dir(&sftp, &path).await {
                        let _ = tx.send(messages::PaneMessage::FilesFetched(files));
                    } else {
                        let _ = tx.send(messages::PaneMessage::SftpError("Failed to list dir".into()));
                    }
                });
            }
            return;
        }

        if self.vfs_type == VfsType::Ftp || self.vfs_type == VfsType::WebDav {
            if let Some(op) = &self.opendal_op {
                self.loading = true;
                let tx = self.tx.clone();
                let path = self.current_path.to_string_lossy().into_owned();
                let op = op.clone();
                
                tokio::spawn(async move {
                    if let Ok(files) = crate::opendal_client::list_opendal_dir(&op, &path).await {
                        let _ = tx.send(messages::PaneMessage::FilesFetched(files));
                    } else {
                        let _ = tx.send(messages::PaneMessage::SftpError("Failed to list remote dir".into()));
                    }
                });
            }
            return;
        }

        if let VfsType::Zip(zip_path) = &self.vfs_type {
            self.loading = true;
            let tx = self.tx.clone();
            let current_path = self.current_path.to_string_lossy().into_owned();
            let zip_path = zip_path.clone();
            
            tokio::task::spawn_blocking(move || {
                match crate::archive_ops::list_archive_dir(&zip_path, &current_path) {
                    Ok(results) => {
                        let _ = tx.send(messages::PaneMessage::FilesFetched(results));
                    }
                    Err(e) => {
                        let _ = tx.send(messages::PaneMessage::SftpError(format!("Archiv-Fehler: {}", e)));
                    }
                }
            });
            return;
        }

        self.files.clear();
        if self.current_path.parent().is_some() {
            self.files.push(FileEntry::parent_dir());
        }
        if let Ok(entries) = fs::read_dir(&self.current_path) {
            for entry in entries.flatten() {
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified = metadata.and_then(|m| m.modified().ok()).map(DateTime::from);
                let filename = entry.file_name().to_string_lossy().to_string();
                self.files.push(FileEntry::from_filename(filename, is_dir, size, modified));
            }
            self.sort_files();
        }
    }

    fn sort_files(&mut self) {
        // ".." is always first
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut parent_dir = None;
        
        for file in self.files.drain(..) {
            if file.name == ".." {
                parent_dir = Some(file);
            } else if file.is_dir {
                dirs.push(file);
            } else {
                files.push(file);
            }
        }
        
        let sort_cmp = |a: &FileEntry, b: &FileEntry| {
            let res = match self.sort_column {
                SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortColumn::Ext => a.ext.to_lowercase().cmp(&b.ext.to_lowercase()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortColumn::Size => a.size.cmp(&b.size).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortColumn::Date => a.modified.cmp(&b.modified).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            };
            if self.sort_order == SortOrder::Descending {
                res.reverse()
            } else {
                res
            }
        };
        
        dirs.sort_by(sort_cmp);
        files.sort_by(sort_cmp);
        
        if let Some(p) = parent_dir {
            self.files.push(p);
        }
        self.files.extend(dirs);
        self.files.extend(files);
    }
}

#[derive(Clone)]
struct SyncEntry {
    rel_path: String,
    left_size: Option<u64>,
    right_size: Option<u64>,
    status: &'static str, // "Left only", "Right only", "Different", "Identical"
    selected: bool,
}

struct FaroCommander {
    left_pane: PaneState,
    right_pane: PaneState,
    active_pane_is_left: bool,
    show_transfer_dialog: bool,
    transfer_is_move: bool,
    transfer_target_path: String,
    transfer_source_path: String,
    transfer_sources: Vec<(String, String, bool)>, // (src_full_path, file_name, is_dir)
    
    show_mkdir_dialog: bool,
    mkdir_path: String,
    
    show_delete_dialog: bool,
    delete_path: String,
    delete_sources: Vec<(String, bool)>, // (full_path, is_dir)
    
    // Inline Rename (Shift+F6)
    inline_rename_active: bool,
    inline_rename_value: String,
    inline_rename_orig: String,

    // Synchronize Dirs
    show_sync_dialog: bool,
    sync_entries: Vec<SyncEntry>,
    sync_hide_identical: bool,

    // SSH Terminal & Encrypted Vault Profiles
    show_ssh_terminal: bool,
    ssh_vault_unlocked: bool,
    ssh_vault_password: String,
    ssh_vault_password_confirm: String,
    ssh_vault_error: Option<String>,
    ssh_profiles: Vec<crate::ssh_client::SshProfile>,
    ssh_selected_profile: Option<usize>,
    ssh_profile_draft: crate::ssh_client::SshProfile,
    ssh_show_sidebar: bool,
    ssh_output: String,
    ssh_input: String,
    ssh_cmd_history: Vec<String>,
    ssh_history_idx: Option<usize>,
    ssh_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ssh_rx: std::sync::mpsc::Receiver<messages::SshMessage>,
    ssh_ui_tx: std::sync::mpsc::Sender<messages::SshMessage>,

    // IronRDP Remote Desktop & Vault Profiles
    show_rdp_window: bool,
    rdp_fullscreen: bool,
    rdp_auto_size: (u16, u16),
    rdp_profiles: Vec<crate::rdp_client::RdpProfile>,
    rdp_selected_profile: Option<usize>,
    rdp_profile_draft: crate::rdp_client::RdpProfile,
    rdp_show_sidebar: bool,
    rdp_status: String,
    rdp_texture: Option<egui::TextureHandle>,
    rdp_remote_size: (u16, u16),
    rdp_tx: Option<tokio::sync::mpsc::Sender<crate::rdp_client::RdpInputMessage>>,
    rdp_rx: std::sync::mpsc::Receiver<crate::rdp_client::RdpUiMessage>,
    rdp_ui_tx: std::sync::mpsc::Sender<crate::rdp_client::RdpUiMessage>,

    // Viewer / Editor
    show_editor: bool,
    editor_path: String,
    editor_content: String,
    editor_is_view_only: bool,
    editor_vfs_handle: Option<crate::file_ops::VfsHandle>,
    editor_rx: std::sync::mpsc::Receiver<messages::EditorMessage>,
    editor_tx: std::sync::mpsc::Sender<messages::EditorMessage>,
    editor_search_query: String,
    editor_replace_query: String,
    editor_show_find_bar: bool,
    editor_show_replace_bar: bool,
    editor_wrap: bool,
    editor_encoding: String,
    editor_line_ending: String,
    editor_last_cursor: Option<usize>,
    editor_dirty: bool,
    editor_font_size: f32,
    editor_show_statusbar: bool,
    editor_show_save_as: bool,
    editor_save_as_path: String,
    editor_image_data: Option<([usize; 2], Vec<u8>)>,
    editor_image_texture: Option<egui::TextureHandle>,
    editor_image_fit: bool,
    editor_image_zoom: f32,

    // Background task progress
    task_tx: std::sync::mpsc::Sender<(String, f32)>,
    task_rx: std::sync::mpsc::Receiver<(String, f32)>,
    active_task: Option<(String, f32)>,
    
    // Search
    show_search_dialog: bool,
    search_query: String,
    search_results: Vec<String>,
    search_selected: Option<usize>,
    search_running: bool,
    search_tx: std::sync::mpsc::Sender<String>,
    search_rx: std::sync::mpsc::Receiver<String>,

    // Pack / Unpack Archives (Alt+F5 / Alt+F9)
    show_pack_dialog: bool,
    pack_target_path: String,
    pack_source_desc: String,
    pack_sources: Vec<(String, String, bool)>,
    pack_format: crate::archive_ops::ArchiveFormat,
    show_unpack_dialog: bool,
    unpack_archive_path: String,
    unpack_target_dir: String,

    // Feature 2: Multi-Rename Tool (Ctrl+M)
    show_multi_rename: bool,
    multi_rename: crate::tools::MultiRenameState,

    // Feature 3: Bookmarks / Favorites (Ctrl+D)
    bookmarks: Vec<String>,
    show_bookmarks_dialog: bool,
    new_bookmark_input: String,

    // Feature 5: File Properties & Checksums (Alt+Enter)
    show_properties_dialog: bool,
    properties_state: crate::tools::FilePropertiesState,
    props_tx: std::sync::mpsc::Sender<(u64, usize, usize, String, String, String)>,
    props_rx: std::sync::mpsc::Receiver<(u64, usize, usize, String, String, String)>,

    // Feature 6: Command Line & Output
    cmdline_input: String,
    cmdline_output: String,
    show_cmdline_output: bool,

    // Settings (Themes & Languages) + Info / About
    settings: crate::tools::AppSettings,
    show_settings_dialog: bool,
    show_about_dialog: bool,

    // Startup Splash Screen (5 seconds) & Logo Texture
    show_splash: bool,
    splash_start: Option<std::time::Instant>,
    splash_texture: Option<egui::TextureHandle>,

    // Network Scanner & Password Generator (Tools menu)
    show_net_scanner: bool,
    net_scanner: crate::net_scanner::NetScannerState,
    net_scan_tx: std::sync::mpsc::Sender<crate::net_scanner::NetScanUiMessage>,
    net_scan_rx: std::sync::mpsc::Receiver<crate::net_scanner::NetScanUiMessage>,
    show_password_generator: bool,
    password_generator: crate::tools::PasswordGeneratorState,
}

impl Default for FaroCommander {
    fn default() -> Self {
        let (ssh_ui_tx, ssh_rx) = std::sync::mpsc::channel();
        let (rdp_ui_tx, rdp_rx) = std::sync::mpsc::channel();
        let (e_tx, e_rx) = std::sync::mpsc::channel();
        let (task_tx, task_rx) = std::sync::mpsc::channel();
        let (search_tx, search_rx) = std::sync::mpsc::channel();
        let (props_tx, props_rx) = std::sync::mpsc::channel();
        let (net_scan_tx, net_scan_rx) = std::sync::mpsc::channel();
        Self {
            left_pane: PaneState::new("C:\\"),
            right_pane: PaneState::new("C:\\"),
            active_pane_is_left: true,
            show_transfer_dialog: false,
            transfer_is_move: false,
            transfer_target_path: String::new(),
            transfer_source_path: String::new(),
            transfer_sources: Vec::new(),
            show_mkdir_dialog: false,
            mkdir_path: String::new(),
            show_delete_dialog: false,
            delete_path: String::new(),
            delete_sources: Vec::new(),
            inline_rename_active: false,
            inline_rename_value: String::new(),
            inline_rename_orig: String::new(),
            show_sync_dialog: false,
            sync_entries: Vec::new(),
            sync_hide_identical: true,
            show_ssh_terminal: false,
            ssh_vault_unlocked: false,
            ssh_vault_password: String::new(),
            ssh_vault_password_confirm: String::new(),
            ssh_vault_error: None,
            ssh_profiles: Vec::new(),
            ssh_selected_profile: None,
            ssh_profile_draft: crate::ssh_client::SshProfile::default(),
            ssh_show_sidebar: true,
            ssh_output: "FARO COMMANDER SSH Terminal (russh, 80x24 Standard PTY) — Ready.\n".to_string(),
            ssh_input: String::new(),
            ssh_cmd_history: Vec::new(),
            ssh_history_idx: None,
            ssh_tx: None,
            ssh_rx,
            ssh_ui_tx,
            show_rdp_window: false,
            rdp_fullscreen: false,
            rdp_auto_size: (1280, 720),
            rdp_profiles: Vec::new(),
            rdp_selected_profile: None,
            rdp_profile_draft: crate::rdp_client::RdpProfile::default(),
            rdp_show_sidebar: true,
            rdp_status: "RDP Bereit".to_string(),
            rdp_texture: None,
            rdp_remote_size: (1280, 720),
            rdp_tx: None,
            rdp_rx,
            rdp_ui_tx,
            show_editor: false,
            editor_path: String::new(),
            editor_content: String::new(),
            editor_is_view_only: true,
            editor_vfs_handle: None,
            editor_rx: e_rx,
            editor_tx: e_tx,
            editor_search_query: String::new(),
            editor_replace_query: String::new(),
            editor_show_find_bar: false,
            editor_show_replace_bar: false,
            editor_wrap: false,
            editor_encoding: "UTF-8".to_string(),
            editor_line_ending: "Windows (CRLF)".to_string(),
            editor_last_cursor: None,
            editor_dirty: false,
            editor_font_size: 14.0,
            editor_show_statusbar: true,
            editor_show_save_as: false,
            editor_save_as_path: String::new(),
            editor_image_data: None,
            editor_image_texture: None,
            editor_image_fit: true,
            editor_image_zoom: 1.0,
            task_tx,
            task_rx,
            active_task: None,
            show_search_dialog: false,
            search_query: String::new(),
            search_results: Vec::new(),
            search_selected: None,
            search_running: false,
            search_tx,
            search_rx,
            show_pack_dialog: false,
            pack_target_path: String::new(),
            pack_source_desc: String::new(),
            pack_sources: Vec::new(),
            pack_format: crate::archive_ops::ArchiveFormat::Zip,
            show_unpack_dialog: false,
            unpack_archive_path: String::new(),
            unpack_target_dir: String::new(),
            show_multi_rename: false,
            multi_rename: crate::tools::MultiRenameState::default(),
            bookmarks: crate::tools::load_bookmarks(),
            show_bookmarks_dialog: false,
            new_bookmark_input: String::new(),
            show_properties_dialog: false,
            properties_state: crate::tools::FilePropertiesState::default(),
            props_tx,
            props_rx,
            cmdline_input: String::new(),
            cmdline_output: String::new(),
            show_cmdline_output: false,
            settings: crate::tools::load_settings(),
            show_settings_dialog: false,
            show_about_dialog: false,
            show_splash: true,
            splash_start: None,
            splash_texture: None,
            show_net_scanner: false,
            net_scanner: crate::net_scanner::NetScannerState::default(),
            net_scan_tx,
            net_scan_rx,
            show_password_generator: false,
            password_generator: crate::tools::PasswordGeneratorState::default(),
        }
    }
}

impl eframe::App for FaroCommander {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.show_splash {
            if self.splash_texture.is_none() {
                let logo_bytes = include_bytes!("../LOGO.png");
                if let Ok(img) = image::load_from_memory(logo_bytes) {
                    let rgba = img.to_rgba8();
                    let size = [img.width() as usize, img.height() as usize];
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_flat_samples().as_slice());
                    self.splash_texture = Some(ctx.load_texture("splash_logo", color_image, egui::TextureOptions::LINEAR));
                }
            }

            let start = *self.splash_start.get_or_insert_with(std::time::Instant::now);
            let elapsed = start.elapsed().as_secs_f32();
            if elapsed >= 5.0 {
                self.show_splash = false;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(egui::Color32::from_rgb(17, 19, 46)))
                    .show(ctx, |ui| {
                        let avail = ui.available_size();
                        ui.vertical_centered(|ui| {
                            let logo_side = (avail.x.min(avail.y) * 0.72).clamp(200.0, 560.0);
                            let top_pad = ((avail.y - logo_side - 40.0) * 0.5).max(10.0);
                            ui.add_space(top_pad);
                            if let Some(tex) = &self.splash_texture {
                                ui.add(egui::Image::from_texture((tex.id(), egui::vec2(logo_side, logo_side))));
                            }
                            ui.add_space(16.0);
                            let progress = (elapsed / 5.0).clamp(0.0, 1.0);
                            ui.allocate_ui(egui::vec2(logo_side * 0.65, 8.0), |ui| {
                                ui.add(
                                    egui::ProgressBar::new(progress)
                                        .desired_width(logo_side * 0.65)
                                        .desired_height(6.0)
                                        .fill(egui::Color32::from_rgb(104, 232, 219)),
                                );
                            });
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                    .size(12.0)
                                    .color(egui::Color32::from_rgb(140, 175, 210)),
                            );
                        });
                    });
                return;
            }
        }

        // Apply active theme
        crate::tools::apply_theme(ctx, self.settings.theme);
        let is_de = self.settings.language.is_de();

        while let Ok((name, progress)) = self.task_rx.try_recv() {
            if progress >= 1.0 {
                self.active_task = None;
            } else {
                self.active_task = Some((name, progress));
            }
        }

        while let Ok(msg) = self.search_rx.try_recv() {
            if msg == "##DONE##" {
                self.search_running = false;
            } else {
                self.search_results.push(msg);
            }
        }

        while let Ok((sz, f_cnt, d_cnt, crc, md5, sha)) = self.props_rx.try_recv() {
            self.properties_state.size = sz;
            self.properties_state.file_count = f_cnt;
            self.properties_state.dir_count = d_cnt;
            self.properties_state.crc32 = crc;
            self.properties_state.md5 = md5;
            self.properties_state.sha256 = sha;
            self.properties_state.calculating = false;
        }

        if let Some((name, progress)) = &self.active_task {
            egui::Window::new(if is_de { "Vorgang läuft..." } else { "Operation in Progress" })
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("{}: {}", if is_de { "Verarbeite" } else { "Processing" }, name));
                    ui.add(egui::ProgressBar::new(*progress).show_percentage());
                });
        }

        let dialog_open = self.show_transfer_dialog
            || self.show_mkdir_dialog
            || self.show_delete_dialog
            || self.show_search_dialog
            || self.show_sync_dialog
            || self.show_pack_dialog
            || self.show_unpack_dialog
            || self.show_multi_rename
            || self.show_bookmarks_dialog
            || self.show_properties_dialog
            || self.show_cmdline_output
            || self.show_settings_dialog
            || self.show_about_dialog
            || self.inline_rename_active
            || self.active_task.is_some();

        // Handle shortcuts (Total Commander style)
        if !dialog_open {
            if ctx.input(|i| i.key_pressed(egui::Key::F3)) {
                self.action_view(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F4)) {
                self.action_edit(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                if ctx.input(|i| i.modifiers.alt) {
                    self.action_pack();
                } else {
                    self.action_copy();
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F6)) {
                if ctx.input(|i| i.modifiers.shift) {
                    self.action_inline_rename();
                } else {
                    self.action_move();
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F7)) {
                if ctx.input(|i| i.modifiers.alt) {
                    self.show_search_dialog = true;
                } else {
                    self.action_mkdir();
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F8)) || ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.action_delete();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F9) && i.modifiers.alt) {
                self.action_unpack();
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::M)) {
                self.action_multi_rename();
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::D)) {
                let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
                self.new_bookmark_input = active_pane.current_path.to_string_lossy().to_string();
                self.show_bookmarks_dialog = true;
            }
            if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::Enter)) {
                if ctx.input(|i| i.modifiers.shift) {
                    self.action_calculate_dir_sizes(ctx);
                } else {
                    self.action_properties(ctx);
                }
            }
            if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)) {
                if self.active_pane_is_left {
                    self.left_pane.navigate_back();
                } else {
                    self.right_pane.navigate_back();
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
                self.inline_rename_active = false;
                self.active_pane_is_left = !self.active_pane_is_left;
            }
        }

        // Feature 2: Multi-Rename Tool Dialog (Ctrl+M)
        if self.show_multi_rename {
            egui::Window::new("Mehrfach-Umbenenn-Tool (Strg+M)")
                .collapsible(false)
                .resizable(true)
                .default_width(680.0)
                .show(ctx, |ui| {
                    ui.columns(2, |cols| {
                        cols[0].label("Umbenennmaske Dateiname:");
                        cols[0].add(egui::TextEdit::singleline(&mut self.multi_rename.name_mask).desired_width(260.0));
                        cols[0].horizontal(|ui| {
                            if ui.small_button("[N] Name").clicked() { self.multi_rename.name_mask.push_str("[N]"); }
                            if ui.small_button("[C] Zähler").clicked() { self.multi_rename.name_mask.push_str("_[C]"); }
                            if ui.small_button("[Y]-[M]-[D] Datum").clicked() { self.multi_rename.name_mask.push_str("_[Y]-[M]-[D]"); }
                        });
                        cols[0].add_space(4.0);
                        cols[0].label("Erweiterung (Ext):");
                        cols[0].add(egui::TextEdit::singleline(&mut self.multi_rename.ext_mask).desired_width(140.0));

                        cols[1].label("Zähler-Einstellungen [C]:");
                        cols[1].horizontal(|ui| {
                            ui.label("Start:");
                            ui.add(egui::DragValue::new(&mut self.multi_rename.counter_start).clamp_range(0..=99999));
                            ui.label("Schritt:");
                            ui.add(egui::DragValue::new(&mut self.multi_rename.counter_step).clamp_range(1..=100));
                            ui.label("Stellen:");
                            ui.add(egui::DragValue::new(&mut self.multi_rename.counter_digits).clamp_range(1..=8));
                        });
                        cols[1].add_space(4.0);
                        cols[1].label("Suchen & Ersetzen:");
                        cols[1].horizontal(|ui| {
                            ui.label("Suchen:");
                            ui.add(egui::TextEdit::singleline(&mut self.multi_rename.search_for).desired_width(100.0));
                            ui.label("Ersetzen:");
                            ui.add(egui::TextEdit::singleline(&mut self.multi_rename.replace_with).desired_width(100.0));
                        });
                        cols[1].add_space(4.0);
                        cols[1].horizontal(|ui| {
                            ui.label("Schreibweise:");
                            for mode in [
                                crate::tools::CaseMode::Unchanged,
                                crate::tools::CaseMode::Lowercase,
                                crate::tools::CaseMode::Uppercase,
                                crate::tools::CaseMode::TitleCase,
                            ] {
                                if ui.selectable_label(self.multi_rename.case_mode == mode, mode.label()).clicked() {
                                    self.multi_rename.case_mode = mode;
                                }
                            }
                        });
                    });

                    ui.separator();
                    ui.strong(format!("Live-Vorschau ({} Elemente):", self.multi_rename.items.len()));
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        egui::Grid::new("multi_rename_preview").striped(true).min_col_width(280.0).show(ui, |ui| {
                            ui.strong("Alter Dateiname");
                            ui.strong("Neuer Dateiname");
                            ui.end_row();
                            for (idx, (_, old_name, is_dir)) in self.multi_rename.items.iter().enumerate() {
                                let new_name = self.multi_rename.compute_new_name(old_name, *is_dir, idx);
                                ui.label(old_name);
                                if &new_name != old_name {
                                    ui.colored_label(egui::Color32::LIGHT_GREEN, &new_name);
                                } else {
                                    ui.label(&new_name);
                                }
                                ui.end_row();
                            }
                        });
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Start! (Alle umbenennen)").clicked() {
                            for (idx, (full_old_path, old_name, is_dir)) in self.multi_rename.items.iter().enumerate() {
                                let new_name = self.multi_rename.compute_new_name(old_name, *is_dir, idx);
                                if !new_name.is_empty() && &new_name != old_name {
                                    if let Some(parent) = std::path::Path::new(full_old_path).parent() {
                                        let new_full = parent.join(&new_name);
                                        let _ = std::fs::rename(full_old_path, new_full);
                                    }
                                }
                            }
                            self.left_pane.refresh();
                            self.right_pane.refresh();
                            self.show_multi_rename = false;
                        }
                        if ui.button("Schließen").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_multi_rename = false;
                        }
                    });
                });
        }

        // Feature 3: Bookmarks / Favorites Dialog (Ctrl+D)
        if self.show_bookmarks_dialog {
            egui::Window::new("★ Verzeichnis-Favoriten (Strg+D)")
                .collapsible(false)
                .resizable(true)
                .default_width(500.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Neuer Favorit:");
                        ui.add(egui::TextEdit::singleline(&mut self.new_bookmark_input).desired_width(300.0));
                        if ui.button("+ Speichern").clicked() {
                            let trimmed = self.new_bookmark_input.trim().to_string();
                            if !trimmed.is_empty() && !self.bookmarks.contains(&trimmed) {
                                self.bookmarks.push(trimmed);
                                crate::tools::save_bookmarks(&self.bookmarks);
                            }
                        }
                    });
                    ui.separator();
                    ui.strong("Gespeicherte Pfade (Klicken zum Öffnen im aktiven Fenster):");
                    let mut jump_to: Option<String> = None;
                    let mut remove_idx: Option<usize> = None;
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for (idx, bm) in self.bookmarks.iter().enumerate() {
                            ui.horizontal(|ui| {
                                if ui.button(format!("📁 {}", bm)).clicked() {
                                    jump_to = Some(bm.clone());
                                }
                                if ui.small_button("✖").on_hover_text("Favorit löschen").clicked() {
                                    remove_idx = Some(idx);
                                }
                            });
                        }
                    });
                    if let Some(idx) = remove_idx {
                        self.bookmarks.remove(idx);
                        crate::tools::save_bookmarks(&self.bookmarks);
                    }
                    if let Some(target_dir) = jump_to {
                        let pane = if self.active_pane_is_left { &mut self.left_pane } else { &mut self.right_pane };
                        pane.vfs_type = VfsType::Local;
                        pane.navigate_to(PathBuf::from(target_dir));
                        self.show_bookmarks_dialog = false;
                    }
                    ui.separator();
                    if ui.button("Schließen").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.show_bookmarks_dialog = false;
                    }
                });
        }

        // Feature 5: File Properties & Checksums Dialog (Alt+Enter)
        if self.show_properties_dialog {
            egui::Window::new("Datei-Eigenschaften & Prüfsummen (Alt+Enter)")
                .collapsible(false)
                .resizable(true)
                .default_width(540.0)
                .show(ctx, |ui| {
                    egui::Grid::new("props_grid").striped(true).min_col_width(120.0).show(ui, |ui| {
                        ui.strong("Name:");
                        ui.label(&self.properties_state.name);
                        ui.end_row();

                        ui.strong("Pfad:");
                        ui.label(&self.properties_state.full_path);
                        ui.end_row();

                        ui.strong("Typ:");
                        ui.label(if self.properties_state.is_dir {
                            format!("Ordner ({} Dateien, {} Unterordner)", self.properties_state.file_count, self.properties_state.dir_count)
                        } else {
                            "Datei".to_string()
                        });
                        ui.end_row();

                        ui.strong("Größe:");
                        ui.label(format!(
                            "{} ({} Bytes)",
                            format_size_binary(self.properties_state.size),
                            self.properties_state.size
                        ));
                        ui.end_row();

                        ui.strong("Erstellt am:");
                        ui.label(&self.properties_state.created);
                        ui.end_row();

                        ui.strong("Geändert am:");
                        ui.label(&self.properties_state.modified);
                        ui.end_row();

                        ui.strong("Attribute:");
                        if ui.checkbox(&mut self.properties_state.readonly, "Schreibgeschützt (Read-Only)").changed() {
                            if let Ok(meta) = std::fs::metadata(&self.properties_state.full_path) {
                                let mut perms = meta.permissions();
                                perms.set_readonly(self.properties_state.readonly);
                                let _ = std::fs::set_permissions(&self.properties_state.full_path, perms);
                            }
                        }
                        ui.end_row();
                    });

                    if !self.properties_state.is_dir {
                        ui.separator();
                        ui.strong("Prüfsummen (Hashes):");
                        if self.properties_state.calculating {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Berechne CRC32, MD5 und SHA-256...");
                            });
                        } else {
                            egui::Grid::new("hash_grid").striped(true).show(ui, |ui| {
                                ui.strong("CRC32:");
                                ui.monospace(&self.properties_state.crc32);
                                if ui.small_button("📋 Kopieren").clicked() {
                                    ctx.copy_text(self.properties_state.crc32.clone());
                                }
                                ui.end_row();

                                ui.strong("MD5:");
                                ui.monospace(&self.properties_state.md5);
                                if ui.small_button("📋 Kopieren").clicked() {
                                    ctx.copy_text(self.properties_state.md5.clone());
                                }
                                ui.end_row();

                                ui.strong("SHA-256:");
                                ui.monospace(&self.properties_state.sha256);
                                if ui.small_button("📋 Kopieren").clicked() {
                                    ctx.copy_text(self.properties_state.sha256.clone());
                                }
                                ui.end_row();
                            });

                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label("Hash vergleichen:");
                                ui.add(egui::TextEdit::singleline(&mut self.properties_state.compare_hash).desired_width(300.0).hint_text("Prüfsumme zum Vergleichen einfügen..."));
                            });
                            let cmp = self.properties_state.compare_hash.trim().to_lowercase();
                            if !cmp.is_empty() {
                                let matches = cmp == self.properties_state.crc32.to_lowercase()
                                    || cmp == self.properties_state.md5.to_lowercase()
                                    || cmp == self.properties_state.sha256.to_lowercase();
                                if matches {
                                    ui.colored_label(egui::Color32::LIGHT_GREEN, "✅ Prüfsumme stimmt exakt überein!");
                                } else {
                                    ui.colored_label(egui::Color32::LIGHT_RED, "❌ Prüfsumme weicht ab!");
                                }
                            }
                        }
                    }

                    ui.separator();
                    if ui.button("Schließen").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.show_properties_dialog = false;
                    }
                });
        }

        // Feature 6: Command Line Output Window
        if self.show_cmdline_output {
            egui::Window::new(if is_de { "Kommandozeilen-Ausgabe" } else { "Command Line Output" })
                .collapsible(false)
                .resizable(true)
                .default_width(620.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        let mut ro = ReadOnlyTextBuffer(&self.cmdline_output);
                        ui.add(
                            egui::TextEdit::multiline(&mut ro)
                                .font(egui::FontId::monospace(13.0))
                                .desired_width(f32::INFINITY)
                                .desired_rows(14),
                        );
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(if is_de { "📋 Kopieren" } else { "📋 Copy" }).clicked() {
                            ctx.copy_text(self.cmdline_output.clone());
                        }
                        if ui.button(if is_de { "Schließen" } else { "Close" }).clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_cmdline_output = false;
                        }
                    });
                });
        }

        // Settings Dialog (Themes, Languages & Info Field)
        if self.show_settings_dialog {
            egui::Window::new(if is_de { "⚙ Einstellungen" } else { "⚙ Settings" })
                .collapsible(false)
                .resizable(false)
                .default_width(460.0)
                .show(ctx, |ui| {
                    let mut changed = false;
                    let lang = self.settings.language;

                    ui.heading(if is_de { "🌐 Sprache / Language" } else { "🌐 Language / Sprache" });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for l in [crate::tools::AppLanguage::Deutsch, crate::tools::AppLanguage::English] {
                            if ui.selectable_label(self.settings.language == l, l.label()).clicked() {
                                self.settings.language = l;
                                changed = true;
                            }
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.heading(if is_de { "🎨 Farbschema / Theme" } else { "🎨 Color Theme" });
                    ui.add_space(4.0);
                    for th in [
                        crate::tools::AppTheme::Dark,
                        crate::tools::AppTheme::Light,
                        crate::tools::AppTheme::ClassicBlue,
                        crate::tools::AppTheme::Midnight,
                        crate::tools::AppTheme::SolarizedDark,
                    ] {
                        if ui.radio_value(&mut self.settings.theme, th, th.label(lang)).changed() {
                            changed = true;
                        }
                    }

                    if changed {
                        crate::tools::save_settings(&self.settings);
                        crate::tools::apply_theme(ctx, self.settings.theme);
                    }

                    ui.add_space(12.0);
                    ui.separator();
                    // Info Feld FARO COMMANDER (C) 2026 R.Trunk
                    egui::Frame::group(ui.style())
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new(concat!("FARO COMMANDER v", env!("CARGO_PKG_VERSION"), " (C) 2026 R.Trunk"))
                                        .strong()
                                        .size(15.0)
                                        .color(egui::Color32::from_rgb(104, 210, 255)),
                                );
                                ui.small(if is_de {
                                    "Dual-Pane Dateimanager & Netzwerk-Suite • Lizenz: MIT"
                                } else {
                                    "Dual-Pane File Manager & Network Suite • License: MIT"
                                });
                            });
                        });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(if is_de { "ℹ Programm-Info & Bibliotheken öffnen" } else { "ℹ Open About & Libraries" }).clicked() {
                            self.show_about_dialog = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(if is_de { "Schließen (Esc)" } else { "Close (Esc)" }).clicked()
                                || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                self.show_settings_dialog = false;
                            }
                        });
                    });
                });
        }

        // Info / About Dialog
        if self.show_about_dialog {
            egui::Window::new(if is_de { "ℹ Info — FARO COMMANDER" } else { "ℹ About — FARO COMMANDER" })
                .id(egui::Id::new("faro_about_dialog_v3"))
                .collapsible(false)
                .resizable(false)
                .default_width(640.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        if let Some(tex) = &self.splash_texture {
                            ui.add(egui::Image::from_texture((tex.id(), egui::vec2(96.0, 96.0))));
                        }
                        ui.add_space(4.0);
                        ui.heading(concat!("FARO COMMANDER v", env!("CARGO_PKG_VERSION")));
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new("FARO COMMANDER (C) 2026 R.Trunk • MIT License")
                                .strong()
                                .size(14.0)
                                .color(egui::Color32::GOLD),
                        );
                        ui.add_space(4.0);
                        ui.label(if is_de {
                            "Moderner Zwei-Fenster-Dateimanager & Netzwerk-Suite für Windows\nLocal • FTP • SFTP • WebDAV • RDP • SSH • NetScanner • PassGen • Archive"
                        } else {
                            "Modern Dual-Pane File Manager & Network Suite for Windows\nLocal • FTP • SFTP • WebDAV • RDP • SSH • NetScanner • PassGen • Archives"
                        });
                    });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.label(
                        egui::RichText::new(if is_de {
                            "📚 Verwendete Open-Source-Bibliotheken (Crates)"
                        } else {
                            "📚 Used Open-Source Libraries (Crates)"
                        })
                        .strong()
                        .color(egui::Color32::from_rgb(104, 210, 255)),
                    );
                    ui.add_space(4.0);

                    let libs: &[(&str, &str, &str, &str)] = &[
                        ("egui / eframe / egui_extras", "0.27.2", "MIT OR Apache-2.0", if is_de { "GUI-Framework & Rendering" } else { "GUI Framework & Rendering" }),
                        ("egui_code_editor", "0.2.7", "MIT", if is_de { "Integrierter Code-Editor & Syntax-Highlighting" } else { "Built-in Code Editor & Syntax Highlighting" }),
                        ("tokio / futures", "1.53.2 / 0.3", "MIT", if is_de { "Asynchrone Runtime & Netzwerk-Tasks" } else { "Async Runtime & Network Tasks" }),
                        ("opendal", "0.59.4", "Apache-2.0", if is_de { "Abstraktionsschicht für FTP & WebDAV" } else { "Data Access Layer (FTP, WebDAV)" }),
                        ("russh / russh-sftp", "0.64.1 / 3.0.1", "Apache-2.0", if is_de { "SSH2-Client, Terminal & SFTP-Dateitransfer" } else { "SSH2 Client, Terminal & SFTP File Transfer" }),
                        ("ironrdp / ironrdp-tokio / ironrdp-tls", "0.13.0", "MIT OR Apache-2.0", if is_de { "Nativer Remote Desktop Protocol (RDP) Client" } else { "Native Remote Desktop Protocol (RDP) Client" }),
                        ("netscli-core", "0.3.4", "MIT", if is_de { "Netzwerk-Scanner (Subnetz, Port-Scan, OS-Fingerprinting, DNS/mDNS, Ping/Trace)" } else { "Network Scanner (Subnet, Ports, OS Hint, DNS/mDNS, Ping/Trace)" }),
                        ("zxcvbn", "3.1.1", "MIT", if is_de { "Passwort-Stärke- & Wörterbuch-Musteranalyse" } else { "Password Strength & Dictionary Pattern Estimator" }),
                        ("rustls / ring", "0.23.45 / 0.17", "MIT / Apache-2.0 / ISC", if is_de { "TLS-Verschlüsselung & Kryptographischer CSPRNG" } else { "TLS Cryptography & System CSPRNG" }),
                        ("zip / sevenz-rust / tar / flate2 / bzip2", "9.0 / 0.6 / 0.4 / 1.1 / 0.6", "MIT / Apache-2.0", if is_de { "Archiv-Verarbeitung (ZIP, 7z, TAR, GZ, BZ2)" } else { "Archive Support (ZIP, 7z, TAR, GZ, BZ2)" }),
                        ("sha2 / md5 / crc32fast", "0.10 / 0.8 / 1.5", "MIT OR Apache-2.0", if is_de { "Prüfsummen-Berechnung (SHA-256, MD5, CRC32)" } else { "Checksum Hashing (SHA-256, MD5, CRC32)" }),
                        ("serde / serde_json", "1.0", "MIT OR Apache-2.0", if is_de { "Konfigurations- & Profil-Serialisierung" } else { "Configuration & Profile Serialization" }),
                        ("chrono / walkdir / url / image", "0.4 / 2.5 / 2.5 / 0.24", "MIT / Apache-2.0 / Unlicense", if is_de { "Zeitstempel, Dateibaum-Suche, Bildvorschau & Hilfsfunktionen" } else { "Timestamps, Directory Traversal, Image Preview & Utilities" }),
                    ];

                    egui::ScrollArea::vertical()
                        .id_source("about_libs_scroll")
                        .max_height(210.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            egui::Grid::new("about_libs_grid")
                                .striped(true)
                                .num_columns(4)
                                .spacing([10.0, 5.0])
                                .show(ui, |ui| {
                                    ui.strong(if is_de { "Bibliothek" } else { "Library" });
                                    ui.strong("Version");
                                    ui.strong(if is_de { "Lizenz" } else { "License" });
                                    ui.strong(if is_de { "Verwendung" } else { "Purpose" });
                                    ui.end_row();

                                    for (name, ver, lic, desc) in libs {
                                        ui.monospace(*name);
                                        ui.small(*ver);
                                        ui.colored_label(egui::Color32::from_rgb(130, 220, 160), *lic);
                                        ui.small(*desc);
                                        ui.end_row();
                                    }
                                });

                            ui.add_space(6.0);
                            ui.collapsing(if is_de { "📜 MIT-Lizenztext anzeigen" } else { "📜 Show MIT License Text" }, |ui| {
                                ui.small(include_str!("../LICENSE"));
                            });
                        });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.vertical_centered(|ui| {
                        let ok_btn = egui::Button::new(egui::RichText::new("OK (Esc)").strong())
                            .min_size(egui::vec2(140.0, 30.0));
                        if ui.add(ok_btn).clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_about_dialog = false;
                        }
                    });
                    ui.add_space(4.0);
                });
        }

        // Handle Drag & Drop (supports files and folders recursively)
        if !ctx.input(|i| i.raw.dropped_files.is_empty()) {
            let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
            let target_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
            let dst_handle = target_pane.get_vfs_handle();
            let dst_path_base = target_pane.current_path.clone();
            
            let tx_left = self.left_pane.tx.clone();
            let tx_right = self.right_pane.tx.clone();
            let ctx_clone = ctx.clone();
            let task_tx = self.task_tx.clone();
            
            tokio::spawn(async move {
                for file in dropped_files {
                    if let Some(path) = file.path {
                        let src_path = path.to_string_lossy().into_owned();
                        let file_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        let dst_path = dst_path_base.join(&file_name).to_string_lossy().into_owned();
                        
                        let src_handle = crate::file_ops::VfsHandle::Local;
                        let _ = crate::file_ops::copy_file_with_progress(&src_handle, &src_path, &dst_handle, &dst_path, task_tx.clone(), file_name).await;
                    }
                }
                let _ = tx_left.send(messages::PaneMessage::Refresh);
                let _ = tx_right.send(messages::PaneMessage::Refresh);
                ctx_clone.request_repaint();
            });
        }

        if self.show_pack_dialog {
            egui::Window::new("Dateien packen (Alt+F5)")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("Packe {} in Archiv:", self.pack_source_desc));
                    ui.add_space(4.0);
                    ui.label("Archivformat wählen:");
                    let formats = [
                        crate::archive_ops::ArchiveFormat::Zip,
                        crate::archive_ops::ArchiveFormat::SevenZ,
                        crate::archive_ops::ArchiveFormat::TarGz,
                        crate::archive_ops::ArchiveFormat::TarBz2,
                        crate::archive_ops::ArchiveFormat::Tar,
                        crate::archive_ops::ArchiveFormat::Gz,
                        crate::archive_ops::ArchiveFormat::Bz2,
                    ];
                    ui.horizontal_wrapped(|ui| {
                        for fmt in formats {
                            if ui.selectable_label(self.pack_format == fmt, fmt.label()).clicked() {
                                self.pack_format = fmt;
                                self.pack_target_path = crate::archive_ops::ArchiveFormat::replace_extension(&self.pack_target_path, fmt);
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.label("Zieldatei:");
                    ui.add(egui::TextEdit::singleline(&mut self.pack_target_path).desired_width(460.0));
                    ui.add_space(4.0);
                    ui.small("Hinweis: Mehrere Dateien/Ordner werden bei GZ/BZ2 automatisch als TAR.GZ/TAR.BZ2 gepackt.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK (Packen)").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let sources = self.pack_sources.clone();
                            let target = self.pack_target_path.clone();
                            let format = self.pack_format;
                            let tx_left = self.left_pane.tx.clone();
                            let tx_right = self.right_pane.tx.clone();
                            let ctx_clone = ctx.clone();
                            let task_tx = self.task_tx.clone();

                            tokio::task::spawn_blocking(move || {
                                if let Err(e) = crate::archive_ops::pack_archive(sources, &target, format, task_tx) {
                                    let _ = tx_left.send(messages::PaneMessage::SftpError(format!("Pack-Fehler: {}", e)));
                                }
                                let _ = tx_left.send(messages::PaneMessage::Refresh);
                                let _ = tx_right.send(messages::PaneMessage::Refresh);
                                ctx_clone.request_repaint();
                            });

                            self.show_pack_dialog = false;
                        }
                        if ui.button("Abbrechen").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_pack_dialog = false;
                        }
                    });
                });
        }

        if self.show_unpack_dialog {
            egui::Window::new("Archiv entpacken (Alt+F9)")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("Entpacke '{}' nach:", self.unpack_archive_path));
                    ui.add(egui::TextEdit::singleline(&mut self.unpack_target_dir).desired_width(460.0));
                    ui.add_space(4.0);
                    ui.small("Unterstützte Formate: ZIP, 7Z, TAR.GZ / TGZ, TAR.BZ2 / TBZ2, TAR, GZ, BZ2, RAR");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("OK (Entpacken)").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let archive_path = self.unpack_archive_path.clone();
                            let target_dir = self.unpack_target_dir.clone();
                            let tx_left = self.left_pane.tx.clone();
                            let tx_right = self.right_pane.tx.clone();
                            let ctx_clone = ctx.clone();
                            let task_tx = self.task_tx.clone();

                            tokio::task::spawn_blocking(move || {
                                if let Err(e) = crate::archive_ops::extract_archive(&archive_path, &target_dir, task_tx) {
                                    let _ = tx_left.send(messages::PaneMessage::SftpError(format!("Entpack-Fehler: {}", e)));
                                }
                                let _ = tx_left.send(messages::PaneMessage::Refresh);
                                let _ = tx_right.send(messages::PaneMessage::Refresh);
                                ctx_clone.request_repaint();
                            });

                            self.show_unpack_dialog = false;
                        }
                        if ui.button("Abbrechen").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_unpack_dialog = false;
                        }
                    });
                });
        }

        if self.show_transfer_dialog {
            let is_move = self.transfer_is_move;
            let title = if is_move { "Move/Rename (F6)" } else { "Copy (F5)" };
            let verb = if is_move { "Move" } else { "Copy" };
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("{} {} to:", verb, self.transfer_source_path));
                    let response = ui.add(egui::TextEdit::singleline(&mut self.transfer_target_path).desired_width(400.0));
                    response.request_focus();
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let sources = self.transfer_sources.clone();
                            let dst_input = self.transfer_target_path.clone();
                            let src_handle = if self.active_pane_is_left { self.left_pane.get_vfs_handle() } else { self.right_pane.get_vfs_handle() };
                            let dst_handle = if self.active_pane_is_left { self.right_pane.get_vfs_handle() } else { self.left_pane.get_vfs_handle() };
                            let tx_left = self.left_pane.tx.clone();
                            let tx_right = self.right_pane.tx.clone();
                            let ctx_clone = ctx.clone();
                            let task_tx = self.task_tx.clone();

                            tokio::spawn(async move {
                                let is_batch = sources.len() > 1;
                                for (src_path, file_name, _is_dir) in sources {
                                    let dst_path = if is_batch {
                                        std::path::Path::new(&dst_input).join(&file_name).to_string_lossy().into_owned()
                                    } else {
                                        dst_input.clone()
                                    };
                                    if is_move {
                                        let _ = crate::file_ops::move_file(&src_handle, &src_path, &dst_handle, &dst_path, task_tx.clone(), file_name).await;
                                    } else {
                                        let _ = crate::file_ops::copy_file_with_progress(&src_handle, &src_path, &dst_handle, &dst_path, task_tx.clone(), file_name).await;
                                    }
                                }
                                let _ = tx_left.send(messages::PaneMessage::Refresh);
                                let _ = tx_right.send(messages::PaneMessage::Refresh);
                                ctx_clone.request_repaint();
                            });

                            self.show_transfer_dialog = false;
                        }
                        if ui.button("Cancel").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_transfer_dialog = false;
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
                            let path = self.mkdir_path.clone();
                            let handle = if self.active_pane_is_left { self.left_pane.get_vfs_handle() } else { self.right_pane.get_vfs_handle() };
                            let tx_left = self.left_pane.tx.clone();
                            let tx_right = self.right_pane.tx.clone();
                            let ctx_clone = ctx.clone();
                            
                            tokio::spawn(async move {
                                let _ = crate::file_ops::create_dir(&handle, &path).await;
                                let _ = tx_left.send(messages::PaneMessage::Refresh);
                                let _ = tx_right.send(messages::PaneMessage::Refresh);
                                ctx_clone.request_repaint();
                            });
                            
                            self.show_mkdir_dialog = false;
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
                            let sources = self.delete_sources.clone();
                            let handle = if self.active_pane_is_left { self.left_pane.get_vfs_handle() } else { self.right_pane.get_vfs_handle() };
                            let tx_left = self.left_pane.tx.clone();
                            let tx_right = self.right_pane.tx.clone();
                            let ctx_clone = ctx.clone();
                            let task_tx = self.task_tx.clone();
                            
                            tokio::spawn(async move {
                                let total = sources.len().max(1);
                                for (idx, (path, is_dir)) in sources.into_iter().enumerate() {
                                    let _ = task_tx.send((format!("Deleting {}", path), idx as f32 / total as f32));
                                    let _ = crate::file_ops::delete_path(&handle, &path, is_dir).await;
                                }
                                let _ = task_tx.send(("Done".to_string(), 1.0));
                                let _ = tx_left.send(messages::PaneMessage::Refresh);
                                let _ = tx_right.send(messages::PaneMessage::Refresh);
                                ctx_clone.request_repaint();
                            });
                            
                            self.show_delete_dialog = false;
                        }
                        if ui.button("No").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_delete_dialog = false;
                        }
                    });
                });
        }
        
        if self.show_search_dialog {
            let mut search_action_goto: Option<String> = None;
            let mut search_action_open: Option<(String, bool)> = None;

            egui::Window::new("Find Files (Alt+F7)")
                .default_width(620.0)
                .default_height(450.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Search for:");
                        let response = ui.add(egui::TextEdit::singleline(&mut self.search_query).desired_width(300.0));
                        if self.search_results.is_empty() && !self.search_running {
                            response.request_focus();
                        }
                        if ui.button("Find").clicked() || (response.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter))) {
                            if !self.search_running {
                                self.search_results.clear();
                                self.search_selected = None;
                                self.search_running = true;
                                let search_tx = self.search_tx.clone();
                                let query = self.search_query.clone();
                                
                                let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
                                let start_path = active_pane.current_path.to_string_lossy().into_owned();
                                let handle = active_pane.get_vfs_handle();
                                
                                tokio::spawn(async move {
                                    if let crate::file_ops::VfsHandle::Local = handle {
                                        let walker = walkdir::WalkDir::new(start_path).into_iter();
                                        for entry in walker.filter_map(|e| e.ok()) {
                                            let file_name = entry.file_name().to_string_lossy();
                                            if file_name.to_lowercase().contains(&query.to_lowercase()) {
                                                let _ = search_tx.send(entry.path().to_string_lossy().into_owned());
                                            }
                                        }
                                    } else {
                                        let _ = search_tx.send("Search on remote not fully implemented yet".to_string());
                                    }
                                    let _ = search_tx.send("##DONE##".to_string());
                                });
                            }
                        }
                        if ui.button("Close").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_search_dialog = false;
                        }
                    });
                    
                    if self.search_running {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Searching... ({} found)", self.search_results.len()));
                        });
                    } else if !self.search_results.is_empty() {
                        ui.label(format!("{} files found (Double-click to Go to file):", self.search_results.len()));
                    }
                    
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for (idx, res) in self.search_results.iter().enumerate() {
                            let is_sel = self.search_selected == Some(idx);
                            let resp = ui.selectable_label(is_sel, res);
                            if resp.clicked() {
                                self.search_selected = Some(idx);
                            }
                            if resp.double_clicked() {
                                search_action_goto = Some(res.clone());
                            }
                        }
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        let selected_path = self.search_selected.and_then(|idx| self.search_results.get(idx)).cloned();
                        ui.add_enabled_ui(selected_path.is_some(), |ui| {
                            if ui.button("Go to file").clicked() {
                                search_action_goto = selected_path.clone();
                            }
                            if ui.button("View (F3)").clicked() {
                                if let Some(p) = &selected_path {
                                    search_action_open = Some((p.clone(), true));
                                }
                            }
                            if ui.button("Edit (F4)").clicked() {
                                if let Some(p) = &selected_path {
                                    search_action_open = Some((p.clone(), false));
                                }
                            }
                        });
                    });
                });

            if let Some(target_str) = search_action_goto {
                let target_path = PathBuf::from(&target_str);
                let pane = if self.active_pane_is_left { &mut self.left_pane } else { &mut self.right_pane };
                if target_path.is_dir() {
                    pane.current_path = target_path;
                    pane.refresh();
                    pane.selected_index = 0;
                } else if let Some(parent) = target_path.parent() {
                    let file_name = target_path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    pane.current_path = parent.to_path_buf();
                    pane.refresh();
                    if let Some(pos) = pane.files.iter().position(|f| f.full_name().eq_ignore_ascii_case(&file_name)) {
                        pane.selected_index = pos;
                    }
                }
                self.show_search_dialog = false;
            }

            if let Some((target_str, view_only)) = search_action_open {
                let target_path = PathBuf::from(&target_str);
                if target_path.is_file() {
                    self.open_editor_for_path(target_str, crate::file_ops::VfsHandle::Local, view_only, ctx);
                }
            }
        }

        if self.show_sync_dialog {
            egui::Window::new("Synchronize Directories")
                .default_width(700.0)
                .default_height(480.0)
                .show(ctx, |ui| {
                    let left_root = self.left_pane.current_path.clone();
                    let right_root = self.right_pane.current_path.clone();
                    ui.horizontal(|ui| {
                        ui.label(format!("Left: {}", left_root.display()));
                        ui.separator();
                        ui.label(format!("Right: {}", right_root.display()));
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Compare").clicked() {
                            self.compare_directories();
                        }
                        ui.checkbox(&mut self.sync_hide_identical, "Hide identical files");
                        if ui.button("Synchronize Selected").clicked() {
                            let entries: Vec<SyncEntry> = self.sync_entries.iter().filter(|e| e.selected && e.status != "Identical").cloned().collect();
                            let l_root = left_root.clone();
                            let r_root = right_root.clone();
                            let task_tx = self.task_tx.clone();
                            let tx_left = self.left_pane.tx.clone();
                            let tx_right = self.right_pane.tx.clone();
                            let ctx_clone = ctx.clone();

                            tokio::spawn(async move {
                                let h = crate::file_ops::VfsHandle::Local;
                                for entry in entries {
                                    let l_path = l_root.join(&entry.rel_path).to_string_lossy().into_owned();
                                    let r_path = r_root.join(&entry.rel_path).to_string_lossy().into_owned();
                                    match entry.status {
                                        "Left only" | "Left newer / Diff" => {
                                            let _ = crate::file_ops::copy_file_with_progress(&h, &l_path, &h, &r_path, task_tx.clone(), entry.rel_path.clone()).await;
                                        }
                                        "Right only" | "Right newer" => {
                                            let _ = crate::file_ops::copy_file_with_progress(&h, &r_path, &h, &l_path, task_tx.clone(), entry.rel_path.clone()).await;
                                        }
                                        _ => {}
                                    }
                                }
                                let _ = tx_left.send(messages::PaneMessage::Refresh);
                                let _ = tx_right.send(messages::PaneMessage::Refresh);
                                ctx_clone.request_repaint();
                            });
                            self.show_sync_dialog = false;
                        }
                        if ui.button("Close").clicked() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.show_sync_dialog = false;
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(350.0).show(ui, |ui| {
                        egui::Grid::new("sync_grid").striped(true).min_col_width(80.0).show(ui, |ui| {
                            ui.strong("Sync");
                            ui.strong("Relative Path");
                            ui.strong("Left Size");
                            ui.strong("Status");
                            ui.strong("Right Size");
                            ui.end_row();

                            for entry in &mut self.sync_entries {
                                if self.sync_hide_identical && entry.status == "Identical" {
                                    continue;
                                }
                                ui.checkbox(&mut entry.selected, "");
                                ui.label(&entry.rel_path);
                                ui.label(entry.left_size.map(format_size_binary).unwrap_or_else(|| "-".to_string()));
                                ui.label(entry.status);
                                ui.label(entry.right_size.map(format_size_binary).unwrap_or_else(|| "-".to_string()));
                                ui.end_row();
                            }
                        });
                    });
                });
        }

        // Bottom bar (F-Keys)
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
                ui.columns(7, |cols| {
                    if cols[0].add_sized([cols[0].available_width(), 28.0], egui::Button::new(if is_de { "F3 Ansehen" } else { "F3 View" })).clicked() { trigger_f3 = true; }
                    if cols[1].add_sized([cols[1].available_width(), 28.0], egui::Button::new(if is_de { "F4 Bearbeiten" } else { "F4 Edit" })).clicked() { trigger_f4 = true; }
                    if cols[2].add_sized([cols[2].available_width(), 28.0], egui::Button::new(if is_de { "F5 Kopieren" } else { "F5 Copy" })).clicked() { trigger_f5 = true; }
                    if cols[3].add_sized([cols[3].available_width(), 28.0], egui::Button::new(if is_de { "F6 Bewegen" } else { "F6 RenMov" })).clicked() { trigger_f6 = true; }
                    if cols[4].add_sized([cols[4].available_width(), 28.0], egui::Button::new(if is_de { "F7 NeuerOrd" } else { "F7 Mkdir" })).clicked() { trigger_f7 = true; }
                    if cols[5].add_sized([cols[5].available_width(), 28.0], egui::Button::new(if is_de { "F8 Löschen" } else { "F8 Delete" })).clicked() { trigger_f8 = true; }
                    if cols[6].add_sized([cols[6].available_width(), 28.0], egui::Button::new(if is_de { "Alt+F4 Beenden" } else { "Alt+F4 Exit" })).clicked() { trigger_exit = true; }
                });
            });

        // Command Line Bar right above F-Key bar (Feature 6)
        let cmd_frame = egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(6.0, 3.0))
            .fill(ctx.style().visuals.faint_bg_color);
        egui::TopBottomPanel::bottom("cmdline_panel")
            .frame(cmd_frame)
            .show(ctx, |ui| {
                let active_cwd = if self.active_pane_is_left {
                    self.left_pane.current_path.clone()
                } else {
                    self.right_pane.current_path.clone()
                };
                let cwd_str = active_cwd.to_string_lossy().into_owned();
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::LIGHT_BLUE, egui::RichText::new(format!("{}>", cwd_str)).monospace());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ps_btn = ui.button("⚡ PowerShell").on_hover_text(if is_de {
                            "PowerShell in diesem Ordner öffnen"
                        } else {
                            "Open PowerShell in this folder"
                        });
                        if ps_btn.has_focus() { ps_btn.surrender_focus(); }
                        if ps_btn.clicked() {
                            crate::tools::open_terminal_in_dir(&cwd_str, true);
                        }
                        let cmd_btn = ui.button("🖳 CMD").on_hover_text(if is_de {
                            "Eingabeaufforderung (cmd.exe) in diesem Ordner öffnen"
                        } else {
                            "Open Command Prompt (cmd.exe) in this folder"
                        });
                        if cmd_btn.has_focus() { cmd_btn.surrender_focus(); }
                        if cmd_btn.clicked() {
                            crate::tools::open_terminal_in_dir(&cwd_str, false);
                        }
                        let run_clicked = ui.button(if is_de { "▶ Ausführen" } else { "▶ Run" }).clicked();
                        let cmd_edit = ui.add_sized(
                            ui.available_size(),
                            egui::TextEdit::singleline(&mut self.cmdline_input)
                                .hint_text(if is_de {
                                    "Befehl eingeben (Enter zum Ausführen)..."
                                } else {
                                    "Type command (Press Enter to execute)..."
                                })
                                .font(egui::TextStyle::Monospace),
                        );
                        let enter_in_cmd = cmd_edit.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter));
                        if (run_clicked || enter_in_cmd) && !self.cmdline_input.trim().is_empty() {
                            let cmd = self.cmdline_input.trim().to_string();
                            self.cmdline_input.clear();
                            let res = crate::tools::run_command_capture(&cwd_str, &cmd);
                            self.cmdline_output = format!("{}> {}\n{}", cwd_str, cmd, res);
                            self.show_cmdline_output = true;
                            self.left_pane.refresh();
                            self.right_pane.refresh();
                        }
                    });
                });
            });

        if trigger_f3 { self.action_view(ctx); }
        if trigger_f4 { self.action_edit(ctx); }
        if trigger_f5 { self.action_copy(); }
        if trigger_f6 { self.action_move(); }
        if trigger_f7 { self.action_mkdir(); }
        if trigger_f8 { self.action_delete(); }
        if trigger_exit { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }

        // Top bar (Menus & Info Field)
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button(if is_de { "Dateien" } else { "Files" }, |ui| {
                    if ui.button(if is_de { "Dateien packen... (Alt+F5)" } else { "Pack Files... (Alt+F5)" }).clicked() {
                        self.action_pack();
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "Archiv entpacken... (Alt+F9)" } else { "Unpack Archive... (Alt+F9)" }).clicked() {
                        self.action_unpack();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(if is_de { "Mehrfach-Umbenenn-Tool... (Strg+M)" } else { "Multi-Rename Tool... (Ctrl+M)" }).clicked() {
                        self.action_multi_rename();
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "Direkt umbenennen (Umschalt+F6)" } else { "Inline Rename (Shift+F6)" }).clicked() {
                        self.action_inline_rename();
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "Eigenschaften & Prüfsummen... (Alt+Enter)" } else { "Properties & Checksums... (Alt+Enter)" }).clicked() {
                        self.action_properties(ctx);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(if is_de { "Beenden" } else { "Quit" }).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button(if is_de { "Befehle" } else { "Commands" }, |ui| {
                    if ui.button(if is_de { "Suchen... (Alt+F7)" } else { "Search... (Alt+F7)" }).clicked() {
                        self.show_search_dialog = true;
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "Favoriten / Lesezeichen... (Strg+D)" } else { "Bookmarks / Favorites... (Ctrl+D)" }).clicked() {
                        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
                        self.new_bookmark_input = active_pane.current_path.to_string_lossy().to_string();
                        self.show_bookmarks_dialog = true;
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "Ordnergrößen berechnen (Alt+Umschalt+Enter)" } else { "Calculate Dir Sizes (Alt+Shift+Enter)" }).clicked() {
                        self.action_calculate_dir_sizes(ctx);
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "Verzeichnisse synchronisieren..." } else { "Synchronize Dirs..." }).clicked() {
                        self.compare_directories();
                        self.show_sync_dialog = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button(if is_de { "Netzwerk" } else { "Net" }, |ui| {
                    if ui.button(if is_de { "🖥 SSH Terminal & Profile..." } else { "🖥 SSH Terminal & Profiles..." }).clicked() {
                        self.show_ssh_terminal = true;
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "🖵 RDP Remotedesktop & Verbindungen..." } else { "🖵 RDP Remote Desktop & Connections..." }).clicked() {
                        self.show_rdp_window = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    if ui.button(if is_de { "📡 Netzwerk-Scanner (Subnetz / Ports / OS / DNS)..." } else { "📡 Network Scanner (Subnet / Ports / OS / DNS)..." }).clicked() {
                        self.show_net_scanner = true;
                        ui.close_menu();
                    }
                    if ui.button(if is_de { "🔑 Passwort- & PIN-Generator (Sicherheitsanalyse)..." } else { "🔑 Password & PIN Generator (Security Analyzer)..." }).clicked() {
                        self.show_password_generator = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button(if is_de { "⚙ Einstellungen" } else { "⚙ Settings" }, |ui| {
                    if ui.button(if is_de { "⚙ Einstellungen öffnen..." } else { "⚙ Open Settings..." }).clicked() {
                        self.show_settings_dialog = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(concat!("ℹ Info — FARO COMMANDER v", env!("CARGO_PKG_VERSION"), " (C) 2026 R.Trunk")).clicked() {
                        self.show_about_dialog = true;
                        ui.close_menu();
                    }
                });
                if ui.button(if is_de { "ℹ Info" } else { "ℹ About" }).clicked() {
                    self.show_about_dialog = true;
                }

                // Permanent Info Field on the right of the top menu bar
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let info_btn = ui.add(
                        egui::Label::new(
                            egui::RichText::new(concat!("FARO COMMANDER v", env!("CARGO_PKG_VERSION"), " (C) 2026 R.Trunk"))
                                .strong()
                                .color(egui::Color32::from_rgb(104, 210, 255)),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if info_btn.clicked() {
                        self.show_about_dialog = true;
                    }
                });
            });
        });

        // Process SSH Messages
        while let Ok(msg) = self.ssh_rx.try_recv() {
            match msg {
                messages::SshMessage::Output(text) => {
                    self.ssh_output.push_str(&text);
                }
                messages::SshMessage::Error(err) => {
                    self.ssh_output.push_str(&format!("\n[ERROR]: {}\n", err));
                }
                messages::SshMessage::Disconnected => {
                    self.ssh_output.push_str("\n[Disconnected]\n");
                    self.ssh_tx = None;
                }
            }
        }

        if self.show_ssh_terminal {
            let mut open_win = self.show_ssh_terminal;
            let mut connect_sftp_profile: Option<crate::ssh_client::SshProfile> = None;

            egui::Window::new(if is_de {
                "🖥 SSH Terminal (80×24) & Verschlüsselter Profil-Tresor"
            } else {
                "🖥 SSH Terminal (80×24) & Encrypted Profile Vault"
            })
            .open(&mut open_win)
            .default_width(980.0)
            .default_height(550.0)
            .min_width(720.0)
            .show(ctx, |ui| {
                // Top toolbar (no right-edge clipping)
                ui.horizontal_wrapped(|ui| {
                    let toggle_lbl = if self.ssh_show_sidebar {
                        if is_de { "◀ Tresor & Profile ausblenden" } else { "◀ Hide Vault & Profiles" }
                    } else {
                        if is_de { "▶ Tresor & Profile einblenden" } else { "▶ Show Vault & Profiles" }
                    };
                    if ui.button(toggle_lbl).clicked() {
                        self.ssh_show_sidebar = !self.ssh_show_sidebar;
                    }
                    ui.separator();
                    if self.ssh_tx.is_some() {
                        ui.colored_label(
                            egui::Color32::from_rgb(80, 220, 120),
                            format!(
                                "● {}@{}:{} (80×24)",
                                self.ssh_profile_draft.user,
                                self.ssh_profile_draft.host,
                                self.ssh_profile_draft.port
                            ),
                        );
                        if ui.button(if is_de { "⏹ Trennen" } else { "⏹ Disconnect" }).clicked() {
                            self.ssh_tx = None;
                            self.ssh_output.push_str("\n[Disconnected by user]\n");
                        }
                        if ui.button("⚡ Ctrl+C").on_hover_text("Send SIGINT (0x03)").clicked() {
                            if let Some(tx) = &self.ssh_tx {
                                let _ = tx.try_send("\x03".to_string());
                            }
                        }
                    } else {
                        ui.colored_label(
                            egui::Color32::GRAY,
                            if is_de { "○ Nicht verbunden (80×24 PTY)" } else { "○ Disconnected (80×24 PTY)" },
                        );
                    }
                    ui.separator();
                    if ui.button(if is_de { "📋 Kopieren" } else { "📋 Copy" }).clicked() {
                        ctx.copy_text(self.ssh_output.clone());
                    }
                    if ui.button(if is_de { "🧹 Leeren" } else { "🧹 Clear" }).clicked() {
                        self.ssh_output.clear();
                    }
                });
                ui.separator();

                ui.horizontal_top(|ui| {
                    if self.ssh_show_sidebar {
                        ui.vertical(|ui| {
                            ui.set_width(300.0);
                            self.render_vault_box(ui, ctx, is_de);

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.strong(if is_de { "📁 Gespeicherte Profile" } else { "📁 Saved Profiles" });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button(if is_de { "+ Neu" } else { "+ New" }).clicked() {
                                        self.ssh_selected_profile = None;
                                        self.ssh_profile_draft = crate::ssh_client::SshProfile::default();
                                    }
                                });
                            });

                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_source("ssh_profiles_scroll")
                                    .max_height(85.0)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        if !self.ssh_vault_unlocked {
                                            ui.weak(if is_de {
                                                "Tresor entsperren, um Profile zu laden."
                                            } else {
                                                "Unlock vault to load saved profiles."
                                            });
                                        } else if self.ssh_profiles.is_empty() {
                                            ui.weak(if is_de {
                                                "Noch keine Profile im Tresor."
                                            } else {
                                                "No profiles in vault yet."
                                            });
                                        }
                                        let mut clicked_idx = None;
                                        for (idx, p) in self.ssh_profiles.iter().enumerate() {
                                            let icon = match p.auth_method {
                                                crate::ssh_client::SshAuthMethod::KeyFile => "🔑",
                                                crate::ssh_client::SshAuthMethod::Password => "🔒",
                                            };
                                            let label = format!("{} {} ({}@{}:{})", icon, p.name, p.user, p.host, p.port);
                                            if ui
                                                .selectable_label(self.ssh_selected_profile == Some(idx), label)
                                                .clicked()
                                            {
                                                clicked_idx = Some(idx);
                                            }
                                        }
                                        if let Some(idx) = clicked_idx {
                                            self.ssh_selected_profile = Some(idx);
                                            self.ssh_profile_draft = self.ssh_profiles[idx].clone();
                                        }
                                    });
                            });

                            ui.add_space(4.0);
                            egui::Grid::new("ssh_profile_form_grid")
                                .num_columns(2)
                                .spacing([6.0, 4.0])
                                .show(ui, |ui| {
                                    ui.label(if is_de { "Profilname:" } else { "Profile:" });
                                    ui.add(egui::TextEdit::singleline(&mut self.ssh_profile_draft.name).desired_width(195.0));
                                    ui.end_row();

                                    ui.label("Host / Port:");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::TextEdit::singleline(&mut self.ssh_profile_draft.host).desired_width(135.0));
                                        ui.add(egui::DragValue::new(&mut self.ssh_profile_draft.port).clamp_range(1..=65535));
                                    });
                                    ui.end_row();

                                    ui.label(if is_de { "Benutzer:" } else { "User:" });
                                    ui.add(egui::TextEdit::singleline(&mut self.ssh_profile_draft.user).desired_width(195.0));
                                    ui.end_row();

                                    ui.label("Auth:");
                                    ui.horizontal(|ui| {
                                        ui.radio_value(
                                            &mut self.ssh_profile_draft.auth_method,
                                            crate::ssh_client::SshAuthMethod::KeyFile,
                                            "🔑 SSH-Key",
                                        );
                                        ui.radio_value(
                                            &mut self.ssh_profile_draft.auth_method,
                                            crate::ssh_client::SshAuthMethod::Password,
                                            if is_de { "🔒 Passwort" } else { "🔒 Password" },
                                        );
                                    });
                                    ui.end_row();

                                    match self.ssh_profile_draft.auth_method {
                                        crate::ssh_client::SshAuthMethod::KeyFile => {
                                            ui.label(if is_de { "Key-Datei:" } else { "Key File:" });
                                            ui.vertical(|ui| {
                                                ui.add(
                                                    egui::TextEdit::singleline(&mut self.ssh_profile_draft.key_path)
                                                        .hint_text("~/.ssh/id_ed25519")
                                                        .desired_width(195.0),
                                                );
                                                ui.horizontal(|ui| {
                                                    if ui.small_button("ed25519").clicked() {
                                                        self.ssh_profile_draft.key_path = "~/.ssh/id_ed25519".to_string();
                                                    }
                                                    if ui.small_button("id_rsa").clicked() {
                                                        self.ssh_profile_draft.key_path = "~/.ssh/id_rsa".to_string();
                                                    }
                                                    if ui.small_button("Auto").clicked() {
                                                        self.ssh_profile_draft.key_path = crate::ssh_client::default_ssh_key_path();
                                                    }
                                                });
                                            });
                                            ui.end_row();

                                            ui.label("Passphrase:");
                                            ui.add(
                                                egui::TextEdit::singleline(&mut self.ssh_profile_draft.key_passphrase)
                                                    .password(true)
                                                    .hint_text(if is_de { "(im Tresor geschützt)" } else { "(encrypted in vault)" })
                                                    .desired_width(195.0),
                                            );
                                            ui.end_row();
                                        }
                                        crate::ssh_client::SshAuthMethod::Password => {
                                            ui.label(if is_de { "Passwort:" } else { "Password:" });
                                            ui.add(
                                                egui::TextEdit::singleline(&mut self.ssh_profile_draft.password)
                                                    .password(true)
                                                    .hint_text(if is_de { "(im Tresor geschützt)" } else { "(encrypted in vault)" })
                                                    .desired_width(195.0),
                                            );
                                            ui.end_row();
                                        }
                                    }
                                });

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let save_btn = ui.add_enabled(
                                    self.ssh_vault_unlocked,
                                    egui::Button::new(if is_de { "💾 Im Tresor speichern" } else { "💾 Save in Vault" }),
                                );
                                if save_btn.clicked() {
                                    if let Some(idx) = self.ssh_selected_profile {
                                        if idx < self.ssh_profiles.len() {
                                            self.ssh_profiles[idx] = self.ssh_profile_draft.clone();
                                        }
                                    } else {
                                        self.ssh_profiles.push(self.ssh_profile_draft.clone());
                                        self.ssh_selected_profile = Some(self.ssh_profiles.len() - 1);
                                    }
                                    if let Err(e) = crate::ssh_client::save_vault(&self.ssh_vault_password, &self.ssh_profiles) {
                                        self.ssh_vault_error = Some(e);
                                    }
                                }
                                if self.ssh_vault_unlocked {
                                    if let Some(idx) = self.ssh_selected_profile {
                                        if ui.button(if is_de { "🗑 Löschen" } else { "🗑 Delete" }).clicked() {
                                            self.ssh_profiles.remove(idx);
                                            self.ssh_selected_profile = None;
                                            let _ = crate::ssh_client::save_vault(&self.ssh_vault_password, &self.ssh_profiles);
                                        }
                                    }
                                }
                            });

                            ui.add_space(4.0);
                            ui.separator();
                            ui.vertical_centered_justified(|ui| {
                                if ui
                                    .button(
                                        egui::RichText::new(if is_de {
                                            "▶ SSH-Terminal verbinden"
                                        } else {
                                            "▶ Connect SSH Terminal"
                                        })
                                        .strong(),
                                    )
                                    .clicked()
                                {
                                    let prof = self.ssh_profile_draft.clone();
                                    self.ssh_output.push_str(&format!(
                                        "\nConnecting to {}@{}:{}...\n",
                                        prof.user, prof.host, prof.port
                                    ));
                                    let ui_tx = self.ssh_ui_tx.clone();
                                    let (tx, rx) = tokio::sync::mpsc::channel(64);
                                    self.ssh_tx = Some(tx);
                                    tokio::spawn(async move {
                                        crate::ssh_client::start_ssh_terminal_profile(prof, ui_tx, rx).await;
                                    });
                                }
                                if ui
                                    .button(if is_de {
                                        "📂 Als SFTP im Dateifenster öffnen"
                                    } else {
                                        "📂 Open as SFTP in File Pane"
                                    })
                                    .clicked()
                                {
                                    connect_sftp_profile = Some(self.ssh_profile_draft.clone());
                                }
                            });
                        });
                        ui.separator();
                    }

                    // Right side: Standard 80x24 Terminal Console (min 660x384 px)
                    ui.vertical(|ui| {
                        let term_width = ui.available_width().max(650.0);
                        let term_height = (ui.available_height() - 34.0).max(384.0);
                        egui::Frame::none()
                            .fill(egui::Color32::from_rgb(12, 14, 22))
                            .inner_margin(egui::Margin::same(8.0))
                            .rounding(egui::Rounding::same(4.0))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_source("ssh_terminal_output_scroll")
                                    .max_height(term_height)
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        ui.add_sized(
                                            [term_width - 16.0, term_height],
                                            egui::TextEdit::multiline(&mut self.ssh_output)
                                                .font(egui::FontId::monospace(14.0))
                                                .desired_rows(24)
                                                .text_color(egui::Color32::from_rgb(180, 245, 190))
                                                .frame(false)
                                                .interactive(false),
                                        );
                                    });
                            });

                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(104, 210, 255), egui::RichText::new("$").monospace().strong());
                            let send_btn_w = 78.0;
                            let input_w = (ui.available_width() - send_btn_w - 8.0).max(80.0);
                            let res = ui.add_sized(
                                [input_w, 22.0],
                                egui::TextEdit::singleline(&mut self.ssh_input)
                                    .font(egui::FontId::monospace(14.0))
                                    .hint_text(if is_de {
                                        "Befehl eingeben (Enter = Senden, ▲/▼ = Verlauf)..."
                                    } else {
                                        "Type command (Enter = Send, ▲/▼ = History)..."
                                    }),
                            );
                            if res.has_focus() {
                                if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) && !self.ssh_cmd_history.is_empty() {
                                    let next_idx = match self.ssh_history_idx {
                                        Some(i) => i.saturating_sub(1),
                                        None => self.ssh_cmd_history.len() - 1,
                                    };
                                    self.ssh_history_idx = Some(next_idx);
                                    self.ssh_input = self.ssh_cmd_history[next_idx].clone();
                                } else if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                                    if let Some(i) = self.ssh_history_idx {
                                        if i + 1 < self.ssh_cmd_history.len() {
                                            self.ssh_history_idx = Some(i + 1);
                                            self.ssh_input = self.ssh_cmd_history[i + 1].clone();
                                        } else {
                                            self.ssh_history_idx = None;
                                            self.ssh_input.clear();
                                        }
                                    }
                                }
                            }
                            let enter_pressed = res.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter));
                            let send_clicked = ui.button(if is_de { "Senden ↵" } else { "Send ↵" }).clicked();
                            if enter_pressed || send_clicked {
                                let cmd = self.ssh_input.clone();
                                if !cmd.trim().is_empty() {
                                    self.ssh_cmd_history.push(cmd.clone());
                                }
                                self.ssh_history_idx = None;
                                self.ssh_input.clear();
                                if let Some(tx) = &self.ssh_tx {
                                    let _ = tx.try_send(cmd);
                                }
                                res.request_focus();
                            }
                        });
                    });
                });
            });

            self.show_ssh_terminal = open_win;

            if let Some(prof) = connect_sftp_profile {
                let pane = if self.active_pane_is_left {
                    &mut self.left_pane
                } else {
                    &mut self.right_pane
                };
                pane.vfs_type = VfsType::Sftp;
                pane.connection_string = format!("{}@{}:{}", prof.user, prof.host, prof.port);
                pane.loading = true;
                pane.error_msg = None;
                let tx = pane.tx.clone();
                tokio::spawn(async move {
                    match crate::sftp_client::connect_sftp_profile(&prof).await {
                        Ok((_session, sftp)) => {
                            let _ = tx.send(messages::PaneMessage::SftpConnected(sftp));
                        }
                        Err(e) => {
                            let _ = tx.send(messages::PaneMessage::SftpError(e));
                        }
                    }
                });
            }
        }

        // Process RDP Messages
        while let Ok(msg) = self.rdp_rx.try_recv() {
            match msg {
                rdp_client::RdpUiMessage::Connected { width, height } => {
                    self.rdp_remote_size = (width, height);
                    let init_img = egui::ColorImage::new([width as usize, height as usize], egui::Color32::BLACK);
                    if let Some(tex) = &mut self.rdp_texture {
                        tex.set(init_img, egui::TextureOptions::LINEAR);
                    } else {
                        self.rdp_texture = Some(ctx.load_texture("rdp_desktop", init_img, egui::TextureOptions::LINEAR));
                    }
                    self.rdp_status = if is_de {
                        format!("🟢 Verbunden (Auto {}×{})", width, height)
                    } else {
                        format!("🟢 Connected (Auto {}×{})", width, height)
                    };
                }
                rdp_client::RdpUiMessage::FullFrame { width, height, rgba } => {
                    self.rdp_remote_size = (width, height);
                    let size = [width as usize, height as usize];
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                    if let Some(tex) = &mut self.rdp_texture {
                        tex.set(color_img, egui::TextureOptions::LINEAR);
                    } else {
                        self.rdp_texture = Some(ctx.load_texture(
                            "rdp_desktop",
                            color_img,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
                rdp_client::RdpUiMessage::PartialFrame {
                    total_width,
                    total_height,
                    x,
                    y,
                    w,
                    h,
                    rgba,
                } => {
                    let sub_img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    if let Some(tex) = &mut self.rdp_texture {
                        let [tw, th] = tex.size();
                        if tw == total_width as usize && th == total_height as usize {
                            tex.set_partial([x as usize, y as usize], sub_img, egui::TextureOptions::LINEAR);
                        }
                    }
                }
                rdp_client::RdpUiMessage::Error(err) => {
                    self.rdp_status = format!("❌ {}", err);
                    self.rdp_tx = None;
                    self.rdp_texture = None;
                    if self.rdp_fullscreen {
                        self.rdp_fullscreen = false;
                    }
                }
                rdp_client::RdpUiMessage::Disconnected(reason) => {
                    self.rdp_status = if is_de {
                        format!("🔴 Sitzung beendet: {}", reason)
                    } else {
                        format!("🔴 Session ended: {}", reason)
                    };
                    self.rdp_tx = None;
                    self.rdp_texture = None;
                    if self.rdp_fullscreen {
                        self.rdp_fullscreen = false;
                    }
                }
            }
        }

        if self.show_rdp_window {
            let mut close_rdp = false;
            let rdp_title = if is_de {
                "🖵 RDP Remotedesktop & Verbindungen"
            } else {
                "🖵 RDP Remote Desktop & Connections"
            };

            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("rdp_viewport"),
                egui::ViewportBuilder::default()
                    .with_title(rdp_title)
                    .with_inner_size([1240.0, 780.0])
                    .with_min_inner_size([860.0, 540.0])
                    .with_fullscreen(self.rdp_fullscreen),
                |vp_ctx, _class| {
                    // F11 shortcut to toggle RDP Full Screen
                    if vp_ctx.input(|i| i.key_pressed(egui::Key::F11)) {
                        self.rdp_fullscreen = !self.rdp_fullscreen;
                        vp_ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.rdp_fullscreen));
                        if self.rdp_fullscreen {
                            self.rdp_show_sidebar = false;
                        }
                    }

                    if vp_ctx.input(|i| i.viewport().close_requested()) {
                        close_rdp = true;
                    }

                    egui::TopBottomPanel::top("rdp_top_toolbar").show(vp_ctx, |ui| {
                        ui.horizontal(|ui| {
                            let toggle_lbl = if self.rdp_show_sidebar {
                                if is_de { "◀ Verbindungen ausblenden" } else { "◀ Hide Connections" }
                            } else {
                                if is_de { "▶ Verbindungen einblenden" } else { "▶ Show Connections" }
                            };
                            if ui.button(egui::RichText::new(toggle_lbl).strong()).clicked() {
                                self.rdp_show_sidebar = !self.rdp_show_sidebar;
                            }

                            let fs_lbl = if self.rdp_fullscreen {
                                if is_de { "🗗 Vollbild beenden (F11)" } else { "🗗 Exit Full Screen (F11)" }
                            } else {
                                if is_de { "⛶ Vollbild (F11)" } else { "⛶ Full Screen (F11)" }
                            };
                            if ui.button(egui::RichText::new(fs_lbl).strong()).clicked() {
                                self.rdp_fullscreen = !self.rdp_fullscreen;
                                vp_ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.rdp_fullscreen));
                                if self.rdp_fullscreen {
                                    self.rdp_show_sidebar = false;
                                }
                            }

                            ui.separator();
                            ui.label(egui::RichText::new(&self.rdp_status).strong());
                            ui.separator();
                            ui.weak(if is_de {
                                format!("Auto-Auflösung: {}×{}", self.rdp_auto_size.0, self.rdp_auto_size.1)
                            } else {
                                format!("Auto-Resolution: {}×{}", self.rdp_auto_size.0, self.rdp_auto_size.1)
                            });

                            if let Some(tx) = &self.rdp_tx {
                                let tx_clone = tx.clone();
                                if ui
                                    .button(
                                        egui::RichText::new(if is_de {
                                            "🔌 Sitzung beenden"
                                        } else {
                                            "🔌 End Session"
                                        })
                                        .color(egui::Color32::from_rgb(245, 110, 110))
                                        .strong(),
                                    )
                                    .clicked()
                                {
                                    let _ = tx_clone.try_send(rdp_client::RdpInputMessage::Disconnect);
                                    self.rdp_tx = None;
                                    self.rdp_texture = None;
                                    self.rdp_show_sidebar = true;
                                    self.rdp_status = if is_de {
                                        "🔴 Sitzung beendet.".to_string()
                                    } else {
                                        "🔴 Session ended.".to_string()
                                    };
                                    if self.rdp_fullscreen {
                                        self.rdp_fullscreen = false;
                                        vp_ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                                    }
                                }
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(if is_de { "✖ Schließen" } else { "✖ Close" }).clicked() {
                                    close_rdp = true;
                                }
                            });
                        });
                    });

                    if self.rdp_show_sidebar {
                        egui::SidePanel::left("rdp_connections_sidebar")
                            .exact_width(310.0)
                            .resizable(false)
                            .show(vp_ctx, |ui| {
                                ui.add_space(4.0);
                                self.render_vault_box(ui, vp_ctx, is_de);
                                ui.add_space(4.0);

                                ui.horizontal(|ui| {
                                    ui.strong(if is_de { "🖵 Verbindungen" } else { "🖵 Connections" });
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui
                                            .small_button("◀")
                                            .on_hover_text(if is_de {
                                                "Verbindungs-Seitenleiste ausblenden"
                                            } else {
                                                "Hide Connections sidebar"
                                            })
                                            .clicked()
                                        {
                                            self.rdp_show_sidebar = false;
                                        }
                                        if ui
                                            .add_enabled(self.ssh_vault_unlocked, egui::Button::new(if is_de { "➕ Neu" } else { "➕ New" }))
                                            .clicked()
                                        {
                                            let new_prof = rdp_client::RdpProfile::default();
                                            self.rdp_profile_draft = new_prof.clone();
                                            self.rdp_profiles.push(new_prof);
                                            self.rdp_selected_profile = Some(self.rdp_profiles.len() - 1);
                                            let _ = rdp_client::save_rdp_vault(&self.ssh_vault_password, &self.rdp_profiles);
                                        }
                                    });
                                });
                                ui.separator();

                                egui::ScrollArea::vertical()
                                    .id_source("rdp_profiles_scroll")
                                    .max_height(120.0)
                                    .show(ui, |ui| {
                                        if !self.ssh_vault_unlocked {
                                            ui.weak(if is_de {
                                                "🔒 Tresor entsperren, um gespeicherte Verbindungen zu sehen."
                                            } else {
                                                "🔒 Unlock vault to view saved connections."
                                            });
                                        } else if self.rdp_profiles.is_empty() {
                                            ui.weak(if is_de {
                                                "Noch keine RDP-Verbindungen gespeichert."
                                            } else {
                                                "No saved RDP connections yet."
                                            });
                                        }
                                        let mut clicked_idx = None;
                                        for (idx, p) in self.rdp_profiles.iter().enumerate() {
                                            let selected = self.rdp_selected_profile == Some(idx);
                                            let label = format!("🖵 {} ({}@{}:{})", p.name, p.user, p.host, p.port);
                                            if ui.selectable_label(selected, label).clicked() {
                                                clicked_idx = Some(idx);
                                            }
                                        }
                                        if let Some(idx) = clicked_idx {
                                            self.rdp_selected_profile = Some(idx);
                                            self.rdp_profile_draft = self.rdp_profiles[idx].clone();
                                        }
                                    });

                                ui.separator();
                                ui.strong(if is_de { "Verbindungsdaten" } else { "Connection Details" });

                                egui::Grid::new("rdp_profile_form_grid")
                                    .num_columns(2)
                                    .spacing([6.0, 5.0])
                                    .show(ui, |ui| {
                                        ui.label(if is_de { "Profilname:" } else { "Profile Name:" });
                                        ui.add(egui::TextEdit::singleline(&mut self.rdp_profile_draft.name).desired_width(180.0));
                                        ui.end_row();

                                        ui.label("Host / IP:");
                                        ui.horizontal(|ui| {
                                            ui.add(egui::TextEdit::singleline(&mut self.rdp_profile_draft.host).desired_width(120.0));
                                            ui.label(":");
                                            ui.add(egui::DragValue::new(&mut self.rdp_profile_draft.port).clamp_range(1..=65535));
                                        });
                                        ui.end_row();

                                        ui.label(if is_de { "Domäne:" } else { "Domain:" });
                                        ui.add(egui::TextEdit::singleline(&mut self.rdp_profile_draft.domain).desired_width(180.0));
                                        ui.end_row();

                                        ui.label(if is_de { "Benutzer:" } else { "Username:" });
                                        ui.add(egui::TextEdit::singleline(&mut self.rdp_profile_draft.user).desired_width(180.0));
                                        ui.end_row();

                                        ui.label(if is_de { "Passwort:" } else { "Password:" });
                                        ui.add(
                                            egui::TextEdit::singleline(&mut self.rdp_profile_draft.password)
                                                .password(true)
                                                .desired_width(180.0),
                                        );
                                        ui.end_row();

                                        ui.label(if is_de { "Auflösung:" } else { "Resolution:" });
                                        ui.label(
                                            egui::RichText::new(if is_de {
                                                format!("✨ Automatisch ({}×{})", self.rdp_auto_size.0, self.rdp_auto_size.1)
                                            } else {
                                                format!("✨ Automatic ({}×{})", self.rdp_auto_size.0, self.rdp_auto_size.1)
                                            })
                                            .color(egui::Color32::from_rgb(78, 201, 176)),
                                        );
                                        ui.end_row();

                                        ui.label("CredSSP / NLA:");
                                        ui.checkbox(&mut self.rdp_profile_draft.enable_credssp, if is_de { "Aktiviert" } else { "Enabled" });
                                        ui.end_row();

                                        ui.label("Performance:");
                                        ui.checkbox(
                                            &mut self.rdp_profile_draft.fast_mode,
                                            if is_de { "⚡ Speed-Modus (LAN/Low-Latency)" } else { "⚡ Speed Mode (LAN/Low-Latency)" },
                                        );
                                        ui.end_row();
                                    });

                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    let save_btn = ui.add_enabled(
                                        self.ssh_vault_unlocked,
                                        egui::Button::new(if is_de { "💾 Im Tresor speichern" } else { "💾 Save in Vault" }),
                                    );
                                    if save_btn.clicked() {
                                        if let Some(idx) = self.rdp_selected_profile {
                                            if idx < self.rdp_profiles.len() {
                                                self.rdp_profiles[idx] = self.rdp_profile_draft.clone();
                                            }
                                        } else {
                                            self.rdp_profiles.push(self.rdp_profile_draft.clone());
                                            self.rdp_selected_profile = Some(self.rdp_profiles.len() - 1);
                                        }
                                        let _ = rdp_client::save_rdp_vault(&self.ssh_vault_password, &self.rdp_profiles);
                                    }
                                    if let Some(idx) = self.rdp_selected_profile {
                                        if ui
                                            .add_enabled(self.ssh_vault_unlocked, egui::Button::new(if is_de { "🗑 Löschen" } else { "🗑 Delete" }))
                                            .clicked()
                                        {
                                            if idx < self.rdp_profiles.len() {
                                                self.rdp_profiles.remove(idx);
                                            }
                                            self.rdp_selected_profile = None;
                                            let _ = rdp_client::save_rdp_vault(&self.ssh_vault_password, &self.rdp_profiles);
                                        }
                                    }
                                });

                                ui.add_space(6.0);
                                ui.vertical_centered_justified(|ui| {
                                    if ui
                                        .button(
                                            egui::RichText::new(if is_de {
                                                "🖵 RDP-Sitzung starten"
                                            } else {
                                                "🖵 Start RDP Session"
                                            })
                                            .strong(),
                                        )
                                        .clicked()
                                    {
                                        if let Some(old_tx) = self.rdp_tx.take() {
                                            let _ = old_tx.try_send(rdp_client::RdpInputMessage::Disconnect);
                                        }
                                        let (in_tx, in_rx) = tokio::sync::mpsc::channel(256);
                                        self.rdp_tx = Some(in_tx);
                                        self.rdp_status = if is_de {
                                            format!("⏳ Verbinde ({}×{})...", self.rdp_auto_size.0, self.rdp_auto_size.1)
                                        } else {
                                            format!("⏳ Connecting ({}×{})...", self.rdp_auto_size.0, self.rdp_auto_size.1)
                                        };
                                        let prof = self.rdp_profile_draft.clone();
                                        let init_size = self.rdp_auto_size;
                                        let ui_tx = self.rdp_ui_tx.clone();
                                        let ctx_clone = ctx.clone();
                                        std::thread::spawn(move || {
                                            if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                                                rt.block_on(rdp_client::start_rdp_session(prof, init_size, ui_tx, in_rx, ctx_clone));
                                            }
                                        });
                                    }
                                });
                            });
                    }

                    egui::CentralPanel::default()
                        .frame(egui::Frame::none().fill(egui::Color32::from_rgb(12, 14, 18)))
                        .show(vp_ctx, |ui| {
                            let canvas_avail = ui.available_size();
                            let detected_size = rdp_client::normalize_rdp_resolution(
                                canvas_avail.x.max(640.0) as u16,
                                canvas_avail.y.max(480.0) as u16,
                            );
                            if detected_size != self.rdp_auto_size {
                                self.rdp_auto_size = detected_size;
                                if let Some(tx) = &self.rdp_tx {
                                    let _ = tx.try_send(rdp_client::RdpInputMessage::Resize {
                                        width: detected_size.0,
                                        height: detected_size.1,
                                    });
                                }
                            }

                            if let Some(tex) = &self.rdp_texture {
                                let rem_w = self.rdp_remote_size.0.max(1) as f32;
                                let rem_h = self.rdp_remote_size.1.max(1) as f32;
                                let scale = (canvas_avail.x / rem_w).min(canvas_avail.y / rem_h).max(0.1);
                                let disp_size = egui::vec2(rem_w * scale, rem_h * scale);

                                let (rect, response) = ui.allocate_exact_size(disp_size, egui::Sense::click_and_drag());
                                ui.painter().image(
                                    tex.id(),
                                    rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::WHITE,
                                );

                                if response.clicked() || response.dragged() || response.hovered() {
                                    response.request_focus();
                                }

                                if let Some(tx) = &self.rdp_tx {
                                    let ops = rdp_client::collect_egui_rdp_operations(
                                        ui,
                                        &response,
                                        self.rdp_remote_size.0,
                                        self.rdp_remote_size.1,
                                    );
                                    if !ops.is_empty() {
                                        let _ = tx.try_send(rdp_client::RdpInputMessage::Operations(ops));
                                    }
                                }
                            } else {
                                ui.centered_and_justified(|ui| {
                                    ui.label(
                                        egui::RichText::new(if is_de {
                                            "🖵 Keine aktive RDP-Sitzung.\nWählen Sie links eine Verbindung und klicken Sie auf 'RDP-Sitzung starten'."
                                        } else {
                                            "🖵 No active RDP session.\nSelect a connection on the left and click 'Start RDP Session'."
                                        })
                                        .color(egui::Color32::GRAY),
                                    );
                                });
                            }
                        });
                },
            );

            if close_rdp {
                if let Some(tx) = self.rdp_tx.take() {
                    let _ = tx.try_send(rdp_client::RdpInputMessage::Disconnect);
                }
                self.rdp_texture = None;
                self.rdp_fullscreen = false;
                self.show_rdp_window = false;
            }
        }

        // Process Network Scanner Messages
        while let Ok(msg) = self.net_scan_rx.try_recv() {
            use crate::net_scanner::NetScanUiMessage;
            match msg {
                NetScanUiMessage::Progress { text, fraction } => {
                    self.net_scanner.status_text = text;
                    self.net_scanner.progress_fraction = fraction;
                }
                NetScanUiMessage::DiscoverDone { subnet, entries } => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = format!(
                        "Subnetz {} abgeschlossen — {} aktive Geräte gefunden.",
                        subnet,
                        entries.len()
                    );
                    self.net_scanner.discovered_entries = entries;
                }
                NetScanUiMessage::InspectDone(res) => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = format!(
                        "Host {} ({}) untersucht — {} offene Ports.",
                        res.host,
                        res.ip.map(|i| i.to_string()).unwrap_or_default(),
                        res.open_ports.len()
                    );
                    self.net_scanner.inspect_result = Some(*res);
                }
                NetScanUiMessage::PortScanDone { host, ip, ports } => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    let open_ports: Vec<_> = ports.iter().filter(|p| p.open).cloned().collect();
                    self.net_scanner.status_text = format!(
                        "Port-Scan auf {} ({}) abgeschlossen — {} offene Ports.",
                        host,
                        ip,
                        open_ports.len()
                    );
                    self.net_scanner.inspect_result = Some(netscli_core::InspectResult {
                        host,
                        ip: ip.parse().ok(),
                        ping: None,
                        ports,
                        open_ports,
                        hostname: None,
                        mac: None,
                        vendor: None,
                        os_hint: None,
                    });
                }
                NetScanUiMessage::PingDone(summary) => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = format!(
                        "Ping {} ({}) fertig: {}/{} empfangen ({:.0}% Verlust)",
                        summary.host, summary.ip, summary.received, summary.sent, summary.loss_pct
                    );
                    self.net_scanner.ping_result = Some(summary);
                }
                NetScanUiMessage::TraceDone(trace) => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = format!(
                        "Traceroute zu {} abgeschlossen ({} Zeilen).",
                        trace.host,
                        trace.lines.len()
                    );
                    self.net_scanner.trace_result = Some(trace);
                }
                NetScanUiMessage::DnsDone { host, records } => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = format!(
                        "DNS-Lookup für {} abgeschlossen ({} Einträge).",
                        host,
                        records.len()
                    );
                    self.net_scanner.dns_records = records;
                }
                NetScanUiMessage::ArpDone { arp, interfaces } => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = format!(
                        "ARP-Tabelle ({} Einträge) & {} Schnittstellen geladen.",
                        arp.len(),
                        interfaces.len()
                    );
                    self.net_scanner.arp_entries = arp;
                    self.net_scanner.interfaces = interfaces;
                }
                NetScanUiMessage::MdnsDone(services) => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = format!(
                        "mDNS-Suche abgeschlossen — {} Dienste im LAN gefunden.",
                        services.len()
                    );
                    self.net_scanner.mdns_services = services;
                }
                NetScanUiMessage::Error(err) => {
                    self.net_scanner.running = false;
                    self.net_scanner.progress_fraction = None;
                    self.net_scanner.status_text = "Fehler beim Scan".to_string();
                    self.net_scanner.error_message = Some(err);
                }
            }
        }

        // Network Scanner Window (Tools -> Netzwerk-Scanner)
        if self.show_net_scanner {
            let mut open = self.show_net_scanner;
            let mut action_inspect_ip: Option<String> = None;
            let mut action_ssh_ip: Option<String> = None;
            let mut action_rdp_ip: Option<String> = None;

            egui::Window::new(if is_de {
                "📡 Netzwerk-Scanner & Diagnose"
            } else {
                "📡 Network Scanner & Diagnostics"
            })
            .open(&mut open)
            .default_size([1020.0, 660.0])
            .min_width(780.0)
            .min_height(460.0)
            .resizable(true)
            .show(ctx, |ui| {
                use crate::net_scanner::{DiagTool, NetScannerTab};

                // Top Tab Bar + Status
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.net_scanner.active_tab,
                        NetScannerTab::Discover,
                        if is_de {
                            "🌐 Subnetz-Scan & Sweep"
                        } else {
                            "🌐 Subnet Discover & Sweep"
                        },
                    );
                    ui.selectable_value(
                        &mut self.net_scanner.active_tab,
                        NetScannerTab::Inspect,
                        if is_de {
                            "🔍 Host-Inspektion & Port-Scanner"
                        } else {
                            "🔍 Host Inspect & Port Scanner"
                        },
                    );
                    ui.selectable_value(
                        &mut self.net_scanner.active_tab,
                        NetScannerTab::Diagnostics,
                        if is_de {
                            "🛠 Diagnose (Ping / Trace / DNS / ARP / mDNS)"
                        } else {
                            "🛠 Diagnostics (Ping / Trace / DNS / ARP / mDNS)"
                        },
                    );
                    ui.separator();
                    ui.label(if is_de { "Parallelität:" } else { "Concurrency:" });
                    ui.add(
                        egui::DragValue::new(&mut self.net_scanner.concurrency)
                            .clamp_range(8..=512)
                            .speed(4.0),
                    );
                    ui.label(if is_de { "Timeout (ms):" } else { "Timeout (ms):" });
                    ui.add(
                        egui::DragValue::new(&mut self.net_scanner.scan_timeout_ms)
                            .clamp_range(100..=5000)
                            .speed(25.0),
                    );
                    ui.separator();
                    if ui
                        .button(if is_de { "💾 CSV Export..." } else { "💾 Export CSV..." })
                        .on_hover_text(if is_de {
                            "Dialog öffnen, um die aktuellen Scan-Ergebnisse als CSV-Datei zu speichern"
                        } else {
                            "Open dialog to save current scan results as CSV file"
                        })
                        .clicked()
                    {
                        let target_dir = if self.active_pane_is_left {
                            &self.left_pane.current_path
                        } else {
                            &self.right_pane.current_path
                        };
                        let default_file = target_dir.join(self.net_scanner.default_csv_filename());
                        self.net_scanner.export_path_input =
                            default_file.to_string_lossy().to_string();
                        self.net_scanner.show_export_dialog = true;
                    }
                });

                ui.separator();

                // Progress / Status Bar
                ui.horizontal(|ui| {
                    if self.net_scanner.running {
                        ui.spinner();
                    }
                    ui.label(
                        egui::RichText::new(&self.net_scanner.status_text)
                            .color(egui::Color32::LIGHT_BLUE)
                            .strong(),
                    );
                    if let Some(frac) = self.net_scanner.progress_fraction {
                        ui.add(
                            egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                                .show_percentage()
                                .desired_width(220.0),
                        );
                    }
                });

                if let Some(err) = &self.net_scanner.error_message {
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("⚠ {err}"));
                }

                ui.separator();

                match self.net_scanner.active_tab {
                    NetScannerTab::Discover => {
                        ui.horizontal(|ui| {
                            ui.label(if is_de { "Subnetz (CIDR):" } else { "Subnet (CIDR):" });
                            ui.add(
                                egui::TextEdit::singleline(&mut self.net_scanner.subnet_input)
                                    .desired_width(150.0)
                                    .hint_text("192.168.1.0/24"),
                            );
                            if ui
                                .button(if is_de { "🔄 Auto" } else { "🔄 Auto" })
                                .on_hover_text(if is_de {
                                    "Aktuelles Standard-Subnetz automatisch erkennen"
                                } else {
                                    "Auto-detect default IPv4 subnet"
                                })
                                .clicked()
                            {
                                self.net_scanner.subnet_input =
                                    netscli_core::default_ipv4_subnet_string();
                            }

                            ui.checkbox(
                                &mut self.net_scanner.resolve_hostnames,
                                if is_de {
                                    "Hostnamen (DNS/mDNS)"
                                } else {
                                    "Resolve Hostnames"
                                },
                            );
                            ui.checkbox(
                                &mut self.net_scanner.with_port_sweep,
                                if is_de {
                                    "Mit Port-Sweep"
                                } else {
                                    "With Port Sweep"
                                },
                            );
                            if self.net_scanner.with_port_sweep {
                                ui.label("Ports:");
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.net_scanner.sweep_ports_input,
                                    )
                                    .desired_width(180.0)
                                    .hint_text("22,80,443,445,3389"),
                                );
                            }

                            let scan_btn = ui.add_enabled(
                                !self.net_scanner.running,
                                egui::Button::new(
                                    egui::RichText::new(if is_de {
                                        "▶ Subnetz scannen"
                                    } else {
                                        "▶ Scan Subnet"
                                    })
                                    .strong(),
                                ),
                            );
                            if scan_btn.clicked() {
                                self.net_scanner
                                    .start_discover(self.net_scan_tx.clone(), ctx.clone());
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label(if is_de { "🔍 Filter:" } else { "🔍 Filter:" });
                            ui.add(
                                egui::TextEdit::singleline(&mut self.net_scanner.filter_query)
                                    .desired_width(220.0)
                                    .hint_text(if is_de {
                                        "IP, Hostname, MAC oder Hersteller..."
                                    } else {
                                        "IP, Hostname, MAC or Vendor..."
                                    }),
                            );
                            if self.net_scanner.with_port_sweep {
                                ui.checkbox(
                                    &mut self.net_scanner.only_with_open_ports,
                                    if is_de {
                                        "Nur Geräte mit offenen Ports"
                                    } else {
                                        "Only hosts with open ports"
                                    },
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(format!(
                                        "{} {}",
                                        self.net_scanner.discovered_entries.len(),
                                        if is_de { "Geräte" } else { "Hosts" }
                                    ));
                                },
                            );
                        });

                        ui.separator();

                        let q = self.net_scanner.filter_query.trim().to_lowercase();
                        let only_open =
                            self.net_scanner.with_port_sweep && self.net_scanner.only_with_open_ports;

                        egui::ScrollArea::vertical()
                            .id_source("net_discover_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Grid::new("net_discover_grid")
                                    .striped(true)
                                    .min_col_width(70.0)
                                    .spacing([12.0, 6.0])
                                    .show(ui, |ui| {
                                        ui.strong("IP");
                                        ui.strong(if is_de { "Hostname" } else { "Hostname" });
                                        ui.strong("MAC");
                                        ui.strong(if is_de {
                                            "Hersteller (OUI)"
                                        } else {
                                            "Vendor (OUI)"
                                        });
                                        ui.strong("Ping / Quelle");
                                        ui.strong(if is_de {
                                            "Offene Ports"
                                        } else {
                                            "Open Ports"
                                        });
                                        ui.strong(if is_de { "Aktionen" } else { "Actions" });
                                        ui.end_row();

                                        for entry in &self.net_scanner.discovered_entries {
                                            if only_open && entry.open_ports.is_empty() {
                                                continue;
                                            }
                                            let ip_str = entry.host.ip.to_string();
                                            let host_str =
                                                entry.host.hostname.as_deref().unwrap_or("-");
                                            let mac_str = entry.host.mac.as_deref().unwrap_or("-");
                                            let vendor_str =
                                                entry.host.vendor.as_deref().unwrap_or("-");

                                            if !q.is_empty()
                                                && !ip_str.to_lowercase().contains(&q)
                                                && !host_str.to_lowercase().contains(&q)
                                                && !mac_str.to_lowercase().contains(&q)
                                                && !vendor_str.to_lowercase().contains(&q)
                                            {
                                                continue;
                                            }

                                            ui.monospace(&ip_str);
                                            let name_badge = match entry.host.hostname_source {
                                                Some(netscli_core::NameSource::Mdns) => {
                                                    format!("{host_str} (mDNS)")
                                                }
                                                Some(netscli_core::NameSource::Reverse) => {
                                                    host_str.to_string()
                                                }
                                                None => host_str.to_string(),
                                            };
                                            ui.label(name_badge);
                                            ui.monospace(mac_str);
                                            ui.label(vendor_str);

                                            let rtt_badge = match (
                                                entry.host.rtt_ms,
                                                entry.host.found_by,
                                            ) {
                                                (Some(ms), _) => format!("{ms} ms"),
                                                (None, netscli_core::FoundBy::Neighbor) => {
                                                    "ARP".to_string()
                                                }
                                                (None, netscli_core::FoundBy::Probe) => {
                                                    "<1 ms".to_string()
                                                }
                                            };
                                            ui.label(rtt_badge);

                                            if entry.open_ports.is_empty() {
                                                ui.label("-");
                                            } else {
                                                let ports_summary = entry
                                                    .open_ports
                                                    .iter()
                                                    .map(|p| match &p.service {
                                                        Some(s) => format!("{}/{}", p.port, s),
                                                        None => p.port.to_string(),
                                                    })
                                                    .collect::<Vec<_>>()
                                                    .join(", ");
                                                ui.colored_label(
                                                    egui::Color32::LIGHT_GREEN,
                                                    ports_summary,
                                                );
                                            }

                                            ui.horizontal(|ui| {
                                                if ui
                                                    .small_button(if is_de {
                                                        "🔍 Inspizieren"
                                                    } else {
                                                        "🔍 Inspect"
                                                    })
                                                    .clicked()
                                                {
                                                    action_inspect_ip = Some(ip_str.clone());
                                                }
                                                if ui
                                                    .small_button("🖥 SSH")
                                                    .on_hover_text(if is_de {
                                                        "Im SSH-Terminal öffnen"
                                                    } else {
                                                        "Open in SSH Terminal"
                                                    })
                                                    .clicked()
                                                {
                                                    action_ssh_ip = Some(ip_str.clone());
                                                }
                                                if ui
                                                    .small_button("🖵 RDP")
                                                    .on_hover_text(if is_de {
                                                        "Als RDP-Verbindung öffnen"
                                                    } else {
                                                        "Open as RDP Connection"
                                                    })
                                                    .clicked()
                                                {
                                                    action_rdp_ip = Some(ip_str.clone());
                                                }
                                            });
                                            ui.end_row();
                                        }
                                    });
                            });
                    }

                    NetScannerTab::Inspect => {
                        ui.horizontal(|ui| {
                            ui.label(if is_de { "Host / IP:" } else { "Host / IP:" });
                            ui.add(
                                egui::TextEdit::singleline(&mut self.net_scanner.target_host)
                                    .desired_width(180.0)
                                    .hint_text("192.168.1.1 oder example.com"),
                            );
                            ui.label("Ports:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.net_scanner.scan_ports_input)
                                    .desired_width(220.0)
                                    .hint_text(if is_de {
                                        "Leer = Standard-Ports, oder z.B. 1-1024"
                                    } else {
                                        "Empty = default ports, or e.g. 1-1024"
                                    }),
                            );
                            ui.checkbox(&mut self.net_scanner.use_udp, "UDP");
                            ui.checkbox(
                                &mut self.net_scanner.show_closed_ports,
                                if is_de {
                                    "Geschlossene zeigen"
                                } else {
                                    "Show closed"
                                },
                            );

                            if ui
                                .add_enabled(
                                    !self.net_scanner.running && !self.net_scanner.use_udp,
                                    egui::Button::new(
                                        egui::RichText::new(if is_de {
                                            "🔍 Voll-Inspektion (inkl. OS)"
                                        } else {
                                            "🔍 Full Inspect (incl. OS)"
                                        })
                                        .strong(),
                                    ),
                                )
                                .clicked()
                            {
                                self.net_scanner.start_inspect(
                                    self.net_scan_tx.clone(),
                                    ctx.clone(),
                                    false,
                                );
                            }

                            if ui
                                .add_enabled(
                                    !self.net_scanner.running,
                                    egui::Button::new(if is_de {
                                        "🎯 Nur Port-Scan"
                                    } else {
                                        "🎯 Port Scan Only"
                                    }),
                                )
                                .clicked()
                            {
                                self.net_scanner.start_inspect(
                                    self.net_scan_tx.clone(),
                                    ctx.clone(),
                                    true,
                                );
                            }
                        });

                        ui.separator();

                        if let Some(res) = &self.net_scanner.inspect_result {
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let ip_str = res
                                        .ip
                                        .map(|i| i.to_string())
                                        .unwrap_or_else(|| res.host.clone());
                                    ui.strong(format!("Host: {} ({})", res.host, ip_str));
                                    if let Some(hn) = &res.hostname {
                                        ui.separator();
                                        ui.label(format!("DNS: {hn}"));
                                    }
                                    if let Some(mac) = &res.mac {
                                        ui.separator();
                                        ui.monospace(format!("MAC: {mac}"));
                                    }
                                    if let Some(vendor) = &res.vendor {
                                        ui.label(format!("({vendor})"));
                                    }
                                    if let Some(ping) = &res.ping {
                                        ui.separator();
                                        let status = if ping.alive {
                                            format!(
                                                "Ping: {} ms (TTL: {})",
                                                ping.rtt_ms.unwrap_or(0),
                                                ping.ttl
                                                    .map(|t| t.to_string())
                                                    .unwrap_or_else(|| "-".to_string())
                                            )
                                        } else {
                                            "Ping: Keine Antwort".to_string()
                                        };
                                        ui.label(status);
                                    }
                                    ui.separator();
                                    if ui.small_button("🖥 SSH").clicked() {
                                        action_ssh_ip = Some(ip_str.clone());
                                    }
                                    if ui.small_button("🖵 RDP").clicked() {
                                        action_rdp_ip = Some(ip_str.clone());
                                    }
                                });

                                if let Some(os) = &res.os_hint {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(120, 220, 140),
                                            format!(
                                                "💻 {}: {}",
                                                if is_de {
                                                    "Erkanntes Betriebssystem"
                                                } else {
                                                    "Detected OS"
                                                },
                                                os.summary()
                                            ),
                                        );
                                        if !os.evidence.is_empty() {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "[{}]",
                                                    os.evidence.join(" | ")
                                                ))
                                                .small()
                                                .color(egui::Color32::GRAY),
                                            );
                                        }
                                    });
                                }
                            });

                            ui.add_space(4.0);

                            let show_closed = self.net_scanner.show_closed_ports;
                            egui::ScrollArea::vertical()
                                .id_source("net_inspect_ports_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    egui::Grid::new("net_inspect_ports_grid")
                                        .striped(true)
                                        .min_col_width(65.0)
                                        .spacing([12.0, 6.0])
                                        .show(ui, |ui| {
                                            ui.strong("Port");
                                            ui.strong("Status");
                                            ui.strong(if is_de { "Dienst" } else { "Service" });
                                            ui.strong(if is_de {
                                                "Produkt / Version"
                                            } else {
                                                "Product / Version"
                                            });
                                            ui.strong(if is_de { "Latenz" } else { "Latency" });
                                            ui.strong("Banner / HTTP / TLS Details");
                                            ui.end_row();

                                            for p in &res.ports {
                                                if !show_closed && !p.open {
                                                    continue;
                                                }
                                                ui.monospace(p.port_label());
                                                let (status_txt, color) = match p.status {
                                                    netscli_core::PortStatus::Open => {
                                                        ("OPEN", egui::Color32::LIGHT_GREEN)
                                                    }
                                                    netscli_core::PortStatus::OpenFiltered => (
                                                        "OPEN|FILTERED",
                                                        egui::Color32::from_rgb(240, 200, 80),
                                                    ),
                                                    netscli_core::PortStatus::Filtered => {
                                                        ("FILTERED", egui::Color32::YELLOW)
                                                    }
                                                    netscli_core::PortStatus::Closed => {
                                                        ("CLOSED", egui::Color32::GRAY)
                                                    }
                                                    netscli_core::PortStatus::Error => {
                                                        ("ERROR", egui::Color32::LIGHT_RED)
                                                    }
                                                };
                                                ui.colored_label(color, status_txt);
                                                ui.label(p.service.as_deref().unwrap_or("-"));
                                                ui.label(
                                                    p.product_and_version()
                                                        .unwrap_or_else(|| "-".to_string()),
                                                );
                                                ui.label(
                                                    p.latency_ms
                                                        .map(|ms| format!("{ms} ms"))
                                                        .unwrap_or_else(|| "-".to_string()),
                                                );

                                                let mut details = Vec::new();
                                                if let Some(b) = &p.banner {
                                                    details.push(format!("Banner: {b}"));
                                                }
                                                if let Some(http) = &p.http {
                                                    if let Some(st) = &http.status_line {
                                                        details.push(format!("HTTP: {st}"));
                                                    }
                                                }
                                                if let Some(tls) = &p.tls {
                                                    let proto =
                                                        tls.protocol.as_deref().unwrap_or("TLS");
                                                    let cipher =
                                                        tls.cipher_suite.as_deref().unwrap_or("");
                                                    details.push(format!("TLS: {proto} {cipher}"));
                                                }
                                                ui.label(if details.is_empty() {
                                                    "-".to_string()
                                                } else {
                                                    details.join(" | ")
                                                });
                                                ui.end_row();
                                            }
                                        });
                                });
                        } else {
                            ui.label(
                                egui::RichText::new(if is_de {
                                    "Geben Sie oben eine IP oder einen Hostnamen ein (oder klicken Sie im Subnetz-Scan auf '🔍 Inspizieren')."
                                } else {
                                    "Enter an IP or hostname above (or click '🔍 Inspect' in Subnet Scan)."
                                })
                                .color(egui::Color32::GRAY),
                            );
                        }
                    }

                    NetScannerTab::Diagnostics => {
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.net_scanner.diag_tool,
                                DiagTool::Ping,
                                "📶 Ping",
                            );
                            ui.selectable_value(
                                &mut self.net_scanner.diag_tool,
                                DiagTool::Traceroute,
                                "🛣 Traceroute",
                            );
                            ui.selectable_value(
                                &mut self.net_scanner.diag_tool,
                                DiagTool::Dns,
                                "🌍 DNS-Lookup",
                            );
                            ui.selectable_value(
                                &mut self.net_scanner.diag_tool,
                                DiagTool::ArpAndInterfaces,
                                if is_de {
                                    "🖧 ARP & Schnittstellen"
                                } else {
                                    "🖧 ARP & Interfaces"
                                },
                            );
                            ui.selectable_value(
                                &mut self.net_scanner.diag_tool,
                                DiagTool::Mdns,
                                "📡 mDNS / Bonjour",
                            );
                        });

                        ui.separator();

                        ui.horizontal(|ui| {
                            match self.net_scanner.diag_tool {
                                DiagTool::Ping => {
                                    ui.label("Host / IP:");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.net_scanner.diag_target)
                                            .desired_width(200.0),
                                    );
                                    ui.label(if is_de { "Anzahl:" } else { "Count:" });
                                    ui.add(
                                        egui::DragValue::new(&mut self.net_scanner.ping_count)
                                            .clamp_range(1..=20),
                                    );
                                }
                                DiagTool::Traceroute => {
                                    ui.label("Host / IP:");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.net_scanner.diag_target)
                                            .desired_width(200.0),
                                    );
                                    ui.label("Max Hops:");
                                    ui.add(
                                        egui::DragValue::new(&mut self.net_scanner.trace_max_hops)
                                            .clamp_range(1..=64),
                                    );
                                    ui.checkbox(
                                        &mut self.net_scanner.resolve_hostnames,
                                        if is_de { "Hostnamen auflösen" } else { "Resolve names" },
                                    );
                                }
                                DiagTool::Dns => {
                                    ui.label("Domain / IP:");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.net_scanner.diag_target)
                                            .desired_width(200.0),
                                    );
                                    egui::ComboBox::from_label(if is_de { "Typ" } else { "Type" })
                                        .selected_text(&self.net_scanner.dns_record_type)
                                        .show_ui(ui, |ui| {
                                            for t in [
                                                "ALL", "A", "AAAA", "MX", "TXT", "NS", "CNAME",
                                                "SOA", "PTR",
                                            ] {
                                                ui.selectable_value(
                                                    &mut self.net_scanner.dns_record_type,
                                                    t.to_string(),
                                                    t,
                                                );
                                            }
                                        });
                                }
                                DiagTool::ArpAndInterfaces => {
                                    ui.label(if is_de {
                                        "Lokale ARP-Nachbartabelle und aktive Netzwerkadapter"
                                    } else {
                                        "Local ARP neighbor table and active network interfaces"
                                    });
                                }
                                DiagTool::Mdns => {
                                    ui.label(if is_de {
                                        "Suchdauer (Sekunden):"
                                    } else {
                                        "Browse duration (seconds):"
                                    });
                                    ui.add(
                                        egui::DragValue::new(
                                            &mut self.net_scanner.mdns_timeout_secs,
                                        )
                                        .clamp_range(1..=15),
                                    );
                                }
                            }

                            if ui
                                .add_enabled(
                                    !self.net_scanner.running,
                                    egui::Button::new(
                                        egui::RichText::new(if is_de {
                                            "▶ Ausführen"
                                        } else {
                                            "▶ Run"
                                        })
                                        .strong(),
                                    ),
                                )
                                .clicked()
                            {
                                self.net_scanner
                                    .start_diagnostics(self.net_scan_tx.clone(), ctx.clone());
                            }
                        });

                        ui.separator();

                        egui::ScrollArea::vertical()
                            .id_source("net_diag_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| match self.net_scanner.diag_tool {
                                DiagTool::Ping => {
                                    if let Some(p) = &self.net_scanner.ping_result {
                                        ui.heading(format!("{} ({})", p.host, p.ip));
                                        ui.label(format!(
                                            "Gesendet: {} | Empfangen: {} | Verlust: {:.1}%",
                                            p.sent, p.received, p.loss_pct
                                        ));
                                        if let (Some(min), Some(avg), Some(max)) =
                                            (p.rtt_ms_min, p.rtt_ms_avg, p.rtt_ms_max)
                                        {
                                            ui.colored_label(
                                                egui::Color32::LIGHT_GREEN,
                                                format!(
                                                    "Antwortzeit (RTT): Min = {} ms | Avg = {:.1} ms | Max = {} ms",
                                                    min, avg, max
                                                ),
                                            );
                                        }
                                    }
                                }
                                DiagTool::Traceroute => {
                                    if let Some(tr) = &self.net_scanner.trace_result {
                                        for line in &tr.lines {
                                            ui.monospace(line);
                                        }
                                    }
                                }
                                DiagTool::Dns => {
                                    egui::Grid::new("net_dns_grid")
                                        .striped(true)
                                        .spacing([14.0, 6.0])
                                        .show(ui, |ui| {
                                            ui.strong("Typ");
                                            ui.strong("Name");
                                            ui.strong("Wert");
                                            ui.strong("TTL");
                                            ui.end_row();
                                            for r in &self.net_scanner.dns_records {
                                                ui.monospace(&r.record_type);
                                                ui.label(r.name.as_deref().unwrap_or("-"));
                                                ui.monospace(&r.value);
                                                ui.label(
                                                    r.ttl_seconds
                                                        .map(|s| format!("{s}s"))
                                                        .unwrap_or_else(|| "-".to_string()),
                                                );
                                                ui.end_row();
                                            }
                                        });
                                }
                                DiagTool::ArpAndInterfaces => {
                                    ui.strong(if is_de {
                                        "Netzwerkschnittstellen:"
                                    } else {
                                        "Network Interfaces:"
                                    });
                                    egui::Grid::new("net_if_grid")
                                        .striped(true)
                                        .spacing([12.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.strong("Name");
                                            ui.strong("Status");
                                            ui.strong("MAC");
                                            ui.strong("IP-Netze");
                                            ui.end_row();
                                            for iface in &self.net_scanner.interfaces {
                                                ui.label(&iface.name);
                                                ui.label(if iface.is_up { "UP" } else { "DOWN" });
                                                ui.monospace(
                                                    iface
                                                        .mac
                                                        .map(|m| m.to_string())
                                                        .unwrap_or_else(|| "-".to_string()),
                                                );
                                                let ips = iface
                                                    .ips
                                                    .iter()
                                                    .map(|i| i.to_string())
                                                    .collect::<Vec<_>>()
                                                    .join(", ");
                                                ui.monospace(ips);
                                                ui.end_row();
                                            }
                                        });
                                    ui.add_space(10.0);
                                    ui.strong(if is_de {
                                        "ARP-Nachbartabelle:"
                                    } else {
                                        "ARP Neighbor Table:"
                                    });
                                    egui::Grid::new("net_arp_grid")
                                        .striped(true)
                                        .spacing([12.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.strong("IP");
                                            ui.strong("MAC");
                                            ui.strong(if is_de {
                                                "Hersteller (OUI)"
                                            } else {
                                                "Vendor (OUI)"
                                            });
                                            ui.strong("Interface");
                                            ui.end_row();
                                            for a in &self.net_scanner.arp_entries {
                                                ui.monospace(a.ip.to_string());
                                                ui.monospace(a.mac.to_string());
                                                ui.label(a.vendor.as_deref().unwrap_or("-"));
                                                ui.label(&a.interface);
                                                ui.end_row();
                                            }
                                        });
                                }
                                DiagTool::Mdns => {
                                    egui::Grid::new("net_mdns_grid")
                                        .striped(true)
                                        .spacing([12.0, 4.0])
                                        .show(ui, |ui| {
                                            ui.strong("Instanz");
                                            ui.strong("Dienst-Typ");
                                            ui.strong("Hostname");
                                            ui.strong("Port");
                                            ui.strong("IP-Adressen");
                                            ui.end_row();
                                            for s in &self.net_scanner.mdns_services {
                                                ui.label(&s.full_name);
                                                ui.monospace(&s.service_type);
                                                ui.label(&s.hostname);
                                                ui.monospace(s.port.to_string());
                                                let ips = s
                                                    .addresses
                                                    .iter()
                                                    .map(|a| a.to_string())
                                                    .collect::<Vec<_>>()
                                                    .join(", ");
                                                ui.monospace(ips);
                                                ui.end_row();
                                            }
                                        });
                                }
                            });
                    }
                }
            });

            self.show_net_scanner = open;

            if self.net_scanner.show_export_dialog {
                let mut dlg_open = self.net_scanner.show_export_dialog;
                let mut do_save = false;
                let mut do_cancel = false;

                egui::Window::new(if is_de {
                    "💾 CSV-Datei speichern unter..."
                } else {
                    "💾 Save CSV File As..."
                })
                .open(&mut dlg_open)
                .collapsible(false)
                .resizable(true)
                .default_width(560.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(if is_de {
                        "Zielpfad für den CSV-Export (UTF-8 mit BOM):"
                    } else {
                        "Target path for CSV export (UTF-8 with BOM):"
                    });

                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.net_scanner.export_path_input)
                                .desired_width(420.0)
                                .font(egui::TextStyle::Monospace),
                        );
                        if resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            do_save = true;
                        }
                        if ui
                            .button(if is_de {
                                "📂 Durchsuchen..."
                            } else {
                                "📂 Browse..."
                            })
                            .clicked()
                        {
                            if let Some(picked) = crate::net_scanner::NetScannerState::pick_save_csv_path(
                                &self.net_scanner.export_path_input,
                            ) {
                                self.net_scanner.export_path_input = picked;
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label(if is_de { "Schnellwahl:" } else { "Quick folder:" });
                        if ui
                            .small_button(if is_de {
                                "⬅ Linkes Dateifenster"
                            } else {
                                "⬅ Left Pane"
                            })
                            .clicked()
                        {
                            let fname = Path::new(&self.net_scanner.export_path_input)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| self.net_scanner.default_csv_filename());
                            self.net_scanner.export_path_input = self
                                .left_pane
                                .current_path
                                .join(fname)
                                .to_string_lossy()
                                .to_string();
                        }
                        if ui
                            .small_button(if is_de {
                                "➡ Rechtes Dateifenster"
                            } else {
                                "➡ Right Pane"
                            })
                            .clicked()
                        {
                            let fname = Path::new(&self.net_scanner.export_path_input)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| self.net_scanner.default_csv_filename());
                            self.net_scanner.export_path_input = self
                                .right_pane
                                .current_path
                                .join(fname)
                                .to_string_lossy()
                                .to_string();
                        }
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                egui::RichText::new(if is_de {
                                    "💾 Speichern"
                                } else {
                                    "💾 Save"
                                })
                                .strong(),
                            )
                            .clicked()
                        {
                            do_save = true;
                        }
                        if ui
                            .button(if is_de { "Abbrechen" } else { "Cancel" })
                            .clicked()
                        {
                            do_cancel = true;
                        }
                    });
                });

                if do_save {
                    let target = PathBuf::from(self.net_scanner.export_path_input.trim());
                    match self.net_scanner.export_csv_to_path(&target) {
                        Ok(saved) => {
                            self.net_scanner.error_message = None;
                            self.net_scanner.status_text =
                                format!("💾 CSV gespeichert: {}", saved.display());
                            self.left_pane.refresh();
                            self.right_pane.refresh();
                            dlg_open = false;
                        }
                        Err(e) => {
                            self.net_scanner.error_message = Some(e);
                        }
                    }
                }
                if do_cancel {
                    dlg_open = false;
                }
                self.net_scanner.show_export_dialog = dlg_open;
            }

            if let Some(ip) = action_inspect_ip {
                self.net_scanner.target_host = ip;
                self.net_scanner.active_tab = crate::net_scanner::NetScannerTab::Inspect;
                self.net_scanner
                    .start_inspect(self.net_scan_tx.clone(), ctx.clone(), false);
            }
            if let Some(ip) = action_ssh_ip {
                self.ssh_profile_draft.host = ip;
                self.show_ssh_terminal = true;
            }
            if let Some(ip) = action_rdp_ip {
                self.rdp_profile_draft.host = ip;
                self.show_rdp_window = true;
            }
        }

        // Password & PIN Generator Window (Tools -> Passwort- & PIN-Generator)
        if self.show_password_generator {
            let mut open = self.show_password_generator;
            egui::Window::new(if is_de {
                "🔑 Passwort- & PIN-Generator"
            } else {
                "🔑 Password & PIN Generator"
            })
            .open(&mut open)
            .default_width(540.0)
            .resizable(true)
            .show(ctx, |ui| {
                use crate::tools::{PasswordGenMode, PasswordGeneratorState, WordCase};

                ui.horizontal(|ui| {
                    let prev_mode = self.password_generator.mode;
                    ui.selectable_value(
                        &mut self.password_generator.mode,
                        PasswordGenMode::Words,
                        if is_de {
                            "🗣 Wort-Passwort (z.B. ARCTIC-WOLF-XDFG)"
                        } else {
                            "🗣 Word Password (e.g. ARCTIC-WOLF-XDFG)"
                        },
                    );
                    ui.selectable_value(
                        &mut self.password_generator.mode,
                        PasswordGenMode::Password,
                        if is_de {
                            "🔐 Zufallspasswort (Normal)"
                        } else {
                            "🔐 Random Password (Normal)"
                        },
                    );
                    ui.selectable_value(
                        &mut self.password_generator.mode,
                        PasswordGenMode::Pin,
                        if is_de { "🔢 Zufalls-PIN" } else { "🔢 Random PIN" },
                    );
                    if self.password_generator.mode != prev_mode {
                        self.password_generator.generate();
                    }
                });

                ui.separator();

                match self.password_generator.mode {
                    PasswordGenMode::Words => {
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            ui.label(if is_de {
                                "Anzahl Wörter:"
                            } else {
                                "Word Count:"
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(
                                        &mut self.password_generator.word_count,
                                        1..=6,
                                    )
                                    .suffix(if is_de { " Wörter" } else { " words" }),
                                )
                                .changed();

                            ui.separator();
                            ui.label(if is_de {
                                "Zufalls-Endblock (z.B. -XDFG):"
                            } else {
                                "Random Suffix (e.g. -XDFG):"
                            });
                            changed |= ui
                                .add(
                                    egui::Slider::new(
                                        &mut self.password_generator.word_suffix_len,
                                        0..=8,
                                    )
                                    .suffix(if is_de { " Zeichen" } else { " chars" }),
                                )
                                .changed();
                        });

                        ui.horizontal_wrapped(|ui| {
                            ui.label(if is_de { "Trennzeichen:" } else { "Separator:" });
                            for (sep, lbl) in [
                                ("-", "- (Bindestrich)"),
                                ("_", "_ (Unterstrich)"),
                                (".", ". (Punkt)"),
                                (":", ": (Doppelpunkt)"),
                                (" ", "Leerzeichen"),
                            ] {
                                if ui
                                    .selectable_label(
                                        self.password_generator.word_separator == sep,
                                        lbl,
                                    )
                                    .clicked()
                                {
                                    self.password_generator.word_separator = sep.to_string();
                                    changed = true;
                                }
                            }
                        });

                        ui.horizontal_wrapped(|ui| {
                            ui.label(if is_de { "Schreibweise:" } else { "Case:" });
                            for (wc, lbl) in [
                                (WordCase::Upper, "GROSS (ARCTIC-WOLF)"),
                                (WordCase::Title, "Titel (Arctic-Wolf)"),
                                (WordCase::Lower, "klein (arctic-wolf)"),
                            ] {
                                if ui
                                    .selectable_label(self.password_generator.word_case == wc, lbl)
                                    .clicked()
                                {
                                    self.password_generator.word_case = wc;
                                    changed = true;
                                }
                            }
                            if self.password_generator.word_suffix_len > 0 {
                                changed |= ui
                                    .checkbox(
                                        &mut self.password_generator.word_suffix_digits,
                                        if is_de {
                                            "Ziffern im Endblock"
                                        } else {
                                            "Digits in suffix"
                                        },
                                    )
                                    .changed();
                            }
                        });

                        if changed {
                            self.password_generator.generate();
                        }
                    }
                    PasswordGenMode::Password => {
                        ui.horizontal(|ui| {
                            ui.label(if is_de {
                                "Passwortlänge:"
                            } else {
                                "Password Length:"
                            });
                            if ui
                                .add(
                                    egui::Slider::new(&mut self.password_generator.length, 6..=64)
                                        .suffix(if is_de { " Zeichen" } else { " chars" }),
                                )
                                .changed()
                            {
                                self.password_generator.generate();
                            }
                        });

                        ui.horizontal_wrapped(|ui| {
                            let mut changed = false;
                            changed |= ui
                                .checkbox(
                                    &mut self.password_generator.uppercase,
                                    if is_de { "A-Z Groß" } else { "A-Z Upper" },
                                )
                                .changed();
                            changed |= ui
                                .checkbox(
                                    &mut self.password_generator.lowercase,
                                    if is_de { "a-z Klein" } else { "a-z Lower" },
                                )
                                .changed();
                            changed |= ui
                                .checkbox(
                                    &mut self.password_generator.numbers,
                                    if is_de { "0-9 Zahlen (-n)" } else { "0-9 Numbers (-n)" },
                                )
                                .changed();
                            changed |= ui
                                .checkbox(
                                    &mut self.password_generator.symbols,
                                    if is_de {
                                        "!@# Sonderzeichen (-s)"
                                    } else {
                                        "!@# Symbols (-s)"
                                    },
                                )
                                .changed();
                            changed |= ui
                                .checkbox(
                                    &mut self.password_generator.exclude_ambiguous,
                                    if is_de {
                                        "Ohne 0/O/1/l/I"
                                    } else {
                                        "No 0/O/1/l/I"
                                    },
                                )
                                .changed();
                            if changed {
                                if !self.password_generator.uppercase
                                    && !self.password_generator.lowercase
                                    && !self.password_generator.numbers
                                    && !self.password_generator.symbols
                                {
                                    self.password_generator.lowercase = true;
                                }
                                self.password_generator.generate();
                            }
                        });
                    }
                    PasswordGenMode::Pin => {
                        ui.horizontal(|ui| {
                            ui.label(if is_de { "PIN-Länge:" } else { "PIN Length:" });
                            if ui
                                .add(
                                    egui::Slider::new(
                                        &mut self.password_generator.pin_length,
                                        4..=16,
                                    )
                                    .suffix(if is_de { " Ziffern" } else { " digits" }),
                                )
                                .changed()
                            {
                                self.password_generator.generate();
                            }
                        });
                    }
                }

                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password_generator.current)
                            .font(egui::FontId::monospace(17.0))
                            .desired_width(340.0),
                    );
                    if ui
                        .button(
                            egui::RichText::new(if is_de {
                                "🎲 Neu generieren"
                            } else {
                                "🎲 Generate"
                            })
                            .strong(),
                        )
                        .clicked()
                    {
                        self.password_generator.generate();
                    }
                    if ui
                        .button(if is_de {
                            "📋 Kopieren"
                        } else {
                            "📋 Copy"
                        })
                        .clicked()
                    {
                        ctx.copy_text(self.password_generator.current.clone());
                        self.password_generator.copied_notice = true;
                    }
                });

                if self.password_generator.copied_notice {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        if is_de {
                            "✔ In die Zwischenablage kopiert!"
                        } else {
                            "✔ Copied to clipboard!"
                        },
                    );
                }

                ui.separator();

                // Security & Crack-Time Analysis (zxcvbn dictionary + pattern engine)
                let analysis =
                    PasswordGeneratorState::analyze(&self.password_generator.current, is_de);
                let (score_label, score_color) = match analysis.score {
                    0 => (
                        if is_de { "0/4 — Sehr schwach" } else { "0/4 — Very Weak" },
                        egui::Color32::from_rgb(230, 70, 70),
                    ),
                    1 => (
                        if is_de { "1/4 — Schwach" } else { "1/4 — Weak" },
                        egui::Color32::from_rgb(240, 140, 60),
                    ),
                    2 => (
                        if is_de { "2/4 — Mittel" } else { "2/4 — Fair" },
                        egui::Color32::from_rgb(235, 200, 60),
                    ),
                    3 => (
                        if is_de { "3/4 — Stark" } else { "3/4 — Strong" },
                        egui::Color32::from_rgb(120, 210, 95),
                    ),
                    _ => (
                        if is_de { "4/4 — Sehr stark" } else { "4/4 — Very Strong" },
                        egui::Color32::from_rgb(60, 220, 120),
                    ),
                };

                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(if is_de {
                            "🛡 zxcvbn Sicherheitsanalyse:"
                        } else {
                            "🛡 zxcvbn Security Analysis:"
                        });
                        ui.colored_label(score_color, egui::RichText::new(score_label).strong());
                        ui.label(format!(
                            "({:.1} Bits | ~{} Guesses)",
                            analysis.entropy_bits, analysis.guesses
                        ));
                    });

                    ui.add(
                        egui::ProgressBar::new((analysis.score as f32 / 4.0).clamp(0.08, 1.0))
                            .fill(score_color),
                    );

                    if let Some(warn) = &analysis.warning {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 160, 80),
                            format!("⚠ {warn}"),
                        );
                    }
                    for sug in &analysis.suggestions {
                        ui.label(
                            egui::RichText::new(format!("💡 {sug}"))
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    }

                    ui.add_space(4.0);
                    ui.strong(if is_de {
                        "Geschätzte Knackdauer (Crack-Time Estimations):"
                    } else {
                        "Crack-Time Estimations:"
                    });
                    egui::Grid::new("pw_crack_grid")
                        .striped(true)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            ui.label(if is_de {
                                "Online gedrosselt (100 / Std.):"
                            } else {
                                "Online throttled (100 / h):"
                            });
                            ui.monospace(&analysis.crack_100_per_hour);
                            ui.end_row();

                            ui.label(if is_de {
                                "Online ungedrosselt (10 / Sek.):"
                            } else {
                                "Online unthrottled (10 / s):"
                            });
                            ui.monospace(&analysis.crack_10_per_sec);
                            ui.end_row();

                            ui.label(if is_de {
                                "Offline langsamer Hash — bcrypt/Argon2 (10⁴ / Sek.):"
                            } else {
                                "Offline slow hash — bcrypt/Argon2 (10⁴ / s):"
                            });
                            ui.monospace(&analysis.crack_1e4_per_sec);
                            ui.end_row();

                            ui.label(if is_de {
                                "Offline schneller GPU-Hash — MD5/SHA (10¹⁰ / Sek.):"
                            } else {
                                "Offline fast GPU hash — MD5/SHA (10¹⁰ / s):"
                            });
                            ui.monospace(&analysis.crack_1e10_per_sec);
                            ui.end_row();
                        });
                });

                if !self.password_generator.history.is_empty() {
                    ui.add_space(6.0);
                    ui.collapsing(
                        if is_de {
                            "🕘 Zuletzt generiert"
                        } else {
                            "🕘 Recently Generated"
                        },
                        |ui| {
                            let mut pick_hist: Option<String> = None;
                            for h in &self.password_generator.history {
                                ui.horizontal(|ui| {
                                    ui.monospace(h);
                                    if ui
                                        .small_button(if is_de { "📋" } else { "📋" })
                                        .on_hover_text(if is_de {
                                            "Kopieren"
                                        } else {
                                            "Copy"
                                        })
                                        .clicked()
                                    {
                                        ctx.copy_text(h.clone());
                                        self.password_generator.copied_notice = true;
                                    }
                                    if ui
                                        .small_button(if is_de {
                                            "Übernehmen"
                                        } else {
                                            "Use"
                                        })
                                        .clicked()
                                    {
                                        pick_hist = Some(h.clone());
                                    }
                                });
                            }
                            if let Some(p) = pick_hist {
                                self.password_generator.current = p;
                            }
                        },
                    );
                }
            });
            self.show_password_generator = open;
        }

        // Process Editor Messages
        while let Ok(msg) = self.editor_rx.try_recv() {
            match msg {
                messages::EditorMessage::FileLoaded(_path, content, encoding) => {
                    self.editor_line_ending = if content.contains("\r\n") {
                        "Windows (CRLF)".to_string()
                    } else {
                        "Unix (LF)".to_string()
                    };
                    self.editor_content = content;
                    self.editor_encoding = encoding;
                    self.editor_dirty = false;
                    self.editor_image_data = None;
                    self.editor_image_texture = None;
                }
                messages::EditorMessage::ImageLoaded(_path, size, rgba) => {
                    self.editor_encoding = format!("Image ({} x {} px)", size[0], size[1]);
                    self.editor_image_data = Some((size, rgba));
                    self.editor_image_texture = None;
                    self.editor_content.clear();
                    self.editor_dirty = false;
                }
                messages::EditorMessage::Error(err) => {
                    self.editor_content = format!("Error loading file:\n{}", err);
                    self.editor_image_data = None;
                    self.editor_image_texture = None;
                }
            }
        }
        
        if self.show_editor {
            let mut close_editor = false;
            let mut trigger_save = false;
            let is_image_mode = self.editor_image_data.is_some() || self.editor_image_texture.is_some();
            let file_only_name = std::path::Path::new(&self.editor_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.editor_path.clone());
            let dirty_prefix = if self.editor_dirty && !self.editor_is_view_only { "*" } else { "" };
            let app_label = if is_image_mode {
                "Bildanzeige (F3)"
            } else if self.editor_is_view_only {
                "Viewer (F3)"
            } else {
                "Editor"
            };
            let window_title = format!("{}{} - {}", dirty_prefix, file_only_name, app_label);
            
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("editor_viewport"),
                egui::ViewportBuilder::default()
                    .with_title(window_title)
                    .with_inner_size([860.0, 640.0]),
                |vp_ctx, _class| {
                    if let Some((size, rgba)) = self.editor_image_data.take() {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                        self.editor_image_texture = Some(vp_ctx.load_texture(
                            "viewer_image",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }

                    // Keyboard shortcuts inside Editor/Viewer (Windows 10 Notepad style)
                    if !is_image_mode {
                        if !self.editor_is_view_only && vp_ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::S)) {
                            if vp_ctx.input(|i| i.modifiers.shift) {
                                self.editor_save_as_path = self.editor_path.clone();
                                self.editor_show_save_as = true;
                            } else {
                                trigger_save = true;
                            }
                        }
                        if vp_ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::F)) {
                            self.editor_show_find_bar = !self.editor_show_find_bar;
                        }
                        if !self.editor_is_view_only && vp_ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::H)) {
                            self.editor_show_find_bar = true;
                            self.editor_show_replace_bar = !self.editor_show_replace_bar;
                        }
                        if !self.editor_is_view_only && vp_ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                            let now_str = Local::now().format("%H:%M %d.%m.%Y").to_string();
                            self.editor_content.push_str(&now_str);
                            self.editor_dirty = true;
                        }
                        if vp_ctx.input(|i| i.modifiers.ctrl && (i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals))) {
                            self.editor_font_size = (self.editor_font_size + 2.0).min(36.0);
                        }
                        if vp_ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Minus)) {
                            self.editor_font_size = (self.editor_font_size - 2.0).max(8.0);
                        }
                        if vp_ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Num0)) {
                            self.editor_font_size = 14.0;
                        }
                    }

                    // Top Menu Bar (Windows 10 Notepad style)
                    egui::TopBottomPanel::top("editor_menubar").show(vp_ctx, |ui| {
                        if let Some(tex) = &self.editor_image_texture {
                            let tex_size = tex.size();
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.editor_image_fit, "An Fenster anpassen");
                                ui.add_enabled_ui(!self.editor_image_fit, |ui| {
                                    if ui.button("-").clicked() {
                                        self.editor_image_zoom = (self.editor_image_zoom / 1.25).max(0.05);
                                    }
                                    ui.label(format!("{:.0}%", self.editor_image_zoom * 100.0));
                                    if ui.button("+").clicked() {
                                        self.editor_image_zoom = (self.editor_image_zoom * 1.25).min(16.0);
                                    }
                                    if ui.button("100%").clicked() {
                                        self.editor_image_zoom = 1.0;
                                    }
                                });
                                ui.separator();
                                ui.label(format!("{} x {} px", tex_size[0], tex_size[1]));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("Schließen (Esc)").clicked() {
                                        close_editor = true;
                                    }
                                });
                            });
                        } else {
                            egui::menu::bar(ui, |ui| {
                                ui.menu_button("Datei", |ui| {
                                    if !self.editor_is_view_only {
                                        if ui.button("Neu").clicked() {
                                            self.editor_content.clear();
                                            self.editor_dirty = true;
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        if ui.button("Speichern               Strg+S").clicked() {
                                            trigger_save = true;
                                            ui.close_menu();
                                        }
                                        if ui.button("Speichern unter...  Strg+Umschalt+S").clicked() {
                                            self.editor_save_as_path = self.editor_path.clone();
                                            self.editor_show_save_as = true;
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                    }
                                    if ui.button("Beenden").clicked() {
                                        close_editor = true;
                                        ui.close_menu();
                                    }
                                });

                                ui.menu_button("Bearbeiten", |ui| {
                                    if ui.button("Suchen...                Strg+F").clicked() {
                                        self.editor_show_find_bar = true;
                                        ui.close_menu();
                                    }
                                    if !self.editor_is_view_only {
                                        if ui.button("Ersetzen...              Strg+H").clicked() {
                                            self.editor_show_find_bar = true;
                                            self.editor_show_replace_bar = true;
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        if ui.button("Uhrzeit/Datum            F5").clicked() {
                                            let now_str = Local::now().format("%H:%M %d.%m.%Y").to_string();
                                            self.editor_content.push_str(&now_str);
                                            self.editor_dirty = true;
                                            ui.close_menu();
                                        }
                                    }
                                    ui.separator();
                                    if ui.button("Alles kopieren").clicked() {
                                        vp_ctx.copy_text(self.editor_content.clone());
                                        ui.close_menu();
                                    }
                                });

                                ui.menu_button("Format", |ui| {
                                    if ui.checkbox(&mut self.editor_wrap, "Zeilenumbruch").clicked() {
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    ui.label("Schriftgröße:");
                                    ui.horizontal(|ui| {
                                        if ui.button(" - ").clicked() {
                                            self.editor_font_size = (self.editor_font_size - 2.0).max(8.0);
                                        }
                                        ui.label(format!("{:.0} pt", self.editor_font_size));
                                        if ui.button(" + ").clicked() {
                                            self.editor_font_size = (self.editor_font_size + 2.0).min(36.0);
                                        }
                                    });
                                });

                                ui.menu_button("Ansicht", |ui| {
                                    ui.menu_button("Zoom", |ui| {
                                        if ui.button("Vergrößern                    Strg+Plus").clicked() {
                                            self.editor_font_size = (self.editor_font_size + 2.0).min(36.0);
                                            ui.close_menu();
                                        }
                                        if ui.button("Verkleinern                   Strg+Minus").clicked() {
                                            self.editor_font_size = (self.editor_font_size - 2.0).max(8.0);
                                            ui.close_menu();
                                        }
                                        if ui.button("Standardzoom wiederherstellen Strg+0").clicked() {
                                            self.editor_font_size = 14.0;
                                            ui.close_menu();
                                        }
                                    });
                                    ui.checkbox(&mut self.editor_show_statusbar, "Statusleiste");
                                });

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if !self.editor_is_view_only && self.editor_dirty {
                                        if ui.button("💾 Speichern (Strg+S)").clicked() {
                                            trigger_save = true;
                                        }
                                    }
                                });
                            });

                            // Collapsible Find & Replace bar
                            if self.editor_show_find_bar {
                                ui.separator();
                                ui.horizontal(|ui| {
                                    ui.label("Suchen:");
                                    ui.add(egui::TextEdit::singleline(&mut self.editor_search_query).desired_width(200.0));
                                    if !self.editor_search_query.is_empty() {
                                        let count = self.editor_content.to_lowercase().matches(&self.editor_search_query.to_lowercase()).count();
                                        ui.label(format!("({} Treffer)", count));
                                    }
                                    if !self.editor_is_view_only {
                                        if ui.selectable_label(self.editor_show_replace_bar, "Ersetzen ▾").clicked() {
                                            self.editor_show_replace_bar = !self.editor_show_replace_bar;
                                        }
                                    }
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.button("✖").clicked() {
                                            self.editor_show_find_bar = false;
                                            self.editor_show_replace_bar = false;
                                        }
                                    });
                                });
                                if self.editor_show_replace_bar && !self.editor_is_view_only {
                                    ui.horizontal(|ui| {
                                        ui.label("Ersetzen durch:");
                                        ui.add(egui::TextEdit::singleline(&mut self.editor_replace_query).desired_width(200.0));
                                        if ui.button("Ersetzen (1x)").clicked() && !self.editor_search_query.is_empty() {
                                            if self.editor_content.contains(&self.editor_search_query) {
                                                self.editor_content = self.editor_content.replacen(&self.editor_search_query, &self.editor_replace_query, 1);
                                                self.editor_dirty = true;
                                            }
                                        }
                                        if ui.button("Alle ersetzen").clicked() && !self.editor_search_query.is_empty() {
                                            if self.editor_content.contains(&self.editor_search_query) {
                                                self.editor_content = self.editor_content.replace(&self.editor_search_query, &self.editor_replace_query);
                                                self.editor_dirty = true;
                                            }
                                        }
                                    });
                                }
                            }
                        }
                    });

                    // Save As Modal inside Editor Viewport
                    if self.editor_show_save_as {
                        egui::Window::new("Speichern unter")
                            .collapsible(false)
                            .resizable(false)
                            .show(vp_ctx, |ui| {
                                ui.label("Zielpfad:");
                                let resp = ui.add(egui::TextEdit::singleline(&mut self.editor_save_as_path).desired_width(420.0));
                                resp.request_focus();
                                ui.horizontal(|ui| {
                                    if ui.button("Speichern").clicked() || vp_ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                                        self.editor_path = self.editor_save_as_path.clone();
                                        trigger_save = true;
                                        self.editor_show_save_as = false;
                                    }
                                    if ui.button("Abbrechen").clicked() || vp_ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                                        self.editor_show_save_as = false;
                                    }
                                });
                            });
                    }

                    if trigger_save && !self.editor_is_view_only {
                        let path = self.editor_path.clone();
                        let content = self.editor_content.clone();
                        if let Some(handle) = self.editor_vfs_handle.as_ref().cloned() {
                            let tx_left = self.left_pane.tx.clone();
                            let tx_right = self.right_pane.tx.clone();
                            let vp_ctx_clone = vp_ctx.clone();
                            self.editor_dirty = false;
                            tokio::spawn(async move {
                                let _ = crate::file_ops::write_file(&handle, &path, content.into_bytes()).await;
                                let _ = tx_left.send(messages::PaneMessage::Refresh);
                                let _ = tx_right.send(messages::PaneMessage::Refresh);
                                vp_ctx_clone.request_repaint();
                            });
                        }
                    }

                    // Compute cursor line/col from last known cursor position for status bar
                    let mut cursor_line = 1;
                    let mut cursor_col = 1;
                    if let Some(cursor_pos) = self.editor_last_cursor {
                        let text = &self.editor_content;
                        let safe_pos = cursor_pos.min(text.len());
                        if text.is_char_boundary(safe_pos) {
                            cursor_line = text[..safe_pos].chars().filter(|&c| c == '\n').count() + 1;
                            let last_newline = text[..safe_pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                            cursor_col = text[last_newline..safe_pos].chars().count() + 1;
                        }
                    }

                    // Bottom Status Bar (Windows 10 Notepad style)
                    if self.editor_show_statusbar {
                        egui::TopBottomPanel::bottom("editor_statusbar").show(vp_ctx, |ui| {
                            ui.horizontal(|ui| {
                                if self.editor_image_texture.is_some() {
                                    ui.label(&self.editor_path);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(&self.editor_encoding);
                                    });
                                } else {
                                    ui.label(format!("Zeile {}, Spalte {}", cursor_line, cursor_col));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(&self.editor_encoding);
                                        ui.separator();
                                        ui.label(&self.editor_line_ending);
                                        ui.separator();
                                        let zoom_pct = ((self.editor_font_size / 14.0) * 100.0).round() as i32;
                                        ui.label(format!("{}%", zoom_pct));
                                    });
                                }
                            });
                        });
                    }

                    // Main Central Area (Clean Paper Canvas for Editor/Viewer)
                    egui::CentralPanel::default()
                        .frame(egui::Frame::none().fill(vp_ctx.style().visuals.extreme_bg_color).inner_margin(4.0))
                        .show(vp_ctx, |ui| {
                            if let Some(tex) = &self.editor_image_texture {
                                let avail = egui::vec2(ui.available_width(), ui.available_height().max(50.0));
                                let orig = tex.size_vec2();
                                let display_size = if self.editor_image_fit {
                                    let scale_x = avail.x / orig.x.max(1.0);
                                    let scale_y = avail.y / orig.y.max(1.0);
                                    let scale = scale_x.min(scale_y).min(1.0);
                                    orig * scale
                                } else {
                                    orig * self.editor_image_zoom
                                };

                                egui::ScrollArea::both()
                                    .max_height(avail.y)
                                    .show(ui, |ui| {
                                        ui.centered_and_justified(|ui| {
                                            ui.image((tex.id(), display_size));
                                        });
                                    });
                            } else {
                                let scroll = if self.editor_wrap {
                                    egui::ScrollArea::vertical()
                                } else {
                                    egui::ScrollArea::both()
                                };
                                
                                scroll.auto_shrink([false, false]).show(ui, |ui| {
                                    let prev_len = self.editor_content.len();
                                    let mut ro_buffer = ReadOnlyTextBuffer(&self.editor_content);
                                    let mut_buffer: &mut dyn egui::TextBuffer = if self.editor_is_view_only {
                                        &mut ro_buffer
                                    } else {
                                        &mut self.editor_content
                                    };
                                    
                                    let mut text_edit = egui::TextEdit::multiline(mut_buffer)
                                        .frame(false)
                                        .font(egui::FontId::monospace(self.editor_font_size))
                                        .desired_rows(35)
                                        .lock_focus(true);
                                        
                                    if !self.editor_wrap {
                                        text_edit = text_edit.desired_width(f32::INFINITY);
                                    } else {
                                        text_edit = text_edit.desired_width(ui.available_width());
                                    }
                                    
                                    let output = text_edit.show(ui);
                                    if !self.editor_is_view_only && (output.response.changed() || self.editor_content.len() != prev_len) {
                                        self.editor_dirty = true;
                                    }
                                    
                                    if let Some(cursor_range) = output.cursor_range {
                                        let cursor_char_idx = cursor_range.primary.ccursor.index;
                                        let byte_idx = self.editor_content
                                            .char_indices()
                                            .nth(cursor_char_idx)
                                            .map(|(i, _)| i)
                                            .unwrap_or(self.editor_content.len());
                                        self.editor_last_cursor = Some(byte_idx);
                                    }
                                });
                            }
                        });
                    
                    if vp_ctx.input(|i| i.viewport().close_requested()) {
                        close_editor = true;
                    }
                    if self.editor_is_view_only && !self.editor_show_find_bar && vp_ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close_editor = true;
                    }
                }
            );
            if close_editor {
                self.show_editor = false;
                self.editor_image_texture = None;
                self.editor_image_data = None;
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                self.render_pane(&mut cols[0], true, ctx);
                self.render_pane(&mut cols[1], false, ctx);
            });
        });
    }
}

impl FaroCommander {
    fn render_vault_box(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, is_de: bool) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            let has_vault = ssh_client::vault_exists();
            if self.ssh_vault_unlocked {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(if is_de {
                            "🔓 Tresor entsperrt (AES-256)"
                        } else {
                            "🔓 Vault unlocked (AES-256)"
                        })
                        .color(egui::Color32::from_rgb(78, 201, 176))
                        .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(if is_de { "🔒 Sperren" } else { "🔒 Lock" }).clicked() {
                            self.ssh_vault_unlocked = false;
                            self.ssh_vault_password.clear();
                            self.ssh_profiles.clear();
                            self.ssh_selected_profile = None;
                            self.rdp_profiles.clear();
                            self.rdp_selected_profile = None;
                        }
                    });
                });
            } else if !has_vault {
                ui.label(
                    egui::RichText::new(if is_de {
                        "🔐 Passwort-Tresor einrichten (min. 8 Zeichen, Groß-/Kleinschreibung)"
                    } else {
                        "🔐 Setup Password Vault (min. 8 chars, upper & lowercase)"
                    })
                    .strong(),
                );
                egui::Grid::new("vault_setup_grid").num_columns(2).spacing([4.0, 4.0]).show(ui, |ui| {
                    ui.label(if is_de { "Passwort:" } else { "Password:" });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ssh_vault_password)
                            .password(true)
                            .desired_width(160.0),
                    );
                    ui.end_row();
                    ui.label(if is_de { "Bestätigen:" } else { "Confirm:" });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ssh_vault_password_confirm)
                            .password(true)
                            .desired_width(160.0),
                    );
                    ui.end_row();
                });
                if let Some(err) = &self.ssh_vault_error {
                    ui.colored_label(egui::Color32::from_rgb(240, 90, 90), err);
                }
                if ui
                    .button(if is_de {
                        "🛡 Tresor erstellen"
                    } else {
                        "🛡 Create Vault"
                    })
                    .clicked()
                {
                    if let Err(msg) = ssh_client::validate_master_password(&self.ssh_vault_password, is_de) {
                        self.ssh_vault_error = Some(msg.to_string());
                    } else if self.ssh_vault_password != self.ssh_vault_password_confirm {
                        self.ssh_vault_error = Some(if is_de {
                            "Passwörter stimmen nicht überein.".into()
                        } else {
                            "Passwords do not match.".into()
                        });
                    } else {
                        match ssh_client::save_vault(&self.ssh_vault_password, &self.ssh_profiles) {
                            Ok(()) => {
                                let _ = rdp_client::save_rdp_vault(&self.ssh_vault_password, &self.rdp_profiles);
                                self.ssh_vault_unlocked = true;
                                self.ssh_vault_password_confirm.clear();
                                self.ssh_vault_error = None;
                            }
                            Err(e) => self.ssh_vault_error = Some(e),
                        }
                    }
                }
            } else {
                ui.label(
                    egui::RichText::new(if is_de {
                        "🔒 Tresor entsperren"
                    } else {
                        "🔒 Unlock Password Vault"
                    })
                    .strong(),
                );
                let mut try_unlock = false;
                ui.horizontal(|ui| {
                    let res = ui.add(
                        egui::TextEdit::singleline(&mut self.ssh_vault_password)
                            .password(true)
                            .hint_text(if is_de { "Master-Passwort..." } else { "Master password..." })
                            .desired_width(145.0),
                    );
                    if res.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        try_unlock = true;
                    }
                    if ui.button(if is_de { "🔓 Entsperren" } else { "🔓 Unlock" }).clicked() {
                        try_unlock = true;
                    }
                    if ui
                        .small_button("🗑")
                        .on_hover_text(if is_de {
                            "Tresor zurücksetzen (löscht gespeicherte Profile)"
                        } else {
                            "Reset vault (deletes saved profiles)"
                        })
                        .clicked()
                    {
                        ssh_client::reset_vault();
                        self.ssh_vault_error = None;
                    }
                });
                if let Some(err) = &self.ssh_vault_error {
                    ui.colored_label(egui::Color32::from_rgb(240, 90, 90), err);
                }
                if try_unlock {
                    let unlock_res = if std::path::Path::new("ssh_vault.json").exists() {
                        ssh_client::unlock_vault(&self.ssh_vault_password)
                            .map(|ssh_p| (ssh_p, rdp_client::unlock_rdp_vault(&self.ssh_vault_password).unwrap_or_default()))
                    } else {
                        rdp_client::unlock_rdp_vault(&self.ssh_vault_password)
                            .map(|rdp_p| (Vec::new(), rdp_p))
                    };
                    match unlock_res {
                        Ok((profs, rdp_profs)) => {
                            self.ssh_vault_unlocked = true;
                            self.ssh_profiles = profs;
                            self.ssh_selected_profile = if self.ssh_profiles.is_empty() { None } else { Some(0) };
                            if let Some(first) = self.ssh_profiles.first() {
                                self.ssh_profile_draft = first.clone();
                            }
                            self.rdp_profiles = rdp_profs;
                            self.rdp_selected_profile = if self.rdp_profiles.is_empty() { None } else { Some(0) };
                            if let Some(first_rdp) = self.rdp_profiles.first() {
                                self.rdp_profile_draft = first_rdp.clone();
                            }
                            self.ssh_vault_error = None;
                        }
                        Err(e) => self.ssh_vault_error = Some(e),
                    }
                }
            }
        });
    }

    fn action_view(&mut self, ctx: &egui::Context) {
        self.open_editor(true, ctx);
    }

    fn action_edit(&mut self, ctx: &egui::Context) {
        self.open_editor(false, ctx);
    }
    
    fn open_editor(&mut self, is_view_only: bool, ctx: &egui::Context) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        if let Some(file) = active_pane.files.get(active_pane.selected_index) {
            if !file.is_dir && file.name != ".." {
                let mut path = active_pane.current_path.join(&file.full_name()).to_string_lossy().to_string();
                if active_pane.vfs_type != VfsType::Local {
                    path = path.replace("\\", "/");
                }
                let handle = active_pane.get_vfs_handle();
                self.open_editor_for_path(path, handle, is_view_only, ctx);
            }
        }
    }

    fn is_image_extension(path: &str) -> bool {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "tga")
    }

    fn open_editor_for_path(&mut self, path: String, handle: crate::file_ops::VfsHandle, is_view_only: bool, ctx: &egui::Context) {
        let tx = self.editor_tx.clone();
        self.show_editor = true;
        self.editor_path = path.clone();
        self.editor_content = "Loading...".to_string();
        self.editor_is_view_only = is_view_only;
        self.editor_vfs_handle = Some(handle.clone());
        self.editor_image_data = None;
        self.editor_image_texture = None;
        self.editor_image_zoom = 1.0;
        self.editor_dirty = false;
        self.editor_last_cursor = Some(0);
        
        let try_image = is_view_only && Self::is_image_extension(&path);
        let ctx_clone = ctx.clone();
        tokio::spawn(async move {
            match crate::file_ops::read_file(&handle, &path).await {
                Ok(data) => {
                    if try_image {
                        if let Ok(img) = image::load_from_memory(&data) {
                            let rgba = img.to_rgba8();
                            let size = [rgba.width() as usize, rgba.height() as usize];
                            let _ = tx.send(messages::EditorMessage::ImageLoaded(path, size, rgba.into_raw()));
                            ctx_clone.request_repaint();
                            return;
                        }
                    }
                    let mut is_utf8 = true;
                    let text = match String::from_utf8(data.clone()) {
                        Ok(s) => s,
                        Err(_) => {
                            is_utf8 = false;
                            data.into_iter().map(|c| c as char).collect::<String>()
                        }
                    };
                    let encoding = if is_utf8 { "UTF-8".to_string() } else { "ISO-8859-1 (ANSI)".to_string() };
                    let _ = tx.send(messages::EditorMessage::FileLoaded(path, text, encoding));
                }
                Err(e) => {
                    let _ = tx.send(messages::EditorMessage::Error(e));
                }
            }
            ctx_clone.request_repaint();
        });
    }

    fn collect_target_files(pane: &PaneState) -> Vec<(String, String, bool)> {
        let mut list = Vec::new();
        if !pane.selected_files.is_empty() {
            for file in &pane.files {
                let fname = file.full_name();
                if pane.selected_files.contains(&fname) && file.name != ".." {
                    let mut full = pane.current_path.join(&fname).to_string_lossy().to_string();
                    if pane.vfs_type != VfsType::Local {
                        full = full.replace('\\', "/");
                    }
                    list.push((full, fname, file.is_dir));
                }
            }
        } else if let Some(file) = pane.files.get(pane.selected_index) {
            if file.name != ".." {
                let fname = file.full_name();
                let mut full = pane.current_path.join(&fname).to_string_lossy().to_string();
                if pane.vfs_type != VfsType::Local {
                    full = full.replace('\\', "/");
                }
                list.push((full, fname, file.is_dir));
            }
        }
        list
    }
    
    fn open_transfer_dialog(&mut self, is_move: bool) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        let target_pane = if self.active_pane_is_left { &self.right_pane } else { &self.left_pane };
        let targets = Self::collect_target_files(active_pane);
        if !targets.is_empty() {
            if targets.len() == 1 {
                self.transfer_source_path = targets[0].0.clone();
                self.transfer_target_path = target_pane.current_path.join(&targets[0].1).to_string_lossy().to_string();
            } else {
                self.transfer_source_path = format!("{} selected items", targets.len());
                self.transfer_target_path = target_pane.current_path.to_string_lossy().to_string();
            }
            self.transfer_sources = targets;
            self.transfer_is_move = is_move;
            self.show_transfer_dialog = true;
        }
    }

    fn action_copy(&mut self) {
        self.open_transfer_dialog(false);
    }

    fn action_move(&mut self) {
        self.open_transfer_dialog(true);
    }

    fn action_inline_rename(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        if let Some(file) = active_pane.files.get(active_pane.selected_index) {
            if file.name != ".." {
                let fname = file.full_name();
                self.inline_rename_active = true;
                self.inline_rename_orig = fname.clone();
                self.inline_rename_value = fname;
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
        let targets = Self::collect_target_files(active_pane);
        if !targets.is_empty() {
            if targets.len() == 1 {
                self.delete_path = targets[0].0.clone();
            } else {
                self.delete_path = format!("{} selected items", targets.len());
            }
            self.delete_sources = targets.into_iter().map(|(p, _, is_dir)| (p, is_dir)).collect();
            self.show_delete_dialog = true;
        }
    }

    fn action_pack(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        let target_pane = if self.active_pane_is_left { &self.right_pane } else { &self.left_pane };
        if active_pane.vfs_type != VfsType::Local {
            return;
        }
        let targets = Self::collect_target_files(active_pane);
        if !targets.is_empty() {
            let base_name = if targets.len() == 1 {
                std::path::Path::new(&targets[0].1)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            } else {
                active_pane
                    .current_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "archive".to_string())
            };
            let ext = self.pack_format.extension();
            // Avoid defaulting to C:\ root if the opposite pane hasn't been navigated yet, as Windows blocks non-admin writes to C:\
            let dest_base = if target_pane.vfs_type == VfsType::Local
                && target_pane.current_path != std::path::Path::new("C:\\")
            {
                &target_pane.current_path
            } else if active_pane.current_path != std::path::Path::new("C:\\") {
                &active_pane.current_path
            } else {
                &target_pane.current_path
            };
            self.pack_target_path = dest_base
                .join(format!("{}.{}", base_name, ext))
                .to_string_lossy()
                .to_string();
            self.pack_source_desc = if targets.len() == 1 {
                targets[0].1.clone()
            } else {
                format!("{} Elemente", targets.len())
            };
            self.pack_sources = targets;
            self.show_pack_dialog = true;
        }
    }

    fn action_unpack(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        let target_pane = if self.active_pane_is_left { &self.right_pane } else { &self.left_pane };
        let dest_dir = if target_pane.vfs_type == VfsType::Local
            && target_pane.current_path != std::path::Path::new("C:\\")
        {
            target_pane.current_path.to_string_lossy().to_string()
        } else if active_pane.vfs_type == VfsType::Local
            && active_pane.current_path != std::path::Path::new("C:\\")
        {
            active_pane.current_path.to_string_lossy().to_string()
        } else {
            target_pane.current_path.to_string_lossy().to_string()
        };

        if let VfsType::Zip(arch_path) = &active_pane.vfs_type {
            self.unpack_archive_path = arch_path.clone();
            self.unpack_target_dir = dest_dir;
            self.show_unpack_dialog = true;
            return;
        }

        if let Some(file) = active_pane.files.get(active_pane.selected_index) {
            let fname = file.full_name();
            if !file.is_dir && crate::archive_ops::is_archive_filename(&fname) {
                self.unpack_archive_path = active_pane.current_path.join(&fname).to_string_lossy().to_string();
                self.unpack_target_dir = dest_dir;
                self.show_unpack_dialog = true;
            }
        }
    }

    fn action_multi_rename(&mut self) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        if active_pane.vfs_type != VfsType::Local {
            return;
        }
        let mut items = Self::collect_target_files(active_pane);
        if items.is_empty() {
            for f in &active_pane.files {
                if f.name != ".." {
                    let fname = f.full_name();
                    let full = active_pane.current_path.join(&fname).to_string_lossy().to_string();
                    items.push((full, fname, f.is_dir));
                }
            }
        }
        if !items.is_empty() {
            self.multi_rename.items = items;
            self.show_multi_rename = true;
        }
    }

    fn action_properties(&mut self, ctx: &egui::Context) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        if active_pane.vfs_type != VfsType::Local {
            return;
        }
        if let Some(file) = active_pane.files.get(active_pane.selected_index) {
            if file.name == ".." {
                return;
            }
            let full_path = active_pane.current_path.join(file.full_name());
            let full_str = full_path.to_string_lossy().to_string();
            self.properties_state = crate::tools::inspect_path_sync(&full_str);
            self.show_properties_dialog = true;

            let is_dir = self.properties_state.is_dir;
            let tx = self.props_tx.clone();
            let ctx_clone = ctx.clone();
            std::thread::spawn(move || {
                if let Ok(tuple) = crate::tools::compute_hashes_or_dir_stats(&full_str, is_dir) {
                    let _ = tx.send(tuple);
                    ctx_clone.request_repaint();
                }
            });
        }
    }

    fn action_calculate_dir_sizes(&mut self, ctx: &egui::Context) {
        let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
        if active_pane.vfs_type != VfsType::Local {
            return;
        }
        let base_dir = active_pane.current_path.clone();
        let tx = active_pane.tx.clone();
        let dirs: Vec<String> = active_pane
            .files
            .iter()
            .filter(|f| f.is_dir && f.name != "..")
            .map(|f| f.name.clone())
            .collect();
        for dir_name in dirs {
            let dir_path = base_dir.join(&dir_name);
            let tx_clone = tx.clone();
            let ctx_clone = ctx.clone();
            std::thread::spawn(move || {
                let sz = crate::tools::calculate_dir_size(&dir_path);
                let _ = tx_clone.send(messages::PaneMessage::DirSizeCalculated(dir_name, sz));
                ctx_clone.request_repaint();
            });
        }
    }

    fn compare_directories(&mut self) {
        self.sync_entries.clear();
        let l_root = &self.left_pane.current_path;
        let r_root = &self.right_pane.current_path;

        let mut left_map = std::collections::BTreeMap::new();
        let mut right_map = std::collections::BTreeMap::new();

        for entry in walkdir::WalkDir::new(l_root).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                if let Ok(rel) = entry.path().strip_prefix(l_root) {
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    let meta = entry.metadata().ok();
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let mtime = meta.and_then(|m| m.modified().ok());
                    left_map.insert(rel_str, (size, mtime));
                }
            }
        }
        for entry in walkdir::WalkDir::new(r_root).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file() {
                if let Ok(rel) = entry.path().strip_prefix(r_root) {
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    let meta = entry.metadata().ok();
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let mtime = meta.and_then(|m| m.modified().ok());
                    right_map.insert(rel_str, (size, mtime));
                }
            }
        }

        let mut all_keys: std::collections::BTreeSet<String> = left_map.keys().cloned().collect();
        for k in right_map.keys() {
            all_keys.insert(k.clone());
        }

        for rel_path in all_keys {
            let l = left_map.get(&rel_path);
            let r = right_map.get(&rel_path);
            let (status, selected) = match (l, r) {
                (Some(_), None) => ("Left only", true),
                (None, Some(_)) => ("Right only", true),
                (Some((ls, lm)), Some((rs, rm))) => {
                    if ls == rs {
                        ("Identical", false)
                    } else if lm >= rm {
                        ("Left newer / Diff", true)
                    } else {
                        ("Right newer", true)
                    }
                }
                (None, None) => continue,
            };
            self.sync_entries.push(SyncEntry {
                rel_path,
                left_size: l.map(|x| x.0),
                right_size: r.map(|x| x.0),
                status,
                selected,
            });
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
        let mut trigger_calc_dir_sizes = false;
        let mut trigger_open_bookmarks = false;
        ui.push_id(if is_left { "left_pane" } else { "right_pane" }, |ui| {
            let is_active = self.active_pane_is_left == is_left;
            
            let pane = if is_left {
                &mut self.left_pane
            } else {
                &mut self.right_pane
            };

            // Process async messages
            while let Ok(msg) = pane.rx.try_recv() {
            match msg {
                messages::PaneMessage::SftpConnected(sftp) => {
                    pane.sftp_session = Some(Arc::new(tokio::sync::Mutex::new(sftp)));
                    pane.loading = false;
                    pane.refresh();
                }
                messages::PaneMessage::OpendalConnected(op) => {
                    pane.opendal_op = Some(op);
                    pane.loading = false;
                    pane.refresh();
                }
                messages::PaneMessage::SftpError(err) => {
                    pane.error_msg = Some(err);
                    pane.loading = false;
                }
                messages::PaneMessage::FilesFetched(files) => {
                    pane.files = files;
                    pane.loading = false;
                    pane.sort_files();
                }
                messages::PaneMessage::Refresh => {
                    pane.refresh();
                }
                messages::PaneMessage::DirSizeCalculated(dir_name, total_bytes) => {
                    pane.calculated_dir_sizes.insert(dir_name.clone(), total_bytes);
                    for f in &mut pane.files {
                        if f.is_dir && f.name == dir_name {
                            f.size = total_bytes;
                        }
                    }
                    if pane.sort_column == SortColumn::Size {
                        pane.sort_files();
                    }
                }
            }
        }

        // Removed the strokes/borders as per user request
        let frame = egui::Frame::none()
            .fill(ui.visuals().panel_fill)
            .inner_margin(4.0);

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
                        ui.add(egui::TextEdit::singleline(&mut pane.connection_string).hint_text("user:pass@host:port"));
                        if ui.button("Connect").clicked() {
                            if pane.vfs_type == VfsType::Sftp {
                                pane.loading = true;
                                pane.error_msg = None;
                                let tx = pane.tx.clone();
                                
                                // parse user:pass@host:port
                                let mut conn = pane.connection_string.clone();
                                if !conn.contains("://") {
                                    conn = format!("sftp://{}", conn);
                                }
                                tokio::spawn(async move {
                                    if let Ok(url) = url::Url::parse(&conn) {
                                        let host = url.host_str().unwrap_or("localhost");
                                        let port = url.port().unwrap_or(22);
                                        let user = url.username();
                                        let pass = url.password().unwrap_or("");
                                        
                                        match crate::sftp_client::connect_sftp(host, port, user, pass).await {
                                            Ok((_session, sftp)) => {
                                                let _ = tx.send(messages::PaneMessage::SftpConnected(sftp));
                                            }
                                            Err(e) => {
                                                let _ = tx.send(messages::PaneMessage::SftpError(e));
                                            }
                                        }
                                    } else {
                                        let _ = tx.send(messages::PaneMessage::SftpError("Invalid URI format".to_string()));
                                    }
                                });
                            } else if pane.vfs_type == VfsType::Ftp || pane.vfs_type == VfsType::WebDav {
                                pane.loading = true;
                                pane.error_msg = None;
                                let tx = pane.tx.clone();
                                let conn = pane.connection_string.clone();
                                let vfs_type = pane.vfs_type.clone();
                                
                                tokio::spawn(async move {
                                    match crate::opendal_client::connect_opendal(&vfs_type, &conn) {
                                        Ok(op) => {
                                            let _ = tx.send(messages::PaneMessage::OpendalConnected(op));
                                        }
                                        Err(e) => {
                                            let _ = tx.send(messages::PaneMessage::SftpError(format!("Connect Error: {}", e)));
                                        }
                                    }
                                });
                            }
                        }
                        if pane.vfs_type == VfsType::Sftp {
                            if ui.button("🔑 Profile...").clicked() {
                                self.active_pane_is_left = is_left;
                                self.show_ssh_terminal = true;
                            }
                        }
                        
                        if pane.loading {
                            ui.spinner();
                        }
                    } else {
                        // Dynamic Windows Drives (immediately surrender focus so Space/Enter never re-triggers drive button)
                        for drive in Self::get_windows_drives() {
                            let btn = ui.button(&drive[0..2]).on_hover_text(&drive);
                            if btn.has_focus() {
                                btn.surrender_focus();
                            }
                            if btn.clicked() {
                                self.active_pane_is_left = is_left;
                                pane.navigate_to(PathBuf::from(&drive));
                            }
                        }
                    }
                });

                if let Some(err) = pane.error_msg.clone() {
                    ui.horizontal(|ui| {
                        ui.colored_label(egui::Color32::LIGHT_RED, &err);
                        if ui.small_button("✖").clicked() {
                            pane.error_msg = None;
                        }
                    });
                }
                ui.separator();

                let path_str = pane.current_path.to_string_lossy().into_owned();
                let is_de = self.settings.language.is_de();
                
                // Current Path, History/Bookmarks/Size Buttons & Multi-Select Controls
                ui.horizontal(|ui| {
                    let back_btn = ui
                        .add_enabled(!pane.history.is_empty(), egui::Button::new("◄").small())
                        .on_hover_text(if is_de {
                            "Zurück zum vorherigen Ordner (Alt+Links)"
                        } else {
                            "Back to previous folder (Alt+Left)"
                        });
                    if back_btn.has_focus() { back_btn.surrender_focus(); }
                    if back_btn.clicked() {
                        self.active_pane_is_left = is_left;
                        pane.navigate_back();
                    }

                    let fav_btn = ui.small_button("★").on_hover_text(if is_de {
                        "Lesezeichen / Favoriten (Strg+D)"
                    } else {
                        "Bookmarks / Favorites (Ctrl+D)"
                    });
                    if fav_btn.has_focus() { fav_btn.surrender_focus(); }
                    if fav_btn.clicked() {
                        self.active_pane_is_left = is_left;
                        trigger_open_bookmarks = true;
                    }

                    if is_active {
                        ui.colored_label(egui::Color32::LIGHT_BLUE, egui::RichText::new(&path_str).strong());
                    } else {
                        ui.label(egui::RichText::new(&path_str).strong());
                    }
                    if !pane.selected_files.is_empty() {
                        ui.colored_label(
                            egui::Color32::GOLD,
                            if is_de {
                                format!("({} ausgewählt)", pane.selected_files.len())
                            } else {
                                format!("({} selected)", pane.selected_files.len())
                            },
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !pane.selected_files.is_empty() {
                            let clear_btn = ui.small_button(if is_de { "Auswahl aufheben" } else { "Clear selection" });
                            if clear_btn.has_focus() { clear_btn.surrender_focus(); }
                            if clear_btn.clicked() {
                                pane.selected_files.clear();
                            }
                        }
                        let all_btn = ui.small_button(if is_de { "Alle markieren" } else { "Select all" });
                        if all_btn.has_focus() { all_btn.surrender_focus(); }
                        if all_btn.clicked() {
                            self.active_pane_is_left = is_left;
                            for f in &pane.files {
                                if f.name != ".." {
                                    pane.selected_files.insert(f.full_name());
                                }
                            }
                        }
                        if pane.vfs_type == VfsType::Local {
                            let sz_btn = ui
                                .small_button(if is_de { "📊 Größen" } else { "📊 Sizes" })
                                .on_hover_text(if is_de {
                                    "Ordnergrößen berechnen (Alt+Umschalt+Enter)"
                                } else {
                                    "Calculate folder sizes (Alt+Shift+Enter)"
                                });
                            if sz_btn.has_focus() { sz_btn.surrender_focus(); }
                            if sz_btn.clicked() {
                                self.active_pane_is_left = is_left;
                                trigger_calc_dir_sizes = true;
                            }
                        }
                    });
                });
                ui.separator();
                
                // File List
                let mut action_navigate = None;
                let mut action_open_external: Option<PathBuf> = None;
                let mut commit_rename: Option<(String, String)> = None;
                let mut cancel_rename = false;
                
                // Handle keyboard navigation & Multi-Select (Insert / Space / Shift+Arrows / Ctrl+A)
                let no_modal = !self.show_transfer_dialog
                    && !self.show_mkdir_dialog
                    && !self.show_delete_dialog
                    && !self.show_search_dialog
                    && !self.show_sync_dialog
                    && !self.show_pack_dialog
                    && !self.show_unpack_dialog
                    && !self.show_multi_rename
                    && !self.show_bookmarks_dialog
                    && !self.show_properties_dialog
                    && !self.show_cmdline_output
                    && !self.show_settings_dialog
                    && !self.show_about_dialog
                    && !self.inline_rename_active
                    && !ctx.wants_keyboard_input();
                if is_active && no_modal {
                    if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::A)) {
                        let selectable_count = pane.files.iter().filter(|f| f.name != "..").count();
                        if pane.selected_files.len() >= selectable_count && selectable_count > 0 {
                            pane.selected_files.clear();
                        } else {
                            for f in &pane.files {
                                if f.name != ".." {
                                    pane.selected_files.insert(f.full_name());
                                }
                            }
                        }
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        if ctx.input(|i| i.modifiers.shift) {
                            if let Some(file) = pane.files.get(pane.selected_index) {
                                if file.name != ".." {
                                    let fname = file.full_name();
                                    if !pane.selected_files.remove(&fname) {
                                        pane.selected_files.insert(fname);
                                    }
                                }
                            }
                        }
                        if pane.selected_index + 1 < pane.files.len() {
                            pane.selected_index += 1;
                        }
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        if ctx.input(|i| i.modifiers.shift) {
                            if let Some(file) = pane.files.get(pane.selected_index) {
                                if file.name != ".." {
                                    let fname = file.full_name();
                                    if !pane.selected_files.remove(&fname) {
                                        pane.selected_files.insert(fname);
                                    }
                                }
                            }
                        }
                        if pane.selected_index > 0 {
                            pane.selected_index -= 1;
                        }
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::Insert) || i.key_pressed(egui::Key::Space)) {
                        if let Some(file) = pane.files.get(pane.selected_index) {
                            if file.name != ".." {
                                let fname = file.full_name();
                                if !pane.selected_files.remove(&fname) {
                                    pane.selected_files.insert(fname);
                                }
                            }
                        }
                        if pane.selected_index + 1 < pane.files.len() {
                            pane.selected_index += 1;
                        }
                    }
                    if ctx.input(|i| !i.modifiers.alt && !i.modifiers.ctrl && i.key_pressed(egui::Key::Enter)) {
                        if let Some(file) = pane.files.get(pane.selected_index) {
                            let full_fname = file.full_name();
                            let is_arch = !file.is_dir && crate::archive_ops::is_archive_filename(&full_fname);
                            if file.is_dir || is_arch {
                                action_navigate = Some((if is_arch { full_fname } else { file.name.clone() }, is_arch));
                            } else if pane.vfs_type == VfsType::Local && file.name != ".." {
                                action_open_external = Some(pane.current_path.join(&full_fname));
                            }
                        }
                    }
                }

                let mut shift_select_range: Option<(usize, usize)> = None;
                
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .vscroll(true)
                    .auto_shrink([false, false])
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(egui_extras::Column::exact(24.0)) // Multi-select checkbox
                .column(egui_extras::Column::initial(150.0).at_least(100.0).clip(true).resizable(true)) // Name
                .column(egui_extras::Column::initial(50.0).at_least(30.0).clip(true).resizable(true))  // Ext
                .column(egui_extras::Column::initial(80.0).at_least(50.0).clip(true).resizable(true))  // Size
                .column(egui_extras::Column::remainder().at_least(100.0).clip(true)) // Date
                .header(20.0, |mut header| {
                    let mut sort_clicked = None;
                    header.col(|ui| {
                        ui.label("✓");
                    });
                    for (col, de, en) in [
                        (SortColumn::Name, "Name", "Name"),
                        (SortColumn::Ext, "Erw.", "Ext"),
                        (SortColumn::Size, "Größe", "Size"),
                        (SortColumn::Date, "Datum", "Date"),
                    ] {
                        header.col(|ui| {
                            let base = if is_de { de } else { en };
                            let lbl = if pane.sort_column == col {
                                format!("{} {}", base, if pane.sort_order == SortOrder::Ascending { "▲" } else { "▼" })
                            } else {
                                base.to_string()
                            };
                            if ui.add(egui::Label::new(egui::RichText::new(lbl).strong()).sense(egui::Sense::click())).clicked() {
                                sort_clicked = Some(col);
                            }
                        });
                    }
                    
                    if let Some(col) = sort_clicked {
                        if pane.sort_column == col {
                            pane.sort_order = if pane.sort_order == SortOrder::Ascending { SortOrder::Descending } else { SortOrder::Ascending };
                        } else {
                            pane.sort_column = col;
                            pane.sort_order = SortOrder::Ascending;
                        }
                        pane.sort_files();
                    }
                })
                .body(|mut body| {
                    for (idx, file) in pane.files.iter().enumerate() {
                        let is_selected = is_active && pane.selected_index == idx;
                        let fname = file.full_name();
                        let is_marked = pane.selected_files.contains(&fname);
                        
                        let (name_text, ext_text, size_text, date_text) = if file.name == ".." {
                            ("[..]".to_string(), "".to_string(), "<DIR>".to_string(), "".to_string())
                        } else if file.is_dir {
                            let sz_label = if let Some(&calc_sz) = pane.calculated_dir_sizes.get(&file.name) {
                                format_size_binary(calc_sz)
                            } else {
                                "<DIR>".to_string()
                            };
                            let date_str = file.modified.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                            (format!("[{}]", file.name), "".to_string(), sz_label, date_str)
                        } else {
                            let date_str = file.modified.map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                            (file.name.clone(), file.ext.clone(), format_size_binary(file.size), date_str)
                        };

                        body.row(20.0, |mut row| {
                            row.set_selected(is_selected || is_marked);
                            
                            let mut row_clicked = false;
                            let mut row_double_clicked = false;
                            let mut row_right_clicked = false;

                            row.col(|ui| {
                                if file.name != ".." {
                                    let mut checked = is_marked;
                                    let cb = ui.checkbox(&mut checked, "");
                                    if cb.has_focus() {
                                        cb.surrender_focus();
                                    }
                                    if cb.changed() {
                                        self.active_pane_is_left = is_left;
                                        pane.selected_index = idx;
                                        if checked {
                                            pane.selected_files.insert(fname.clone());
                                        } else {
                                            pane.selected_files.remove(&fname);
                                        }
                                    }
                                }
                            });

                            let mut add_clickable_label = |ui: &mut egui::Ui, text: &str| {
                                let rich = if is_marked {
                                    egui::RichText::new(text).color(egui::Color32::GOLD).strong()
                                } else {
                                    egui::RichText::new(text)
                                };
                                let r = ui.add(egui::Label::new(rich).sense(egui::Sense::click()))
                                          .on_hover_cursor(egui::CursorIcon::Default);
                                if r.clicked() { row_clicked = true; }
                                if r.double_clicked() { row_double_clicked = true; }
                                if r.secondary_clicked() { row_right_clicked = true; }
                            };

                            row.col(|ui| {
                                if is_selected && self.inline_rename_active {
                                    let edit_resp = ui.add(egui::TextEdit::singleline(&mut self.inline_rename_value).desired_width(140.0));
                                    edit_resp.request_focus();
                                    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                                        commit_rename = Some((self.inline_rename_orig.clone(), self.inline_rename_value.clone()));
                                    } else if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                                        cancel_rename = true;
                                    }
                                } else {
                                    add_clickable_label(ui, &name_text);
                                }
                            });
                            row.col(|ui| add_clickable_label(ui, &ext_text));
                            row.col(|ui| add_clickable_label(ui, &size_text));
                            row.col(|ui| add_clickable_label(ui, &date_text));
                            
                            let resp = row.response();
                            if resp.has_focus() {
                                resp.surrender_focus();
                            }
                            if resp.clicked() || row_clicked {
                                let (ctrl_held, shift_held) = ctx.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift));
                                self.active_pane_is_left = is_left;
                                if shift_held {
                                    shift_select_range = Some((pane.selected_index, idx));
                                } else if ctrl_held && file.name != ".." {
                                    if !pane.selected_files.remove(&fname) {
                                        pane.selected_files.insert(fname.clone());
                                    }
                                }
                                pane.selected_index = idx;
                            }
                            if resp.secondary_clicked() || row_right_clicked {
                                self.active_pane_is_left = is_left;
                                pane.selected_index = idx;
                                if file.name != ".." {
                                    if !pane.selected_files.remove(&fname) {
                                        pane.selected_files.insert(fname.clone());
                                    }
                                }
                            }
                            if resp.double_clicked() || row_double_clicked {
                                let full_fname = file.full_name();
                                let is_arch = !file.is_dir && crate::archive_ops::is_archive_filename(&full_fname);
                                if file.is_dir || is_arch {
                                    action_navigate = Some((if is_arch { full_fname } else { file.name.clone() }, is_arch));
                                } else if pane.vfs_type == VfsType::Local && file.name != ".." {
                                    action_open_external = Some(pane.current_path.join(&full_fname));
                                }
                            }
                        });
                    }
                });

                if let Some(ext_path) = action_open_external {
                    crate::tools::open_with_system_default(&ext_path.to_string_lossy());
                }

                if let Some((start_idx, end_idx)) = shift_select_range {
                    let lo = start_idx.min(end_idx);
                    let hi = start_idx.max(end_idx);
                    for i in lo..=hi {
                        if let Some(f) = pane.files.get(i) {
                            if f.name != ".." {
                                pane.selected_files.insert(f.full_name());
                            }
                        }
                    }
                }

                if cancel_rename {
                    self.inline_rename_active = false;
                }
                if let Some((old_name, new_name)) = commit_rename {
                    self.inline_rename_active = false;
                    if !new_name.is_empty() && old_name != new_name {
                        let src_path = pane.current_path.join(&old_name).to_string_lossy().into_owned();
                        let dst_path = pane.current_path.join(&new_name).to_string_lossy().into_owned();
                        let handle = pane.get_vfs_handle();
                        let tx = pane.tx.clone();
                        let task_tx = self.task_tx.clone();
                        let ctx_clone = ctx.clone();
                        tokio::spawn(async move {
                            let _ = crate::file_ops::move_file(&handle, &src_path, &handle, &dst_path, task_tx, new_name).await;
                            let _ = tx.send(messages::PaneMessage::Refresh);
                            ctx_clone.request_repaint();
                        });
                    }
                }

                if let Some((nav_dir, is_arch)) = action_navigate {
                    if nav_dir == ".." {
                        let mut exited_zip = false;
                        if let VfsType::Zip(zip_path) = &pane.vfs_type {
                            let parent = pane.current_path.parent();
                            if pane.current_path.as_os_str().is_empty() || parent.is_none() || parent.unwrap().as_os_str().is_empty() {
                                // Exit archive completely
                                let zp = PathBuf::from(zip_path);
                                pane.vfs_type = VfsType::Local;
                                pane.current_path = zp.parent().unwrap_or(std::path::Path::new("C:\\")).to_path_buf();
                                pane.refresh();
                                pane.selected_index = 0;
                                exited_zip = true;
                            }
                        }
                        
                        if !exited_zip {
                            if let Some(parent) = pane.current_path.parent().map(|p| p.to_path_buf()) {
                                let prev_dir_name = pane.current_path.file_name().unwrap_or_default().to_string_lossy().to_string();
                                
                                pane.navigate_to(parent);
                                
                                // Try to find the directory we just left and select it
                                if let Some(idx) = pane.files.iter().position(|f| f.name.eq_ignore_ascii_case(&prev_dir_name)) {
                                    pane.selected_index = idx;
                                } else {
                                    pane.selected_index = 0;
                                }
                            }
                        }
                    } else if is_arch {
                        let archive_full_path = pane.current_path.join(&nav_dir).to_string_lossy().into_owned();
                        pane.vfs_type = VfsType::Zip(archive_full_path);
                        pane.current_path = PathBuf::new(); // Root of archive
                        pane.refresh();
                        pane.selected_index = 0;
                    } else {
                        let next_path = pane.current_path.join(nav_dir);
                        pane.navigate_to(next_path);
                    }
                }
            });
        });
        }); // Close push_id

        if trigger_open_bookmarks {
            let active_pane = if self.active_pane_is_left { &self.left_pane } else { &self.right_pane };
            self.new_bookmark_input = active_pane.current_path.to_string_lossy().to_string();
            self.show_bookmarks_dialog = true;
        }
        if trigger_calc_dir_sizes {
            self.action_calculate_dir_sizes(ctx);
        }
    }
}

fn main() -> Result<(), eframe::Error> {
    // Initialize rustls default provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Initialize tokio runtime for background network tasks
    let rt = tokio::runtime::Runtime::new().expect("Unable to create tokio runtime");
    let _guard = rt.enter();
    
    // We leak the runtime so it stays alive for the duration of the app, 
    // allowing us to call tokio::spawn from anywhere.
    std::mem::forget(rt);

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1200.0, 800.0])
        .with_title(concat!("FaroCommander v", env!("CARGO_PKG_VERSION")));

    if let Ok(img) = image::load_from_memory(include_bytes!("../LOGO.png")) {
        let rgba = img.to_rgba8();
        let (width, height) = (rgba.width(), rgba.height());
        viewport = viewport.with_icon(Arc::new(egui::IconData {
            rgba: rgba.into_raw(),
            width,
            height,
        }));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "FaroCommander",
        options,
        Box::new(|_cc| Box::<FaroCommander>::default()),
    )
}
mod opendal_client;
