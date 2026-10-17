use russh::client;
use russh_sftp::client::SftpSession;
use crate::FileEntry;
use crate::ssh_client::{connect_and_authenticate, SshAuthMethod, SshHandler, SshProfile};

pub async fn connect_sftp_profile(
    profile: &SshProfile,
) -> Result<(client::Handle<SshHandler>, SftpSession), String> {
    let session = connect_and_authenticate(profile).await?;
    let channel = session.channel_open_session().await.map_err(|e| e.to_string())?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| e.to_string())?;
    let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| e.to_string())?;
    Ok((session, sftp))
}

pub async fn connect_sftp(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
) -> Result<(client::Handle<SshHandler>, SftpSession), String> {
    let profile = SshProfile {
        name: String::new(),
        host: host.to_string(),
        port,
        user: user.to_string(),
        auth_method: SshAuthMethod::Password,
        password: pass.to_string(),
        key_path: String::new(),
        key_passphrase: String::new(),
    };
    connect_sftp_profile(&profile).await
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
