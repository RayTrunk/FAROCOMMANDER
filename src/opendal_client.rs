use opendal::Operator;
use opendal::services::{Ftp, Webdav};
use url::Url;
use futures::TryStreamExt;

pub fn connect_opendal(vfs_type: &crate::VfsType, conn_str: &str) -> Result<Operator, String> {
    let mut conn = conn_str.to_string();
    if !conn.contains("://") {
        conn = format!("{}://{}", vfs_type.name().to_lowercase(), conn);
    }
    let url = Url::parse(&conn)
        .map_err(|e| e.to_string())?;
    
    let host = url.host_str().unwrap_or("localhost");
    let port = url.port();
    let user = url.username();
    let pass = url.password().unwrap_or("");
    
    let endpoint = if let Some(p) = port {
        format!("{}:{}", host, p)
    } else {
        host.to_string()
    };

    match vfs_type {
        crate::VfsType::Ftp => {
            let mut builder = Ftp::default();
            builder = builder.endpoint(&endpoint);
            if !user.is_empty() {
                builder = builder.user(user);
            }
            if !pass.is_empty() {
                builder = builder.password(pass);
            }
            Ok(Operator::new(builder).map_err(|e| e.to_string())?.into())
        }
        crate::VfsType::WebDav => {
            let mut builder = Webdav::default();
            builder = builder.endpoint(&format!("http://{}", endpoint));
            if !user.is_empty() {
                builder = builder.username(user);
            }
            if !pass.is_empty() {
                builder = builder.password(pass);
            }
            Ok(Operator::new(builder).map_err(|e| e.to_string())?.into())
        }
        _ => Err("Unsupported VFS type for OpenDAL".into())
    }
}

pub async fn list_opendal_dir(op: &Operator, path: &str) -> Result<Vec<crate::FileEntry>, String> {
    let path = if path.is_empty() { "/" } else { path };
    let mut entries = Vec::new();
    if path != "." && path != "/" {
        entries.push(crate::FileEntry::parent_dir());
    }
    let mut lister = op.lister(path).await.map_err(|e| e.to_string())?;
    while let Some(entry) = lister.try_next().await.map_err(|e| e.to_string())? {
        let meta = op.stat(entry.path()).await.map_err(|e| e.to_string())?;
        let filename = entry.name().trim_end_matches('/').to_string();
        entries.push(crate::FileEntry::from_filename(
            filename,
            meta.mode().is_dir(),
            meta.content_length(),
            None,
        ));
    }
    Ok(entries)
}
