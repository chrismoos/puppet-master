use std::path::{Path, PathBuf};

use pm_protocol::domain::WorkerTranscript;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct WorkerTranscriptStore {
    root: PathBuf,
}

impl WorkerTranscriptStore {
    pub fn new(base: &Path, controller: &str) -> std::io::Result<Self> {
        let controller_hash = hex::encode(Sha256::digest(controller.as_bytes()));
        let root = base.join(controller_hash);
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn persist(&self, terminal_id: u64, generation: u64, data: &[u8]) -> std::io::Result<()> {
        let target = self.path(terminal_id, generation);
        let temporary = self.root.join(format!(
            ".terminal-{terminal_id}-generation-{generation}.upload"
        ));
        let mut file = std::fs::File::create(&temporary)?;
        std::io::Write::write_all(&mut file, data)?;
        file.sync_all()?;
        std::fs::rename(temporary, target)
    }

    pub fn pending(&self) -> std::io::Result<Vec<WorkerTranscript>> {
        let mut pending = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some((terminal_id, generation)) = parse_name(&entry.file_name().to_string_lossy())
            else {
                continue;
            };
            pending.push(WorkerTranscript {
                terminal_id,
                generation,
                size: entry.metadata()?.len(),
            });
        }
        pending.sort_by_key(|item| (item.terminal_id, item.generation));
        Ok(pending)
    }

    pub fn path(&self, terminal_id: u64, generation: u64) -> PathBuf {
        self.root.join(format!(
            "terminal-{terminal_id}-generation-{generation}.bin"
        ))
    }

    pub fn acknowledge(&self, terminal_id: u64, generation: u64) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(terminal_id, generation)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn parse_name(name: &str) -> Option<(u64, u64)> {
    let rest = name.strip_prefix("terminal-")?;
    let (terminal, rest) = rest.split_once("-generation-")?;
    let generation = rest.strip_suffix(".bin")?;
    Some((terminal.parse().ok()?, generation.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_transcripts_survive_reopening_until_acknowledged() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkerTranscriptStore::new(temp.path(), "ws://controller").unwrap();
        store.persist(7, 3, b"transcript").unwrap();

        let reopened = WorkerTranscriptStore::new(temp.path(), "ws://controller").unwrap();
        assert_eq!(
            reopened.pending().unwrap(),
            vec![WorkerTranscript {
                terminal_id: 7,
                generation: 3,
                size: 10,
            }]
        );
        assert_eq!(std::fs::read(reopened.path(7, 3)).unwrap(), b"transcript");
        reopened.acknowledge(7, 3).unwrap();
        assert!(reopened.pending().unwrap().is_empty());
    }

    #[test]
    fn ignores_partial_upload_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkerTranscriptStore::new(temp.path(), "ws://controller").unwrap();
        std::fs::write(
            store.root.join(".terminal-7-generation-3.upload"),
            b"partial",
        )
        .unwrap();
        assert!(store.pending().unwrap().is_empty());
    }
}
