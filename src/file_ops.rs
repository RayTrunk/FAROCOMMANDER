use std::sync::Arc;
use tokio::sync::Mutex;
use russh_sftp::client::SftpSession;
use opendal::Operator;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
pub enum VfsHandle {
    Local,
    Sftp(Arc<Mutex<SftpSession>>),
    Opendal(Operator),
    Zip(String),
}

pub async fn read_file(handle: &VfsHandle, path: &str) -> Result<Vec<u8>, String> {
    match handle {
        VfsHandle::Local => {
            tokio::fs::read(path).await.map_err(|e| format!("Failed to read '{}': {}", path, e))
        }
        VfsHandle::Sftp(session) => {
            let sftp = session.lock().await;
            let mut file = sftp.open(path).await.map_err(|e| format!("SFTP Failed to open '{}': {}", path, e))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).await.map_err(|e| format!("SFTP Failed to read '{}': {}", path, e))?;
            Ok(buf)
        }
        VfsHandle::Opendal(op) => {
            op.read(path).await.map(|b| b.to_vec()).map_err(|e| format!("OpenDAL Failed to read '{}': {}", path, e))
        }
        VfsHandle::Zip(zip_path) => {
            let path_clone = path.to_string();
            let zip_path_clone = zip_path.clone();
            tokio::task::spawn_blocking(move || {
                crate::archive_ops::read_archive_entry(&zip_path_clone, &path_clone)
            }).await.map_err(|e| e.to_string())?
        }
    }
}

pub async fn write_file(handle: &VfsHandle, path: &str, data: Vec<u8>) -> Result<(), String> {
    match handle {
        VfsHandle::Local => {
            tokio::fs::write(path, data).await.map_err(|e| format!("Failed to write '{}': {}", path, e))
        }
        VfsHandle::Sftp(session) => {
            let sftp = session.lock().await;
            let mut file = sftp.open(path).await.map_err(|e| e.to_string())?;
            file.write_all(&data).await.map_err(|e| e.to_string())?;
            Ok(())
        }
        VfsHandle::Opendal(op) => {
            op.write(path, data).await.map(|_| ()).map_err(|e| e.to_string())
        }
        VfsHandle::Zip(_) => {
            Err("Zip writing is not supported yet".to_string())
        }
    }
}

pub async fn delete_path(handle: &VfsHandle, path: &str, is_dir: bool) -> Result<(), String> {
    match handle {
        VfsHandle::Local => {
            if is_dir {
                tokio::fs::remove_dir_all(path).await.map_err(|e| e.to_string())
            } else {
                tokio::fs::remove_file(path).await.map_err(|e| e.to_string())
            }
        }
        VfsHandle::Sftp(session) => {
            let sftp = session.lock().await;
            if is_dir {
                sftp.remove_dir(path).await.map_err(|e| e.to_string())
            } else {
                sftp.remove_file(path).await.map_err(|e| e.to_string())
            }
        }
        VfsHandle::Opendal(op) => {
            if is_dir {
                op.delete_with(path).recursive(true).await.map_err(|e| e.to_string())
            } else {
                op.delete(path).await.map_err(|e| e.to_string())
            }
        }
        VfsHandle::Zip(_) => {
            Err("Zip modifying is not supported yet".to_string())
        }
    }
}

pub async fn create_dir(handle: &VfsHandle, path: &str) -> Result<(), String> {
    match handle {
        VfsHandle::Local => {
            tokio::fs::create_dir_all(path).await.map_err(|e| e.to_string())
        }
        VfsHandle::Sftp(session) => {
            let sftp = session.lock().await;
            sftp.create_dir(path).await.map_err(|e| e.to_string())
        }
        VfsHandle::Opendal(op) => {
            op.create_dir(path).await.map_err(|e| e.to_string())
        }
        VfsHandle::Zip(_) => {
            Err("Zip modifying is not supported yet".to_string())
        }
    }
}

pub async fn copy_single_file_chunked(
    src_handle: &VfsHandle,
    src_path: &str,
    dst_handle: &VfsHandle,
    dst_path: &str,
    tx: &std::sync::mpsc::Sender<(String, f32)>,
    label: &str,
) -> Result<(), String> {
    if let (VfsHandle::Local, VfsHandle::Local) = (src_handle, dst_handle) {
        if let Some(parent) = std::path::Path::new(dst_path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tx.send((label.to_string(), 0.2));
        tokio::fs::copy(src_path, dst_path).await.map_err(|e| e.to_string())?;
        return Ok(());
    }

    // Fallback for remote VFS combinations
    let _ = tx.send((label.to_string(), 0.1));
    let data = read_file(src_handle, src_path).await?;
    let _ = tx.send((label.to_string(), 0.6));
    write_file(dst_handle, dst_path, data).await?;
    Ok(())
}

pub async fn copy_file_with_progress(
    src_handle: &VfsHandle,
    src_path: &str,
    dst_handle: &VfsHandle,
    dst_path: &str,
    tx: std::sync::mpsc::Sender<(String, f32)>,
    file_name: String,
) -> Result<(), String> {
    // Check if source is a local directory -> recursive copy!
    if let VfsHandle::Local = src_handle {
        let sp = std::path::Path::new(src_path);
        if sp.is_dir() {
            let mut entries = Vec::new();
            for entry in walkdir::WalkDir::new(sp).into_iter().filter_map(|e| e.ok()) {
                entries.push(entry);
            }
            let total = entries.len().max(1);
            for (idx, entry) in entries.into_iter().enumerate() {
                let rel = entry.path().strip_prefix(sp).unwrap_or(entry.path());
                let target = if let VfsHandle::Local = dst_handle {
                    std::path::Path::new(dst_path).join(rel).to_string_lossy().into_owned()
                } else {
                    format!("{}/{}", dst_path.trim_end_matches('/'), rel.to_string_lossy().replace('\\', "/"))
                };
                let frac = idx as f32 / total as f32;
                let item_name = entry.file_name().to_string_lossy().into_owned();
                let _ = tx.send((format!("Copying {}/{}", file_name, item_name), frac));
                
                if entry.file_type().is_dir() {
                    let _ = create_dir(dst_handle, &target).await;
                } else if entry.file_type().is_file() {
                    let s = entry.path().to_string_lossy().into_owned();
                    let _ = copy_single_file_chunked(src_handle, &s, dst_handle, &target, &tx, &format!("Copying {}", item_name)).await;
                }
            }
            let _ = tx.send((file_name, 1.0));
            return Ok(());
        }
    }

    let res = copy_single_file_chunked(src_handle, src_path, dst_handle, dst_path, &tx, &format!("Copying {}", file_name)).await;
    let _ = tx.send((file_name, 1.0));
    res
}

pub async fn move_file(
    src_handle: &VfsHandle,
    src_path: &str,
    dst_handle: &VfsHandle,
    dst_path: &str,
    tx: std::sync::mpsc::Sender<(String, f32)>,
    file_name: String,
) -> Result<(), String> {
    let _ = tx.send((format!("Moving {}", file_name), 0.0));
    // Fast path: Local to Local rename (works instantly for files and directories on the same drive)
    if let (VfsHandle::Local, VfsHandle::Local) = (src_handle, dst_handle) {
        if tokio::fs::rename(src_path, dst_path).await.is_ok() {
            let _ = tx.send((format!("Moving {}", file_name), 1.0));
            return Ok(());
        }
    }
    let is_dir = if let VfsHandle::Local = src_handle {
        std::path::Path::new(src_path).is_dir()
    } else {
        false
    };
    copy_file_with_progress(src_handle, src_path, dst_handle, dst_path, tx.clone(), file_name.clone()).await?;
    delete_path(src_handle, src_path, is_dir).await?;
    let _ = tx.send((format!("Moving {}", file_name), 1.0));
    Ok(())
}
