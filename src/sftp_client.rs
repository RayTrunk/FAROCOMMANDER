use russh::client;
use russh::client::AuthResult;
use russh::keys::PublicKeyOrCertificate;
use russh_sftp::client::SftpSession;
use std::sync::Arc;
use crate::FileEntry;

pub struct ClientHandler {}

impl client::Handler for ClientHandler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true) // Accept all keys for now
    }
}

pub async fn connect_sftp(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
) -> Result<(client::Handle<ClientHandler>, SftpSession), String> {
    let config = client::Config {
        ..<_>::default()
    };
    let config = Arc::new(config);
    let sh = ClientHandler {};
    
    let mut session = client::connect(config, (host, port), sh).await.map_err(|e| e.to_string())?;
    
    let auth_res = session.authenticate_password(user, pass).await.map_err(|e| e.to_string())?;
    if !matches!(auth_res, AuthResult::Success) {
        return Err("Authentication failed".into());
    }

    let channel = session.channel_open_session().await.map_err(|e| e.to_string())?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| e.to_string())?;
    
    let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| e.to_string())?;
    
    Ok((session, sftp))
}

pub async fn list_sftp_dir(sftp: &SftpSession, path: &str) -> Result<Vec<FileEntry>, String> {
    let mut dir = sftp.read_dir(path).await.map_err(|e| e.to_string())?;
    
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    
    // Add parent directory entry if not at root
    if path != "." && path != "/" && path != "" {
        dirs.push(FileEntry {
            name: "..".to_string(),
            ext: String::new(),
            is_dir: true,
            size: 0,
            modified: None,
        });
    }
    
    while let Some(item) = dir.next() {
        let metadata = item.metadata();
        let is_dir = metadata.is_dir();
        
        // russh-sftp's ReadDir iterator automatically filters out "." and ".."
        // so we don't need to manually skip them here.

        let size = metadata.len();
        let modified = metadata.modified().ok()
            .map(|sys_time| chrono::DateTime::from(sys_time));

        let filename = item.file_name();
        
        let (name, ext) = if is_dir {
            (filename.clone(), "".to_string())
        } else {
            let p = std::path::Path::new(&filename);
            let n = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let e = p.extension().unwrap_or_default().to_string_lossy().to_string();
            (n, e)
        };

        let entry = FileEntry {
            name: if is_dir { filename } else { name },
            ext,
            is_dir,
            size,
            modified,
        };

        if is_dir {
            dirs.push(entry);
        } else {
            files.push(entry);
        }
    }
    
    // Sort alphabetically, ignoring the first item if it's ".."
    let start_idx = if !dirs.is_empty() && dirs[0].name == ".." { 1 } else { 0 };
    dirs[start_idx..].sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    
    dirs.extend(files);
    Ok(dirs)
}
