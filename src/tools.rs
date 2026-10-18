use chrono::{DateTime, Local};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;

#[cfg(windows)]
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
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd.exe")
            .args(["/C", "start", "", path_str])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(path_str)
            .spawn();
    }
}

pub fn open_terminal_in_dir(dir_str: &str, use_powershell: bool) {
    #[cfg(windows)]
    {
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
    #[cfg(not(windows))]
    {
        let _ = use_powershell;
        // On Linux, try common terminal emulators
        let terminals = ["x-terminal-emulator", "gnome-terminal", "konsole", "xfce4-terminal", "alacritty", "kitty", "foot", "xterm"];
        for term in terminals {
            if let Ok(mut child) = std::process::Command::new(term)
                .current_dir(dir_str)
                .spawn()
            {
                let _ = child;
                return;
            }
        }
    }
}

pub fn run_command_capture(dir_str: &str, cmd_line: &str) -> String {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd.exe");
        c.args(["/C", cmd_line]).creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", cmd_line]);
        c
    };

    match cmd.current_dir(dir_str).output() {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasswordGenMode {
    #[default]
    Password,
    Words,
    Pin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordCase {
    #[default]
    Upper,
    Title,
    Lower,
}

const WORD_LIST: &[&str] = &[
    "ARCTIC", "WOLF", "FALCON", "SOLAR", "COBALT", "PHOENIX", "THUNDER", "GLACIER", "QUANTUM",
    "NEXUS", "TITAN", "SHADOW", "SILVER", "ORBIT", "MATRIX", "VECTOR", "CRYSTAL", "VORTEX",
    "CYBER", "EMBER", "STORM", "HORIZON", "AURORA", "COSMIC", "DELTA", "OMEGA", "STELLAR",
    "NEBULA", "APEX", "PRISM", "VALKYRIE", "KRAKEN", "GRANITE", "OBSIDIAN", "ZENITH", "ECLIPSE",
    "MIRAGE", "TEMPEST", "VANGUARD", "SENTINEL", "NOMAD", "ATLAS", "COMET", "PULSAR", "METEOR",
    "ONYX", "JADE", "TOPAZ", "AMBER", "CRIMSON", "INDIGO", "SCARLET", "PANTHER", "TIGER", "EAGLE",
    "HAWK", "RAVEN", "LYNX", "COBRA", "VIPER", "BISON", "MANTIS", "DRAGON", "GRIFFIN", "HYDRA",
    "PEGASUS", "SPARTAN", "VIKING", "SAMURAI", "KNIGHT", "RANGER", "PILOT", "CAPTAIN", "BEACON",
    "FORTRESS", "CITADEL", "BASTION", "BUNKER", "HARBOR", "ANCHOR", "COMPASS", "RADAR", "SONAR",
    "LASER", "PLASMA", "PHOTON", "PROTON", "NEUTRON", "GRAVITY", "MAGNET", "TURBO", "DYNAMO",
    "REACTOR", "ENGINE", "ROCKET", "SHUTTLE", "CAPSULE", "SATELLITE", "VOYAGER", "PIONEER",
    "ODYSSEY", "GENESIS", "INFINITY", "PARADOX", "ENIGMA", "CIPHER", "TOKEN", "VAULT", "SHIELD",
    "ARMOR", "BLADE", "ARROW", "LANCE", "HAMMER", "ANVIL", "FORGE", "SPARK", "FLAME", "BLAZE",
    "FROST", "BLIZZARD", "TUNDRA", "TAIGA", "CANYON", "SUMMIT", "RIDGE", "VALLEY", "LAGOON",
    "CORAL", "OCEAN", "PACIFIC", "ATLANTIC", "NORDIC", "ALPINE", "SAHARA", "AMAZON", "EVEREST",
    "OLYMPUS", "ANDES", "SIERRA", "SEQUOIA", "REDWOOD", "CEDAR", "MAPLE", "BIRCH", "WILLOW",
    "LOTUS", "ORCHID", "BAMBOO", "CACTUS", "CLOVER", "ACORN", "WALNUT", "ALMOND", "MANGO",
    "CITRUS", "PEPPER", "GINGER", "SAFFRON", "VANILLA", "COCOA", "COFFEE", "NECTAR", "ELIXIR",
    "VELVET", "MARBLE", "QUARTZ", "BASALT", "COPPER", "BRONZE", "NICKEL", "CHROME", "PLATINUM",
    "TITANIUM", "CARBON", "SILICON", "ARGON", "NEON", "XENON", "HELIUM", "LITHIUM", "ZIRCON",
];

pub struct PasswordSecurityAnalysis {
    pub entropy_bits: f64,
    pub guesses: u64,
    pub score: u8, // 0..=4
    pub crack_100_per_hour: String,
    pub crack_10_per_sec: String,
    pub crack_1e4_per_sec: String,
    pub crack_1e10_per_sec: String,
    pub warning: Option<String>,
    pub suggestions: Vec<String>,
}

pub struct PasswordGeneratorState {
    pub mode: PasswordGenMode,
    pub length: usize,
    pub pin_length: usize,
    pub uppercase: bool,
    pub lowercase: bool,
    pub numbers: bool,
    pub symbols: bool,
    pub exclude_ambiguous: bool,
    // Words / Passphrase options (e.g. ARCTIC-WOLF-XDFG)
    pub word_count: usize,
    pub word_separator: String,
    pub word_case: WordCase,
    pub word_suffix_len: usize,
    pub word_suffix_digits: bool,
    pub current: String,
    pub history: Vec<String>,
    pub copied_notice: bool,
}

impl Default for PasswordGeneratorState {
    fn default() -> Self {
        let mut s = Self {
            mode: PasswordGenMode::Words,
            length: 16,
            pin_length: 6,
            uppercase: true,
            lowercase: true,
            numbers: true,
            symbols: true,
            exclude_ambiguous: false,
            word_count: 2,
            word_separator: "-".to_string(),
            word_case: WordCase::Upper,
            word_suffix_len: 4,
            word_suffix_digits: false,
            current: String::new(),
            history: Vec::new(),
            copied_notice: false,
        };
        s.generate();
        s
    }
}

impl PasswordGeneratorState {
    pub fn generate(&mut self) {
        use ring::rand::{SecureRandom, SystemRandom};
        let rng = SystemRandom::new();
        let rand_usize = |bound: usize| -> usize {
            if bound <= 1 {
                return 0;
            }
            let mut buf = [0u8; 8];
            let _ = rng.fill(&mut buf);
            (u64::from_le_bytes(buf) as usize) % bound
        };

        let result = match self.mode {
            PasswordGenMode::Pin => {
                let digits = b"0123456789";
                (0..self.pin_length.clamp(3, 32))
                    .map(|_| digits[rand_usize(digits.len())] as char)
                    .collect::<String>()
            }
            PasswordGenMode::Words => {
                let count = self.word_count.clamp(1, 8);
                let mut parts: Vec<String> = Vec::with_capacity(count + 1);
                for _ in 0..count {
                    let raw = WORD_LIST[rand_usize(WORD_LIST.len())];
                    let formatted = match self.word_case {
                        WordCase::Upper => raw.to_uppercase(),
                        WordCase::Lower => raw.to_lowercase(),
                        WordCase::Title => {
                            let mut c = raw.chars();
                            match c.next() {
                                Some(first) => {
                                    first.to_uppercase().collect::<String>()
                                        + &c.as_str().to_lowercase()
                                }
                                None => String::new(),
                            }
                        }
                    };
                    parts.push(formatted);
                }
                if self.word_suffix_len > 0 {
                    let charset: &[u8] = match (self.word_case, self.word_suffix_digits) {
                        (WordCase::Lower, false) => b"abcdefghjkmnpqrstuvwxyz",
                        (WordCase::Lower, true) => b"abcdefghjkmnpqrstuvwxyz23456789",
                        (_, false) => b"ABCDEFGHJKLMNPQRSTUVWXYZ",
                        (_, true) => b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789",
                    };
                    let suffix: String = (0..self.word_suffix_len.clamp(1, 12))
                        .map(|_| charset[rand_usize(charset.len())] as char)
                        .collect();
                    parts.push(suffix);
                }
                parts.join(&self.word_separator)
            }
            PasswordGenMode::Password => {
                let ambig = |c: &char| matches!(c, '0' | 'O' | 'o' | '1' | 'l' | 'I' | '|');
                let mut sets: Vec<(Vec<char>, usize)> = Vec::new();
                if self.lowercase {
                    let v: Vec<char> = ('a'..='z')
                        .filter(|c| !self.exclude_ambiguous || !ambig(c))
                        .collect();
                    sets.push((v, 30));
                }
                if self.uppercase {
                    let v: Vec<char> = ('A'..='Z')
                        .filter(|c| !self.exclude_ambiguous || !ambig(c))
                        .collect();
                    sets.push((v, 30));
                }
                if self.numbers {
                    let v: Vec<char> = ('0'..='9')
                        .filter(|c| !self.exclude_ambiguous || !ambig(c))
                        .collect();
                    sets.push((v, 20));
                }
                if self.symbols {
                    let v: Vec<char> = "~`!@#$%^&*()-_+={}[]|\\:;\"',.<>/?"
                        .chars()
                        .filter(|c| !self.exclude_ambiguous || !ambig(c))
                        .collect();
                    sets.push((v, 20));
                }
                if sets.is_empty() {
                    sets.push((('a'..='z').collect(), 100));
                }

                let len = self.length.clamp(4, 128);
                let mut chars: Vec<char> = Vec::with_capacity(len);
                for (set, _) in sets.iter().take(len) {
                    if !set.is_empty() {
                        chars.push(set[rand_usize(set.len())]);
                    }
                }
                let total_weight: usize = sets.iter().map(|(_, w)| *w).sum();
                while chars.len() < len {
                    let mut pick = rand_usize(total_weight);
                    for (set, w) in &sets {
                        if pick < *w {
                            chars.push(set[rand_usize(set.len())]);
                            break;
                        }
                        pick -= *w;
                    }
                }
                for i in (1..chars.len()).rev() {
                    let j = rand_usize(i + 1);
                    chars.swap(i, j);
                }
                chars.into_iter().collect()
            }
        };

        if !self.current.is_empty() && self.history.first() != Some(&self.current) {
            self.history.insert(0, self.current.clone());
            self.history.truncate(8);
        }
        self.current = result;
        self.copied_notice = false;
    }

    pub fn analyze(pw: &str, is_de: bool) -> PasswordSecurityAnalysis {
        if pw.is_empty() {
            let instant = if is_de { "Sofort" } else { "Instant" }.to_string();
            return PasswordSecurityAnalysis {
                entropy_bits: 0.0,
                guesses: 0,
                score: 0,
                crack_100_per_hour: instant.clone(),
                crack_10_per_sec: instant.clone(),
                crack_1e4_per_sec: instant.clone(),
                crack_1e10_per_sec: instant,
                warning: None,
                suggestions: Vec::new(),
            };
        }

        let entropy = zxcvbn::zxcvbn(pw, &[]);
        let guesses = entropy.guesses();
        let entropy_bits = if guesses > 0 {
            (guesses as f64).log2()
        } else {
            0.0
        };
        let score: u8 = entropy.score().into();
        let ct = entropy.crack_times();

        let (warning, suggestions) = match entropy.feedback() {
            Some(fb) => (
                fb.warning().map(|w| w.to_string()),
                fb.suggestions().iter().map(|s| s.to_string()).collect(),
            ),
            None => (None, Vec::new()),
        };

        PasswordSecurityAnalysis {
            entropy_bits,
            guesses,
            score,
            crack_100_per_hour: ct.online_throttling_100_per_hour().to_string(),
            crack_10_per_sec: ct.online_no_throttling_10_per_second().to_string(),
            crack_1e4_per_sec: ct.offline_slow_hashing_1e4_per_second().to_string(),
            crack_1e10_per_sec: ct.offline_fast_hashing_1e10_per_second().to_string(),
            warning,
            suggestions,
        }
    }
}

