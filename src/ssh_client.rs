use std::sync::mpsc::Sender;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::pbkdf2::{derive, PBKDF2_HMAC_SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{client, ChannelMsg};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

const SSH_VAULT_FILE: &str = "ssh_vault.json";
const LEGACY_PROFILES_FILE: &str = "ssh_profiles.json";
const PBKDF2_ITERATIONS: u32 = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SshAuthMethod {
    Password,
    KeyFile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SshProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth_method: SshAuthMethod,
    pub password: String,
    pub key_path: String,
    pub key_passphrase: String,
}

impl Default for SshProfile {
    fn default() -> Self {
        Self {
            name: "Neuer Server".to_string(),
            host: "127.0.0.1".to_string(),
            port: 22,
            user: "root".to_string(),
            auth_method: SshAuthMethod::KeyFile,
            password: String::new(),
            key_path: default_ssh_key_path(),
            key_passphrase: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct EncryptedVaultFile {
    salt: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

pub fn vault_exists() -> bool {
    std::path::Path::new(SSH_VAULT_FILE).exists() || std::path::Path::new("rdp_vault.json").exists()
}

pub fn reset_vault() {
    let _ = std::fs::remove_file(SSH_VAULT_FILE);
    let _ = std::fs::remove_file("rdp_vault.json");
    let _ = std::fs::remove_file(LEGACY_PROFILES_FILE);
}

/// Validates that the master password has at least 8 characters, at least one uppercase, and at least one lowercase letter.
pub fn validate_master_password(pw: &str, is_de: bool) -> Result<(), &'static str> {
    if pw.chars().count() < 8 {
        return Err(if is_de {
            "Passwort muss mindestens 8 Zeichen lang sein."
        } else {
            "Password must be at least 8 characters long."
        });
    }
    if !pw.chars().any(|c| c.is_uppercase()) {
        return Err(if is_de {
            "Passwort muss mindestens einen Großbuchstaben (A-Z) enthalten."
        } else {
            "Password must contain at least one uppercase letter (A-Z)."
        });
    }
    if !pw.chars().any(|c| c.is_lowercase()) {
        return Err(if is_de {
            "Passwort muss mindestens einen Kleinbuchstaben (a-z) enthalten."
        } else {
            "Password must contain at least one lowercase letter (a-z)."
        });
    }
    Ok(())
}

fn derive_vault_key(master_password: &str, salt: &[u8]) -> Result<LessSafeKey, String> {
    let mut key_bytes = [0u8; 32];
    derive(
        PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF2_ITERATIONS).unwrap(),
        salt,
        master_password.as_bytes(),
        &mut key_bytes,
    );
    let unbound = UnboundKey::new(&AES_256_GCM, &key_bytes).map_err(|_| "Key init failed".to_string())?;
    Ok(LessSafeKey::new(unbound))
}

pub fn save_encrypted_json<T: Serialize>(path: &str, master_password: &str, data: &T) -> Result<(), String> {
    let rng = SystemRandom::new();
    let mut salt = vec![0u8; 16];
    let mut nonce_bytes = [0u8; 12];
    rng.fill(&mut salt).map_err(|_| "RNG failure".to_string())?;
    rng.fill(&mut nonce_bytes).map_err(|_| "RNG failure".to_string())?;

    let key = derive_vault_key(master_password, &salt)?;
    let mut in_out = serde_json::to_vec(data).map_err(|e| e.to_string())?;
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce_bytes), Aad::empty(), &mut in_out)
        .map_err(|_| "Encryption failed".to_string())?;

    let vault = EncryptedVaultFile {
        salt,
        nonce: nonce_bytes.to_vec(),
        ciphertext: in_out,
    };
    let json = serde_json::to_string_pretty(&vault).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

pub fn load_encrypted_json<T: for<'de> Deserialize<'de>>(path: &str, master_password: &str) -> Result<T, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let vault: EncryptedVaultFile = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if vault.nonce.len() != 12 {
        return Err("Invalid vault nonce".into());
    }
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&vault.nonce);

    let key = derive_vault_key(master_password, &vault.salt)?;
    let mut buf = vault.ciphertext;
    let decrypted = key
        .open_in_place(Nonce::assume_unique_for_key(nonce_bytes), Aad::empty(), &mut buf)
        .map_err(|_| "Falsches Tresor-Passwort / Invalid Master Password".to_string())?;

    serde_json::from_slice(decrypted).map_err(|e| e.to_string())
}

pub fn save_vault(master_password: &str, profiles: &[SshProfile]) -> Result<(), String> {
    save_encrypted_json(SSH_VAULT_FILE, master_password, &profiles)?;
    let _ = std::fs::remove_file(LEGACY_PROFILES_FILE);
    Ok(())
}

pub fn unlock_vault(master_password: &str) -> Result<Vec<SshProfile>, String> {
    load_encrypted_json(SSH_VAULT_FILE, master_password)
}

pub fn default_ssh_key_path() -> String {
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let ssh_dir = PathBuf::from(&home).join(".ssh");
        for candidate in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let p = ssh_dir.join(candidate);
            if p.exists() {
                return p.to_string_lossy().to_string();
            }
        }
        return ssh_dir.join("id_ed25519").to_string_lossy().to_string();
    }
    "~/.ssh/id_ed25519".to_string()
}

pub fn resolve_key_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(trimmed)
}

pub struct SshHandler {}

impl client::Handler for SshHandler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub async fn connect_and_authenticate(
    profile: &SshProfile,
) -> Result<client::Handle<SshHandler>, String> {
    let config = Arc::new(client::Config::default());
    let mut session = client::connect(config, (profile.host.as_str(), profile.port), SshHandler {})
        .await
        .map_err(|e| format!("Connection failed ({}:{}): {}", profile.host, profile.port, e))?;

    let auth_res = match profile.auth_method {
        SshAuthMethod::Password => session
            .authenticate_password(&profile.user, &profile.password)
            .await
            .map_err(|e| format!("Password auth error: {}", e))?,
        SshAuthMethod::KeyFile => {
            let key_path = resolve_key_path(&profile.key_path);
            let pass_opt = if profile.key_passphrase.is_empty() {
                None
            } else {
                Some(profile.key_passphrase.as_str())
            };
            let secret_key = load_secret_key(&key_path, pass_opt)
                .map_err(|e| format!("Key error ({}): {}", key_path.display(), e))?;
            let rsa_hash = session
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .unwrap_or(Some(russh::keys::HashAlg::Sha256));
            let key_with_alg = PrivateKeyWithHashAlg::new(Arc::new(secret_key), rsa_hash);
            session
                .authenticate_publickey(&profile.user, key_with_alg)
                .await
                .map_err(|e| format!("Public key auth error: {}", e))?
        }
    };

    if !matches!(auth_res, client::AuthResult::Success) {
        return Err("Authentication rejected by server".into());
    }

    Ok(session)
}

/// Strip ANSI CSI/OSC escape sequences so output renders cleanly in standard monospace TextEdit
pub fn strip_ansi_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if ('@'..='~').contains(&nc) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if nc == '\x07' {
                            break;
                        }
                        if nc == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

pub async fn start_ssh_terminal_profile(
    profile: SshProfile,
    ui_tx: Sender<crate::messages::SshMessage>,
    mut input_rx: tokio::sync::mpsc::Receiver<String>,
) {
    let session = match connect_and_authenticate(&profile).await {
        Ok(s) => s,
        Err(e) => {
            let _ = ui_tx.send(crate::messages::SshMessage::Error(e));
            return;
        }
    };

    let mut channel = match session.channel_open_session().await {
        Ok(c) => c,
        Err(e) => {
            let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("Channel error: {}", e)));
            return;
        }
    };

    // Standard 80x24 terminal PTY
    if let Err(e) = channel.request_pty(true, "xterm-256color", 80, 24, 0, 0, &[]).await {
        let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("PTY error: {}", e)));
    }

    if let Err(e) = channel.request_shell(true).await {
        let _ = ui_tx.send(crate::messages::SshMessage::Error(format!("Shell error: {}", e)));
    }

    let auth_tag = match profile.auth_method {
        SshAuthMethod::Password => "Password",
        SshAuthMethod::KeyFile => "SSH-Key",
    };
    let _ = ui_tx.send(crate::messages::SshMessage::Output(format!(
        "✓ Connected to {}@{}:{} ({}, 80x24 PTY)\n",
        profile.user, profile.host, profile.port, auth_tag
    )));

    loop {
        tokio::select! {
            input_opt = input_rx.recv() => {
                if let Some(mut input) = input_opt {
                    if input != "\x03" && !input.ends_with('\n') {
                        input.push('\n');
                    }
                    let _ = channel.data(input.as_bytes()).await;
                } else {
                    let _ = channel.eof().await;
                    break;
                }
            }
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        let raw = String::from_utf8_lossy(&data);
                        let clean = strip_ansi_codes(&raw);
                        if !clean.is_empty() {
                            let _ = ui_tx.send(crate::messages::SshMessage::Output(clean));
                        }
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        let _ = ui_tx.send(crate::messages::SshMessage::Disconnected);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}
