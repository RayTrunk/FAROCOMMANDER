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
    let mut entries = Vec::new();
    if path != "." && path != "/" && !path.is_empty() {
        entries.push(FileEntry::parent_dir());
    }
    while let Some(item) = dir.next() {
        let metadata = item.metadata();
        let modified = metadata.modified().ok().map(chrono::DateTime::from);
        entries.push(FileEntry::from_filename(
            item.file_name(),
            metadata.is_dir(),
            metadata.len(),
            modified,
        ));
    }
    Ok(entries)
}
