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
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    
    if path != "." && path != "/" && path != "" {
        dirs.push(crate::FileEntry {
            name: "..".to_string(),
            ext: "".to_string(),
            is_dir: true,
            size: 0,
            modified: None,
        });
    }

    let mut lister = op.lister(path).await.map_err(|e| e.to_string())?;
    
    while let Some(entry) = lister.try_next().await.map_err(|e| e.to_string())? {
        let meta = op.stat(entry.path()).await.map_err(|e| e.to_string())?;
        let is_dir = meta.mode().is_dir();
        let size = meta.content_length();
        let modified = None; // Ignoring modification time for opendal for simplicity
        
        let filename = entry.name().to_string();
        // remove trailing slash from dir names if any
        let filename = filename.trim_end_matches('/').to_string();
        
        let (name, ext) = if is_dir {
            (filename.clone(), "".to_string())
        } else {
            let p = std::path::Path::new(&filename);
            let n = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let e = p.extension().unwrap_or_default().to_string_lossy().to_string();
            (n, e)
        };
        
        let entry = crate::FileEntry {
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
    
    let start_idx = if !dirs.is_empty() && dirs[0].name == ".." { 1 } else { 0 };
    dirs[start_idx..].sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    
    dirs.extend(files);
    
    Ok(dirs)
}
