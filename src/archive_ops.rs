use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArchiveFormat {
    Zip,
    SevenZ,
    TarGz,
    TarBz2,
    Tar,
    Gz,
    Bz2,
}

impl ArchiveFormat {
    pub fn label(&self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "ZIP (.zip)",
            ArchiveFormat::SevenZ => "7-Zip (.7z)",
            ArchiveFormat::TarGz => "TAR.GZ (.tar.gz)",
            ArchiveFormat::TarBz2 => "TAR.BZ2 (.tar.bz2)",
            ArchiveFormat::Tar => "TAR (.tar)",
            ArchiveFormat::Gz => "GZip (.gz)",
            ArchiveFormat::Bz2 => "BZip2 (.bz2)",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::SevenZ => "7z",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::TarBz2 => "tar.bz2",
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::Gz => "gz",
            ArchiveFormat::Bz2 => "bz2",
        }
    }

    pub fn replace_extension(path: &str, new_fmt: ArchiveFormat) -> String {
        let lower = path.to_lowercase();
        let known_exts = [
            ".tar.gz", ".tar.bz2", ".tgz", ".tbz2", ".zip", ".7z", ".tar", ".gz", ".bz2", ".rar",
        ];
        for ext in known_exts {
            if lower.ends_with(ext) {
                let prefix = &path[..path.len() - ext.len()];
                return format!("{}.{}", prefix, new_fmt.extension());
            }
        }
        format!("{}.{}", path, new_fmt.extension())
    }
}

pub fn is_archive_filename(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.ends_with(".zip")
        || lower.ends_with(".7z")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tbz2")
        || lower.ends_with(".tar")
        || lower.ends_with(".gz")
        || lower.ends_with(".bz2")
        || lower.ends_with(".rar")
}

fn is_tar_stream(data: &[u8]) -> bool {
    data.len() >= 512 && &data[257..262] == b"ustar"
}

fn open_tar_reader(archive_path: &str, lower: &str) -> Result<Box<dyn Read>, String> {
    let file = File::open(archive_path).map_err(|e| e.to_string())?;
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Ok(Box::new(flate2::read::GzDecoder::new(file)))
    } else if lower.ends_with(".tar.bz2") || lower.ends_with(".tbz2") {
        Ok(Box::new(bzip2::read::BzDecoder::new(file)))
    } else {
        Ok(Box::new(file))
    }
}

fn decompress_stream_bytes(archive_path: &str, is_gz: bool) -> Result<Vec<u8>, String> {
    let file = File::open(archive_path).map_err(|e| e.to_string())?;
    let mut reader: Box<dyn Read> = if is_gz {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(bzip2::read::BzDecoder::new(file))
    };
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

struct RawArchiveItem {
    path: String,
    is_dir: bool,
    size: u64,
}

fn collect_tar_items<R: Read>(archive: &mut tar::Archive<R>) -> Vec<RawArchiveItem> {
    let mut items = Vec::new();
    if let Ok(entries) = archive.entries() {
        for entry in entries.flatten() {
            let norm = entry
                .path()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let is_dir = entry.header().entry_type().is_dir() || norm.ends_with('/');
            let size = entry.size();
            items.push(RawArchiveItem {
                path: norm,
                is_dir,
                size,
            });
        }
    }
    items
}

fn list_raw_archive_items(archive_path: &str) -> Result<Vec<RawArchiveItem>, String> {
    let lower = archive_path.to_lowercase();
    let mut items = Vec::new();

    if lower.ends_with(".zip") {
        let file = File::open(archive_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..archive.len() {
            if let Ok(zf) = archive.by_index(i) {
                if let Ok(name) = zf.name() {
                    let norm = name.replace('\\', "/");
                    let is_dir = norm.ends_with('/') || zf.is_dir();
                    items.push(RawArchiveItem {
                        path: norm,
                        is_dir,
                        size: zf.size(),
                    });
                }
            }
        }
    } else if lower.ends_with(".7z") {
        let reader = sevenz_rust::SevenZReader::open(archive_path, sevenz_rust::Password::empty())
            .map_err(|e| e.to_string())?;
        for entry in &reader.archive().files {
            let norm = entry.name().replace('\\', "/");
            items.push(RawArchiveItem {
                path: norm,
                is_dir: entry.is_directory(),
                size: entry.size(),
            });
        }
    } else if lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tbz2")
        || lower.ends_with(".tar")
    {
        let mut archive = tar::Archive::new(open_tar_reader(archive_path, &lower)?);
        items = collect_tar_items(&mut archive);
    } else if lower.ends_with(".gz") || lower.ends_with(".bz2") {
        let decompressed = decompress_stream_bytes(archive_path, lower.ends_with(".gz")).unwrap_or_default();
        if is_tar_stream(&decompressed) {
            let mut archive = tar::Archive::new(Cursor::new(decompressed));
            items = collect_tar_items(&mut archive);
        } else {
            let stem = Path::new(archive_path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            items.push(RawArchiveItem {
                path: stem,
                is_dir: false,
                size: decompressed.len() as u64,
            });
        }
    } else if lower.ends_with(".rar") {
        if let Ok(out) = std::process::Command::new("tar.exe")
            .args(["-tf", archive_path])
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let norm = line.trim().replace('\\', "/");
                    if !norm.is_empty() {
                        let is_dir = norm.ends_with('/');
                        items.push(RawArchiveItem {
                            path: norm,
                            is_dir,
                            size: 0,
                        });
                    }
                }
            }
        }
    }

    Ok(items)
}

pub fn list_archive_dir(
    archive_path: &str,
    current_subpath: &str,
) -> Result<Vec<crate::FileEntry>, String> {
    let raw_items = list_raw_archive_items(archive_path)?;
    let mut results = Vec::new();
    let mut dirs_found = std::collections::HashSet::new();
    let mut files_found = std::collections::HashSet::new();

    let clean_sub = current_subpath.trim_matches(|c| c == '/' || c == '\\').replace('\\', "/");
    let prefix = if clean_sub.is_empty() {
        "".to_string()
    } else {
        format!("{}/", clean_sub)
    };

    for item in raw_items {
        let name = item.path.trim_start_matches("./");
        if name.starts_with(&prefix) {
            let remainder = &name[prefix.len()..];
            if remainder.is_empty() {
                continue;
            }
            let is_dir = remainder.ends_with('/') || item.is_dir;
            let mut split = remainder.trim_end_matches('/').split('/');
            let first = split.next().unwrap_or("");
            if first.is_empty() {
                continue;
            }
            let has_more = split.next().is_some();

            if has_more || is_dir {
                dirs_found.insert(first.to_string());
            } else if files_found.insert(first.to_string()) {
                let file_stem = Path::new(first)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let ext = Path::new(first)
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                results.push(crate::FileEntry {
                    name: file_stem,
                    ext,
                    is_dir: false,
                    size: item.size,
                    modified: None,
                });
            }
        }
    }

    let mut dir_entries = Vec::new();
    for d in dirs_found {
        let d = d.trim_end_matches('/').to_string();
        if !d.is_empty() {
            dir_entries.push(crate::FileEntry {
                name: d,
                ext: "".to_string(),
                is_dir: true,
                size: 0,
                modified: None,
            });
        }
    }
    dir_entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    results.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let mut all = Vec::new();
    all.push(crate::FileEntry {
        name: "..".to_string(),
        ext: "".to_string(),
        is_dir: true,
        size: 0,
        modified: None,
    });
    all.extend(dir_entries);
    all.extend(results);
    Ok(all)
}

fn read_from_tar<R: Read>(archive: &mut tar::Archive<R>, target: &str) -> Result<Vec<u8>, String> {
    for mut entry in archive.entries().map_err(|e| e.to_string())?.flatten() {
        let norm = entry
            .path()
            .map(|p| p.to_string_lossy().trim_start_matches("./").replace('\\', "/"))
            .unwrap_or_default();
        if norm == target {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            return Ok(buf);
        }
    }
    Err(format!("Eintrag '{}' im Archiv nicht gefunden", target))
}

pub fn read_archive_entry(archive_path: &str, entry_path: &str) -> Result<Vec<u8>, String> {
    let lower = archive_path.to_lowercase();
    let target = entry_path.trim_matches(|c| c == '/' || c == '\\').replace('\\', "/");

    if lower.ends_with(".zip") {
        let file = File::open(archive_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let mut zf = archive.by_name(&target).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        zf.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        return Ok(buf);
    } else if lower.ends_with(".7z") {
        let mut reader =
            sevenz_rust::SevenZReader::open(archive_path, sevenz_rust::Password::empty())
                .map_err(|e| e.to_string())?;
        let mut found_bytes: Option<Vec<u8>> = None;
        let _ = reader.for_each_entries(|entry, r| {
            let norm = entry.name().trim_matches('/').replace('\\', "/");
            if norm == target {
                let mut buf = Vec::new();
                let _ = r.read_to_end(&mut buf);
                found_bytes = Some(buf);
                Ok(false)
            } else {
                std::io::copy(r, &mut std::io::sink())?;
                Ok(true)
            }
        });
        return found_bytes.ok_or_else(|| format!("Eintrag '{}' in 7z nicht gefunden", target));
    } else if lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tbz2")
        || lower.ends_with(".tar")
    {
        let mut archive = tar::Archive::new(open_tar_reader(archive_path, &lower)?);
        return read_from_tar(&mut archive, &target);
    } else if lower.ends_with(".gz") || lower.ends_with(".bz2") {
        let buf = decompress_stream_bytes(archive_path, lower.ends_with(".gz"))?;
        if is_tar_stream(&buf) {
            let mut archive = tar::Archive::new(Cursor::new(buf));
            return read_from_tar(&mut archive, &target);
        }
        return Ok(buf);
    } else if lower.ends_with(".rar") {
        let out = std::process::Command::new("tar.exe")
            .args(["-xOf", archive_path, &target])
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        return Err("RAR-Eintrag konnte nicht gelesen werden".to_string());
    }

    Err("Nicht unterstütztes Archivformat".to_string())
}

pub fn extract_archive(
    archive_path: &str,
    dest_dir: &str,
    tx: crossbeam_channel::Sender<(String, f32)>,
) -> Result<(), String> {
    let res = extract_archive_inner(archive_path, dest_dir, &tx);
    let _ = tx.send(("Fertig".to_string(), 1.0));
    res
}

fn extract_archive_inner(
    archive_path: &str,
    dest_dir: &str,
    tx: &crossbeam_channel::Sender<(String, f32)>,
) -> Result<(), String> {
    let lower = archive_path.to_lowercase();
    let archive_name = Path::new(archive_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("Zielordner-Fehler: {}", e))?;
    let _ = tx.send((format!("Entpacke {}...", archive_name), 0.05));

    if lower.ends_with(".zip") {
        let file = File::open(archive_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let total = archive.len().max(1);
        for i in 0..archive.len() {
            if let Ok(mut zf) = archive.by_index(i) {
                let rel_name = match zf.enclosed_name() {
                    Some(p) => p.to_path_buf(),
                    None => continue,
                };
                let out_path = Path::new(dest_dir).join(&rel_name);
                let frac = (i as f32 / total as f32).clamp(0.05, 0.99);
                let _ = tx.send((
                    format!("Entpacke {}", rel_name.to_string_lossy()),
                    frac,
                ));

                if zf.is_dir() {
                    let _ = std::fs::create_dir_all(&out_path);
                } else {
                    if let Some(p) = out_path.parent() {
                        let _ = std::fs::create_dir_all(p);
                    }
                    let mut out_file = File::create(&out_path).map_err(|e| e.to_string())?;
                    std::io::copy(&mut zf, &mut out_file).map_err(|e| e.to_string())?;
                }
            }
        }
    } else if lower.ends_with(".7z") {
        let mut reader =
            sevenz_rust::SevenZReader::open(archive_path, sevenz_rust::Password::empty())
                .map_err(|e| e.to_string())?;
        let total = reader.archive().files.len().max(1);
        let mut idx = 0usize;
        let dest_base = Path::new(dest_dir).to_path_buf();
        reader
            .for_each_entries(|entry, r| {
                idx += 1;
                let frac = (idx as f32 / total as f32).clamp(0.05, 0.99);
                let name = entry.name().replace('\\', "/");
                let _ = tx.send((format!("Entpacke {}", name), frac));
                let out_path = dest_base.join(&name);
                if entry.is_directory() {
                    let _ = std::fs::create_dir_all(&out_path);
                } else {
                    if let Some(p) = out_path.parent() {
                        let _ = std::fs::create_dir_all(p);
                    }
                    let mut out_file = File::create(&out_path)?;
                    std::io::copy(r, &mut out_file)?;
                }
                Ok(true)
            })
            .map_err(|e| e.to_string())?;
    } else if lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tbz2")
        || lower.ends_with(".tar")
    {
        let mut archive = tar::Archive::new(open_tar_reader(archive_path, &lower)?);
        let _ = tx.send((format!("Entpacke {}...", archive_name), 0.3));
        archive.unpack(dest_dir).map_err(|e| e.to_string())?;
    } else if lower.ends_with(".gz") || lower.ends_with(".bz2") {
        let decompressed = decompress_stream_bytes(archive_path, lower.ends_with(".gz"))?;
        if is_tar_stream(&decompressed) {
            let mut archive = tar::Archive::new(Cursor::new(decompressed));
            archive.unpack(dest_dir).map_err(|e| e.to_string())?;
        } else {
            let stem = Path::new(archive_path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let out_path = Path::new(dest_dir).join(stem);
            std::fs::write(out_path, decompressed).map_err(|e| e.to_string())?;
        }
    } else if lower.ends_with(".rar") {
        let _ = tx.send((format!("Entpacke RAR {}...", archive_name), 0.3));
        let status = std::process::Command::new("tar.exe")
            .args(["-xf", archive_path, "-C", dest_dir])
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("RAR-Entpacken fehlgeschlagen".to_string());
        }
    }

    Ok(())
}

fn append_items_to_tar<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    all_items: &[(std::path::PathBuf, String, bool)],
    tx: &crossbeam_channel::Sender<(String, f32)>,
    fmt_name: &str,
) -> Result<(), String> {
    let total = all_items.len().max(1);
    for (idx, (fs_path, rel_path, is_dir)) in all_items.iter().enumerate() {
        let frac = (idx as f32 / total as f32).clamp(0.02, 0.98);
        let _ = tx.send((format!("Packe ({}): {}", fmt_name, rel_path), frac));
        let clean_rel = rel_path.trim_matches('/').replace('\\', "/");
        if clean_rel.is_empty() {
            continue;
        }
        if *is_dir {
            tar.append_dir(&clean_rel, fs_path).map_err(|e| e.to_string())?;
        } else {
            let mut f = File::open(fs_path).map_err(|e| e.to_string())?;
            tar.append_file(&clean_rel, &mut f).map_err(|e| e.to_string())?;
        }
    }
    tar.finish().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn pack_archive(
    sources: Vec<(String, String, bool)>,
    target_archive: &str,
    format: ArchiveFormat,
    tx: crossbeam_channel::Sender<(String, f32)>,
) -> Result<(), String> {
    let res = pack_archive_inner(sources, target_archive, format, &tx);
    let _ = tx.send(("Archiv erstellt".to_string(), 1.0));
    res
}

fn pack_archive_inner(
    sources: Vec<(String, String, bool)>,
    target_archive: &str,
    format: ArchiveFormat,
    tx: &crossbeam_channel::Sender<(String, f32)>,
) -> Result<(), String> {
    if let Some(parent) = Path::new(target_archive).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    // Collect all files and directories with their relative archive path
    let mut all_items: Vec<(std::path::PathBuf, String, bool)> = Vec::new();
    for (src_path, name, is_dir) in &sources {
        let sp = Path::new(src_path);
        if *is_dir {
            for entry in walkdir::WalkDir::new(sp).into_iter().filter_map(|e| e.ok()) {
                let rel = entry.path().strip_prefix(sp).unwrap_or(entry.path());
                let rel_str = if rel.as_os_str().is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", name, rel.to_string_lossy().replace('\\', "/"))
                };
                all_items.push((
                    entry.path().to_path_buf(),
                    rel_str,
                    entry.file_type().is_dir(),
                ));
            }
        } else {
            all_items.push((sp.to_path_buf(), name.clone(), false));
        }
    }

    let total = all_items.len().max(1);
    let _ = tx.send((format!("Erstelle {}...", target_archive), 0.02));

    match format {
        ArchiveFormat::Zip => {
            let file = File::create(target_archive)
                .map_err(|e| format!("Konnte '{}' nicht erstellen: {}", target_archive, e))?;
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            for (idx, (fs_path, rel_path, is_dir)) in all_items.iter().enumerate() {
                let frac = (idx as f32 / total as f32).clamp(0.02, 0.98);
                let _ = tx.send((format!("Packe (ZIP): {}", rel_path), frac));
                if *is_dir {
                    let dir_name = if rel_path.ends_with('/') {
                        rel_path.clone()
                    } else {
                        format!("{}/", rel_path)
                    };
                    let _ = zip.add_directory(dir_name, options);
                } else {
                    zip.start_file(rel_path, options).map_err(|e| e.to_string())?;
                    let mut f = File::open(fs_path).map_err(|e| e.to_string())?;
                    std::io::copy(&mut f, &mut zip).map_err(|e| e.to_string())?;
                }
            }
            zip.finish().map_err(|e| e.to_string())?;
        }
        ArchiveFormat::SevenZ => {
            let mut sz = sevenz_rust::SevenZWriter::create(target_archive)
                .map_err(|e| format!("Konnte '{}' nicht erstellen: {}", target_archive, e))?;
            for (idx, (fs_path, rel_path, is_dir)) in all_items.iter().enumerate() {
                let frac = (idx as f32 / total as f32).clamp(0.02, 0.98);
                let _ = tx.send((format!("Packe (7z): {}", rel_path), frac));
                let entry = sevenz_rust::SevenZArchiveEntry::from_path(fs_path, rel_path.clone());
                if *is_dir {
                    sz.push_archive_entry::<std::io::Empty>(entry, None)
                        .map_err(|e| e.to_string())?;
                } else {
                    let f = File::open(fs_path).map_err(|e| e.to_string())?;
                    sz.push_archive_entry(entry, Some(f))
                        .map_err(|e| e.to_string())?;
                }
            }
            sz.finish().map_err(|e| e.to_string())?;
        }
        ArchiveFormat::TarGz => {
            let file = File::create(target_archive)
                .map_err(|e| format!("Konnte '{}' nicht erstellen: {}", target_archive, e))?;
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            append_items_to_tar(&mut tar, &all_items, tx, "TAR.GZ")?;
            let enc = tar.into_inner().map_err(|e| e.to_string())?;
            enc.finish().map_err(|e| e.to_string())?;
        }
        ArchiveFormat::TarBz2 => {
            let file = File::create(target_archive)
                .map_err(|e| format!("Konnte '{}' nicht erstellen: {}", target_archive, e))?;
            let enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            append_items_to_tar(&mut tar, &all_items, tx, "TAR.BZ2")?;
            let enc = tar.into_inner().map_err(|e| e.to_string())?;
            enc.finish().map_err(|e| e.to_string())?;
        }
        ArchiveFormat::Tar => {
            let file = File::create(target_archive)
                .map_err(|e| format!("Konnte '{}' nicht erstellen: {}", target_archive, e))?;
            let mut tar = tar::Builder::new(file);
            append_items_to_tar(&mut tar, &all_items, tx, "TAR")?;
        }
        ArchiveFormat::Gz => {
            let file = File::create(target_archive)
                .map_err(|e| format!("Konnte '{}' nicht erstellen: {}", target_archive, e))?;
            let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            if all_items.len() == 1 && !all_items[0].2 {
                let (first_path, rel_path, _) = &all_items[0];
                let _ = tx.send((format!("Packe (GZ): {}", rel_path), 0.3));
                let mut src = File::open(first_path).map_err(|e| e.to_string())?;
                std::io::copy(&mut src, &mut enc).map_err(|e| e.to_string())?;
                enc.finish().map_err(|e| e.to_string())?;
            } else {
                let mut tar = tar::Builder::new(enc);
                append_items_to_tar(&mut tar, &all_items, tx, "GZ")?;
                let enc = tar.into_inner().map_err(|e| e.to_string())?;
                enc.finish().map_err(|e| e.to_string())?;
            }
        }
        ArchiveFormat::Bz2 => {
            let file = File::create(target_archive)
                .map_err(|e| format!("Konnte '{}' nicht erstellen: {}", target_archive, e))?;
            let mut enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
            if all_items.len() == 1 && !all_items[0].2 {
                let (first_path, rel_path, _) = &all_items[0];
                let _ = tx.send((format!("Packe (BZ2): {}", rel_path), 0.3));
                let mut src = File::open(first_path).map_err(|e| e.to_string())?;
                std::io::copy(&mut src, &mut enc).map_err(|e| e.to_string())?;
                enc.finish().map_err(|e| e.to_string())?;
            } else {
                let mut tar = tar::Builder::new(enc);
                append_items_to_tar(&mut tar, &all_items, tx, "BZ2")?;
                let enc = tar.into_inner().map_err(|e| e.to_string())?;
                enc.finish().map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(())
}
