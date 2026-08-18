#[derive(Deserialize, Serialize)]
struct BankCache {
    schema: u8,
    root: PathBuf,
    fingerprint: u64,
    payload: ScanPayload,
}

#[derive(Serialize)]
struct BankCacheRef<'a> {
    schema: u8,
    root: &'a Path,
    fingerprint: u64,
    payload: &'a ScanPayload,
}

fn bank_cache_path(config: &Config, root: &Path) -> PathBuf {
    let mut hash = DefaultHasher::new();
    root.hash(&mut hash);
    config
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("bank-{:016x}.msgpack", hash.finish()))
}

fn bank_tree_fingerprint(root: &Path) -> Option<u64> {
    let metadata = fs::symlink_metadata(root).ok()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    let mut directories = vec![(root.to_path_buf(), PathBuf::new(), 0_usize)];
    let mut scripts = 0_usize;
    while let Some((directory, relative, depth)) = directories.pop() {
        let mut entries: Vec<_> = fs::read_dir(&directory)
            .ok()?
            .collect::<io::Result<_>>()
            .ok()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let file_type = entry.file_type().ok()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let child = relative.join(entry.file_name());
            if file_type.is_dir() {
                if depth < MAX_DEPTH && !ignored_directory(&entry.file_name().to_string_lossy()) {
                    directories.push((path, child, depth + 1));
                }
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("sbatch")
            {
                continue;
            }
            let metadata = entry.metadata().ok()?;
            child.hash(&mut hasher);
            metadata.len().hash(&mut hasher);
            metadata.dev().hash(&mut hasher);
            metadata.ino().hash(&mut hasher);
            metadata.mtime().hash(&mut hasher);
            metadata.mtime_nsec().hash(&mut hasher);
            metadata.ctime().hash(&mut hasher);
            metadata.ctime_nsec().hash(&mut hasher);
            scripts += 1;
            if scripts > MAX_SCRIPTS {
                return Some(hasher.finish());
            }
        }
    }
    Some(hasher.finish())
}

fn load_bank_cache(config: &Config, root: &Path) -> Option<(ScanPayload, u64)> {
    let path = bank_cache_path(config, root);
    let metadata = fs::metadata(&path).ok()?;
    if metadata.len() > MAX_BANK_CACHE_BYTES
        || SystemTime::now()
            .duration_since(metadata.modified().ok()?)
            .ok()?
            > BANK_CACHE_TTL
    {
        return None;
    }
    let reader = BufReader::with_capacity(256 * 1024, fs::File::open(path).ok()?);
    let cache: BankCache = rmp_serde::from_read(reader).ok()?;
    let fingerprint = cache.fingerprint;
    (cache.schema == BANK_CACHE_SCHEMA
        && cache.root == root
        && Some(fingerprint) == bank_tree_fingerprint(root))
    .then_some((cache.payload, fingerprint))
}

fn store_bank_cache(config: &Config, root: &Path, payload: &ScanPayload) {
    if payload.error.is_some() {
        return;
    }
    let Some(fingerprint) = bank_tree_fingerprint(root) else {
        return;
    };
    let path = bank_cache_path(config, root);
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|file| {
            let mut writer = BufWriter::with_capacity(256 * 1024, file);
            rmp_serde::encode::write(
                &mut writer,
                &BankCacheRef {
                    schema: BANK_CACHE_SCHEMA,
                    root,
                    fingerprint,
                    payload,
                },
            )
            .map_err(io::Error::other)?;
            writer.flush()
        });
    if result.is_ok() {
        let _ = fs::rename(&temporary, path);
    } else {
        let _ = fs::remove_file(temporary);
    }
}
