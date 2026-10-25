use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::{Error, PageOptions, Result};

#[derive(Debug, Clone)]
pub(crate) struct Checkpoint {
    pub(crate) path: PathBuf,
    access: Arc<Mutex<()>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    page: usize,
    text: String,
}

impl Checkpoint {
    /// Opens or replaces a checkpoint on the producer's blocking thread.
    pub(crate) fn open(
        directory: &Path,
        input: &Path,
        options: &PageOptions,
        label: &str,
        pages: usize,
        access: &Arc<Mutex<()>>,
    ) -> Result<(Self, BTreeMap<usize, String>)> {
        // Repeated inputs can open this file while another occurrence appends a record.
        let _guard = access
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let context =
            |source| Error::io(format!("reading metadata for {}", input.display()), source);
        let canonical = dunce::canonicalize(input).map_err(context)?;
        let metadata = std::fs::metadata(&canonical).map_err(context)?;
        let modified = metadata.modified().map_err(context)?;
        let mut hash = Sha256::new();
        hash.update(b"tahweel-checkpoint-v1\0");
        let path_bytes = canonical.as_os_str().as_encoded_bytes();
        hash.update(path_bytes.len().to_le_bytes());
        hash.update(path_bytes);
        hash.update(metadata.len().to_le_bytes());
        match modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => {
                hash.update([0]);
                hash.update(duration.as_nanos().to_le_bytes());
            }
            Err(error) => {
                hash.update([1]);
                hash.update(error.duration().as_nanos().to_le_bytes());
            }
        }
        let options = serde_json::to_vec(options).map_err(|source| Error::Checkpoint {
            context: "encoding checkpoint settings".into(),
            source: source.into(),
        })?;
        hash.update(options.len().to_le_bytes());
        hash.update(options);
        hash.update(label.as_bytes());
        let mut key = String::with_capacity(64);
        for byte in hash.finalize() {
            // String's fmt::Write implementation never returns an error.
            write!(&mut key, "{byte:02x}").expect("formatting into a String cannot fail");
        }
        let path = directory.join(format!("{key}.jsonl"));
        std::fs::create_dir_all(directory)
            .map_err(|source| Error::io(format!("creating {}", directory.display()), source))?;
        let loaded = match std::fs::File::open(&path) {
            Ok(file) => load(BufReader::new(file), pages),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(Error::io(
                    format!("reading checkpoint {}", path.display()),
                    source,
                ));
            }
        };
        let texts = if let Some(texts) = loaded {
            texts
        } else {
            std::fs::File::create(&path).map_err(|source| {
                Error::io(format!("creating checkpoint {}", path.display()), source)
            })?;
            BTreeMap::new()
        };
        Ok((
            Self {
                path,
                access: access.clone(),
            },
            texts,
        ))
    }

    pub(crate) async fn append(&self, page: usize, text: &str) -> Result<()> {
        #[derive(serde::Serialize)]
        struct Saved<'a> {
            page: usize,
            text: &'a str,
        }
        let mut record =
            serde_json::to_vec(&Saved { page, text }).map_err(|source| Error::Checkpoint {
                context: "encoding checkpoint page".into(),
                source: source.into(),
            })?;
        record.push(b'\n');
        let checkpoint = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = checkpoint
                .access
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let write = || -> std::io::Result<()> {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&checkpoint.path)?;
                file.write_all(&record)?;
                file.flush()
            };
            write().map_err(|source| {
                Error::io(
                    format!(
                        "writing page {page} to checkpoint {}",
                        checkpoint.path.display()
                    ),
                    source,
                )
            })
        })
        .await
        .map_err(|source| Error::Task {
            context: "appending a checkpoint page".into(),
            source,
        })?
    }

    pub(crate) async fn remove(&self) -> Result<()> {
        let checkpoint = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = checkpoint
                .access
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match std::fs::remove_file(&checkpoint.path) {
                Ok(()) => Ok(()),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(Error::io(
                    format!("removing checkpoint {}", checkpoint.path.display()),
                    source,
                )),
            }
        })
        .await
        .map_err(|source| Error::Task {
            context: "removing a checkpoint".into(),
            source,
        })?
    }
}

fn load(reader: impl BufRead, pages: usize) -> Option<BTreeMap<usize, String>> {
    let mut loaded = BTreeMap::new();
    for line in reader.lines() {
        let record: Record = serde_json::from_str(&line.ok()?).ok()?;
        if record.page == 0 || record.page > pages {
            return None;
        }
        // Repeated inputs can share a checkpoint; the latest recognized text wins.
        loaded.insert(record.page, record.text);
    }
    Some(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_inputs_wait_for_a_complete_record_before_loading() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input.png");
        std::fs::write(&input, b"source").unwrap();
        let directory = root.path().join("cp");
        let options = PageOptions::default();
        let access = Arc::default();
        let (checkpoint, _) =
            Checkpoint::open(&directory, &input, &options, "fake", 1, &access).unwrap();
        let guard = access.lock().unwrap();
        // Model a record whose write has not finished yet. Opening another occurrence must
        // wait for it, rather than treating the partial record as a corrupt checkpoint.
        std::fs::write(&checkpoint.path, b"{\"page\":1,\"text\":").unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let shared = access.clone();
        let reader = std::thread::spawn(move || {
            let result = Checkpoint::open(&directory, &input, &options, "fake", 1, &shared);
            send.send(result).unwrap();
        });
        assert!(matches!(
            receive.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        std::fs::write(&checkpoint.path, b"{\"page\":1,\"text\":\"complete\"}\n").unwrap();
        drop(guard);
        let (_, texts) = receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(texts[&1], "complete");
        reader.join().unwrap();
    }

    #[tokio::test]
    async fn keys_cover_path_size_mtime_settings_and_label() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input.png");
        std::fs::write(&input, b"source bytes").unwrap();
        let checkpoints = root.path().join("cp");
        let options = PageOptions::default();
        let access = Arc::default();
        let (original, _) =
            Checkpoint::open(&checkpoints, &input, &options, "drive:ar", 2, &access).unwrap();
        original.append(2, "second\npage").await.unwrap();
        original.append(1, "first").await.unwrap();
        let (same, texts) =
            Checkpoint::open(&checkpoints, &input, &options, "drive:ar", 2, &access).unwrap();
        assert_eq!(original.path, same.path);
        assert_eq!(texts[&1], "first");
        assert_eq!(texts[&2], "second\npage");
        let (other_language, texts) =
            Checkpoint::open(&checkpoints, &input, &options, "drive:en", 2, &access).unwrap();
        assert_ne!(original.path, other_language.path);
        assert_eq!(texts, BTreeMap::new());
        let mut changed = options.clone();
        changed.dpi += 1;
        assert_ne!(
            original.path,
            Checkpoint::open(&checkpoints, &input, &changed, "drive:ar", 2, &access)
                .unwrap()
                .0
                .path
        );
        let copy = root.path().join("another.png");
        std::fs::copy(&input, &copy).unwrap();
        assert_ne!(
            original.path,
            Checkpoint::open(&checkpoints, &copy, &options, "drive:ar", 2, &access)
                .unwrap()
                .0
                .path
        );
        let file = std::fs::File::options().write(true).open(&input).unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1)),
        )
        .unwrap();
        let timestamp_key =
            Checkpoint::open(&checkpoints, &input, &options, "drive:ar", 2, &access)
                .unwrap()
                .0
                .path;
        assert_ne!(original.path, timestamp_key);
        std::fs::write(&input, b"new longer source bytes").unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1)),
        )
        .unwrap();
        assert_ne!(
            timestamp_key,
            Checkpoint::open(&checkpoints, &input, &options, "drive:ar", 2, &access)
                .unwrap()
                .0
                .path
        );
    }

    #[tokio::test]
    async fn invalid_records_reset_the_entire_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input.png");
        std::fs::write(&input, b"source").unwrap();
        let options = PageOptions::default();
        let access = Arc::default();
        let directory = root.path().join("cp");
        let (checkpoint, _) =
            Checkpoint::open(&directory, &input, &options, "fake", 2, &access).unwrap();
        for invalid in [
            "{\"page\":0,\"text\":\"bad\"}\n",
            "{\"page\":3,\"text\":\"bad\"}\n",
            "{\"page\":1,\"text\":\"ok\"}\n{incomplete",
            "{\"page\":1,\"text\":\"ok\",\"unexpected\":true}\n",
        ] {
            std::fs::write(&checkpoint.path, invalid).unwrap();
            let (_, texts) =
                Checkpoint::open(&directory, &input, &options, "fake", 2, &access).unwrap();
            assert_eq!(texts, BTreeMap::new());
            assert_eq!(std::fs::read(&checkpoint.path).unwrap(), Vec::<u8>::new());
        }
        checkpoint.remove().await.unwrap();
        checkpoint.remove().await.unwrap();
    }
}
