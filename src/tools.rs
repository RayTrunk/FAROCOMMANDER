use chrono::{DateTime, Local};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::Path;

const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    Unchanged,
    Lowercase,
    Uppercase,
    TitleCase,
}

impl CaseMode {
    pub fn label(&self) -> &'static str {
        match self {
            CaseMode::Unchanged => "Unverändert",
            CaseMode::Lowercase => "alles klein",
            CaseMode::Uppercase => "ALLES GROSS",
            CaseMode::TitleCase => "Erster Buchstabe groß",
        }
    }
}

#[derive(Clone)]
pub struct MultiRenameState {
    pub items: Vec<(String, String, bool)>, // (full_path, old_file_name, is_dir)
    pub name_mask: String,
    pub ext_mask: String,
    pub search_for: String,
    pub replace_with: String,
    pub case_mode: CaseMode,
    pub counter_start: u32,
    pub counter_step: u32,
    pub counter_digits: usize,
}

impl Default for MultiRenameState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            name_mask: "[N]".to_string(),
            ext_mask: "[E]".to_string(),
            search_for: "".to_string(),
            replace_with: "".to_string(),
            case_mode: CaseMode::Unchanged,
            counter_start: 1,
            counter_step: 1,
            counter_digits: 3,
        }
    }
}

impl MultiRenameState {
    pub fn compute_new_name(&self, old_full_name: &str, is_dir: bool, index: usize) -> String {
        let p = Path::new(old_full_name);
        let orig_name = if is_dir {
            old_full_name.to_string()
        } else {
            p.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| old_full_name.to_string())
        };
        let orig_ext = if is_dir {
            "".to_string()
        } else {
            p.extension()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        };

        let num = self.counter_start + (index as u32) * self.counter_step;
        let counter_str = format!("{:0width$}", num, width = self.counter_digits);
        let now = Local::now();
        let y_str = now.format("%Y").to_string();
        let m_str = now.format("%m").to_string();
        let d_str = now.format("%d").to_string();

        let apply_tokens = |mask: &str| -> String {
            mask.replace("[N]", &orig_name)
                .replace("[n]", &orig_name)
                .replace("[E]", &orig_ext)
                .replace("[e]", &orig_ext)
                .replace("[C]", &counter_str)
                .replace("[c]", &counter_str)
                .replace("[Y]", &y_str)
                .replace("[M]", &m_str)
                .replace("[D]", &d_str)
        };

        let new_stem = apply_tokens(&self.name_mask);
        let new_ext = apply_tokens(&self.ext_mask);

        let mut combined = if is_dir || new_ext.trim().is_empty() {
            new_stem
        } else {
            format!("{}.{}", new_stem, new_ext.trim_start_matches('.'))
        };

        if !self.search_for.is_empty() {
            combined = combined.replace(&self.search_for, &self.replace_with);
        }

        match self.case_mode {
            CaseMode::Unchanged => combined,
            CaseMode::Lowercase => combined.to_lowercase(),
            CaseMode::Uppercase => combined.to_uppercase(),
            CaseMode::TitleCase => {
                let mut chars = combined.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                    }
                }
            }
        }
    }
}

const BOOKMARKS_FILE: &str = "bookmarks.json";

pub fn load_bookmarks() -> Vec<String> {
    if let Ok(data) = std::fs::read_to_string(BOOKMARKS_FILE) {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&data) {
            if !list.is_empty() {
                return list;
            }
        }
    }
    let mut defaults = vec!["C:\\".to_string()];
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = cwd.to_string_lossy().to_string();
        if !defaults.contains(&cwd_str) {
            defaults.push(cwd_str);
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if Path::new(&profile).exists() && !defaults.contains(&profile) {
            defaults.push(profile);
        }
    }
    defaults
}

pub fn save_bookmarks(bookmarks: &[String]) {
    if let Ok(json) = serde_json::to_string_pretty(bookmarks) {
        let _ = std::fs::write(BOOKMARKS_FILE, json);
    }
}

pub fn calculate_dir_size(path: &Path) -> u64 {
    compute_hashes_or_dir_stats(&path.to_string_lossy(), true).map(|t| t.0).unwrap_or(0)
}

#[derive(Clone, Default)]
pub struct FilePropertiesState {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub file_count: usize,
    pub dir_count: usize,
    pub created: String,
    pub modified: String,
    pub readonly: bool,
    pub crc32: String,
    pub md5: String,
    pub sha256: String,
    pub calculating: bool,
    pub compare_hash: String,
}

pub fn inspect_path_sync(path_str: &str) -> FilePropertiesState {
    let p = Path::new(path_str);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path_str.to_string());
    let meta = std::fs::metadata(p).ok();
    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let readonly = meta.as_ref().map(|m| m.permissions().readonly()).unwrap_or(false);
    let created = meta
        .as_ref()
        .and_then(|m| m.created().ok())
        .map(|t| DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "-".to_string());
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .map(|t| DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "-".to_string());

    FilePropertiesState {
        name,
        full_path: path_str.to_string(),
        is_dir,
        size,
        file_count: 0,
        dir_count: 0,
        created,
        modified,
        readonly,
        crc32: if is_dir { "-".to_string() } else { "Berechne...".to_string() },
        md5: if is_dir { "-".to_string() } else { "Berechne...".to_string() },
        sha256: if is_dir { "-".to_string() } else { "Berechne...".to_string() },
        calculating: true,
        compare_hash: String::new(),
    }
}

pub fn compute_hashes_or_dir_stats(
    path_str: &str,
    is_dir: bool,
) -> Result<(u64, usize, usize, String, String, String), String> {
    let p = Path::new(path_str);
    if is_dir {
        let mut total_size = 0u64;
        let mut files = 0usize;
        let mut dirs = 0usize;
        for entry in walkdir::WalkDir::new(p).into_iter().filter_map(|e| e.ok()) {
            if entry.path() == p {
                continue;
            }
            if entry.file_type().is_dir() {
                dirs += 1;
            } else if entry.file_type().is_file() {
                files += 1;
                if let Ok(m) = entry.metadata() {
                    total_size = total_size.saturating_add(m.len());
                }
            }
        }
        Ok((
            total_size,
            files,
            dirs,
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
        ))
    } else {
        let mut file = File::open(p).map_err(|e| e.to_string())?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut crc = crc32fast::Hasher::new();
        let mut md5_ctx = md5::Context::new();
        let mut sha256_ctx = Sha256::new();

        let mut buf = vec![0u8; 65536];
        loop {
            let n = file.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            crc.update(&buf[..n]);
            md5_ctx.consume(&buf[..n]);
            sha256_ctx.update(&buf[..n]);
        }

        let crc_hex = format!("{:08X}", crc.finalize());
        let md5_hex = format!("{:x}", md5_ctx.finalize());
        let sha256_hex = format!("{:x}", sha256_ctx.finalize());

        Ok((size, 1, 0, crc_hex, md5_hex, sha256_hex))
    }
}

pub fn open_with_system_default(path_str: &str) {
    let _ = std::process::Command::new("cmd.exe")
        .args(["/C", "start", "", path_str])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

pub fn open_terminal_in_dir(dir_str: &str, use_powershell: bool) {
    if use_powershell {
        let _ = std::process::Command::new("cmd.exe")
            .args(["/C", "start", "powershell.exe", "-NoExit", "-Command", &format!("Set-Location -LiteralPath '{}'", dir_str.replace('\'', "''"))])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    } else {
        let _ = std::process::Command::new("cmd.exe")
            .args(["/C", "start", "cmd.exe", "/K", &format!("cd /d \"{}\"", dir_str)])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

pub fn run_command_capture(dir_str: &str, cmd_line: &str) -> String {
    match std::process::Command::new("cmd.exe")
        .args(["/C", cmd_line])
        .current_dir(dir_str)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(out) => {
            let mut res = String::new();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stdout.is_empty() {
                res.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !res.is_empty() {
                    res.push('\n');
                }
                res.push_str(&stderr);
            }
            if res.trim().is_empty() {
                format!("Befehl '{}' erfolgreich ausgeführt (Exit-Code: {}).", cmd_line, out.status)
            } else {
                res
            }
        }
        Err(e) => format!("Fehler beim Ausführen von '{}': {}", cmd_line, e),
    }
}

// --- Settings: Themes & Language (Deutsch / English) ---

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum AppLanguage {
    Deutsch,
    English,
}

impl AppLanguage {
    pub fn label(&self) -> &'static str {
        match self {
            AppLanguage::Deutsch => "🇩🇪 Deutsch",
            AppLanguage::English => "🇬🇧 English",
        }
    }

    pub fn is_de(&self) -> bool {
        matches!(self, AppLanguage::Deutsch)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum AppTheme {
    Dark,
    Light,
    ClassicBlue,
    Midnight,
    SolarizedDark,
}

impl AppTheme {
    pub fn label(&self, lang: AppLanguage) -> &'static str {
        match (self, lang) {
            (AppTheme::Dark, AppLanguage::Deutsch) => "🌙 Dunkel (Standard)",
            (AppTheme::Dark, AppLanguage::English) => "🌙 Dark (Default)",
            (AppTheme::Light, AppLanguage::Deutsch) => "☀ Hell (Windows Light)",
            (AppTheme::Light, AppLanguage::English) => "☀ Light (Windows Light)",
            (AppTheme::ClassicBlue, AppLanguage::Deutsch) => "🟦 Klassisch Blau (Commander Retro)",
            (AppTheme::ClassicBlue, AppLanguage::English) => "🟦 Classic Blue (Commander Retro)",
            (AppTheme::Midnight, AppLanguage::Deutsch) => "🌌 Mitternacht (OLED / Cyber)",
            (AppTheme::Midnight, AppLanguage::English) => "🌌 Midnight (OLED / Cyber)",
            (AppTheme::SolarizedDark, AppLanguage::Deutsch) => "🌲 Solarized Dunkel",
            (AppTheme::SolarizedDark, AppLanguage::English) => "🌲 Solarized Dark",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AppSettings {
    pub language: AppLanguage,
    pub theme: AppTheme,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            language: AppLanguage::Deutsch,
            theme: AppTheme::Dark,
        }
    }
}

const SETTINGS_FILE: &str = "settings.json";

pub fn load_settings() -> AppSettings {
    if let Ok(data) = std::fs::read_to_string(SETTINGS_FILE) {
        if let Ok(s) = serde_json::from_str::<AppSettings>(&data) {
            return s;
        }
    }
    AppSettings::default()
}

pub fn save_settings(settings: &AppSettings) {
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(SETTINGS_FILE, json);
    }
}

pub fn apply_theme(ctx: &eframe::egui::Context, theme: AppTheme) {
    use eframe::egui::{Color32, Visuals};
    let visuals = match theme {
        AppTheme::Dark => Visuals::dark(),
        AppTheme::Light => Visuals::light(),
        AppTheme::ClassicBlue => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(0, 32, 96);
            v.window_fill = Color32::from_rgb(0, 42, 112);
            v.extreme_bg_color = Color32::from_rgb(0, 22, 68);
            v.faint_bg_color = Color32::from_rgb(0, 50, 128);
            v.override_text_color = Some(Color32::from_rgb(235, 245, 255));
            v.selection.bg_fill = Color32::from_rgb(0, 140, 180);
            v.selection.stroke.color = Color32::from_rgb(255, 255, 100);
            v.widgets.noninteractive.bg_fill = Color32::from_rgb(0, 38, 105);
            v.widgets.inactive.bg_fill = Color32::from_rgb(0, 62, 145);
            v.widgets.hovered.bg_fill = Color32::from_rgb(0, 95, 195);
            v.widgets.active.bg_fill = Color32::from_rgb(0, 130, 220);
            v
        }
        AppTheme::Midnight => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(12, 14, 20);
            v.window_fill = Color32::from_rgb(16, 19, 28);
            v.extreme_bg_color = Color32::from_rgb(6, 8, 12);
            v.faint_bg_color = Color32::from_rgb(20, 24, 36);
            v.override_text_color = Some(Color32::from_rgb(220, 230, 245));
            v.selection.bg_fill = Color32::from_rgb(45, 95, 190);
            v.widgets.inactive.bg_fill = Color32::from_rgb(26, 32, 48);
            v.widgets.hovered.bg_fill = Color32::from_rgb(42, 54, 82);
            v
        }
        AppTheme::SolarizedDark => {
            let mut v = Visuals::dark();
            v.panel_fill = Color32::from_rgb(0, 43, 54);
            v.window_fill = Color32::from_rgb(7, 54, 66);
            v.extreme_bg_color = Color32::from_rgb(0, 33, 42);
            v.faint_bg_color = Color32::from_rgb(10, 62, 76);
            v.override_text_color = Some(Color32::from_rgb(147, 161, 161));
            v.selection.bg_fill = Color32::from_rgb(38, 139, 210);
            v.widgets.inactive.bg_fill = Color32::from_rgb(14, 68, 82);
            v.widgets.hovered.bg_fill = Color32::from_rgb(24, 92, 110);
            v
        }
    };
    ctx.set_visuals(visuals);
}

