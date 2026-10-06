use std::path::{Path, PathBuf};

use pm_protocol::domain::ItemAttachment;
use sha2::{Digest, Sha256};

use crate::daemon::{Daemon, DaemonError};
use crate::storage::{
    ItemAttachmentContent, ITEM_ATTACHMENT_FILENAME_MAX, ITEM_ATTACHMENT_FILE_MAX,
};

pub const MCP_ATTACHMENT_FETCH_MAX: usize = 1024 * 1024;

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn sanitize_filename(value: &str) -> Result<String, DaemonError> {
    let filename = value.trim();
    let invalid = filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.chars().count() > ITEM_ATTACHMENT_FILENAME_MAX
        || filename
            .chars()
            .any(|character| character.is_control() || character == '/' || character == '\\');
    if invalid {
        return Err(DaemonError::Rejected(format!(
            "attachment filename must be 1-{ITEM_ATTACHMENT_FILENAME_MAX} display characters without paths or control characters"
        )));
    }
    Ok(filename.to_owned())
}

/// MIME declarations are display hints, never execution policy. Active or malformed types are
/// forced to octet-stream; downloads are always served as attachments.
pub fn safe_media_type(value: Option<&str>) -> String {
    let candidate = value.unwrap_or_default().trim().to_ascii_lowercase();
    let syntactically_safe = candidate.len() <= 127
        && candidate.contains('/')
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-/".contains(&byte));
    let active = matches!(
        candidate.as_str(),
        "text/html"
            | "application/xhtml+xml"
            | "image/svg+xml"
            | "application/javascript"
            | "text/javascript"
    );
    if syntactically_safe && !active {
        candidate
    } else {
        "application/octet-stream".into()
    }
}

pub(crate) fn scoped_local_file(
    root: &str,
    requested: &str,
) -> Result<(Vec<u8>, String), DaemonError> {
    let root = std::fs::canonicalize(root).map_err(|error| {
        DaemonError::Rejected(format!("cannot access session working directory: {error}"))
    })?;
    if !root.is_dir() {
        return Err(DaemonError::Rejected(
            "session working directory is not a directory".into(),
        ));
    }
    let requested = Path::new(requested);
    let relative = if requested.is_absolute() {
        requested.strip_prefix(&root).map_err(|_| {
            DaemonError::Rejected("attachment path is outside the session working directory".into())
        })?
    } else {
        requested
    };
    if relative.as_os_str().is_empty() {
        return Err(DaemonError::Rejected(
            "attachment path is not a regular file".into(),
        ));
    }
    let mut checked = PathBuf::from(&root);
    for component in relative.components() {
        match component {
            std::path::Component::Normal(_) => checked.push(component),
            std::path::Component::CurDir => continue,
            _ => {
                return Err(DaemonError::Rejected(
                    "attachment path traversal is not allowed".into(),
                ))
            }
        }
        let metadata = std::fs::symlink_metadata(&checked).map_err(|error| {
            DaemonError::Rejected(format!("cannot inspect attachment path: {error}"))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(DaemonError::Rejected(
                "attachment path must not traverse symlinks".into(),
            ));
        }
    }
    let canonical = std::fs::canonicalize(&checked).map_err(|error| {
        DaemonError::Rejected(format!("cannot access attachment file: {error}"))
    })?;
    if !canonical.starts_with(&root) {
        return Err(DaemonError::Rejected(
            "attachment path is outside the session working directory".into(),
        ));
    }
    let metadata = std::fs::metadata(&canonical).map_err(|error| {
        DaemonError::Rejected(format!("cannot inspect attachment file: {error}"))
    })?;
    if !metadata.is_file() {
        return Err(DaemonError::Rejected(
            "attachment path is not a regular file".into(),
        ));
    }
    if metadata.len() > ITEM_ATTACHMENT_FILE_MAX as u64 {
        return Err(DaemonError::Rejected(format!(
            "attachment exceeds the {}-byte limit",
            ITEM_ATTACHMENT_FILE_MAX
        )));
    }
    let file = std::fs::File::open(&canonical)
        .map_err(|error| DaemonError::Rejected(format!("cannot open attachment file: {error}")))?;
    let mut content = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::read_to_end(
        &mut std::io::Read::take(file, ITEM_ATTACHMENT_FILE_MAX as u64 + 1),
        &mut content,
    )
    .map_err(|error| DaemonError::Rejected(format!("cannot read attachment file: {error}")))?;
    if content.len() > ITEM_ATTACHMENT_FILE_MAX {
        return Err(DaemonError::Rejected(format!(
            "attachment exceeds the {}-byte limit",
            ITEM_ATTACHMENT_FILE_MAX
        )));
    }
    let filename = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".into());
    Ok((content, filename))
}

impl Daemon {
    fn attachment_item_in_bucket(&self, bucket_id: u64, item_id: u64) -> Result<(), DaemonError> {
        self.storage().get_item(bucket_id, item_id)?;
        Ok(())
    }

    pub fn attach_item_bytes(
        &self,
        bucket_id: u64,
        item_id: u64,
        filename: &str,
        media_type: Option<&str>,
        content: &[u8],
        actor_session_id: Option<u64>,
    ) -> Result<ItemAttachment, DaemonError> {
        self.attachment_item_in_bucket(bucket_id, item_id)?;
        let filename = sanitize_filename(filename)?;
        let media_type = safe_media_type(media_type);
        let digest: [u8; 32] = Sha256::digest(content).into();
        Ok(self.storage().create_item_attachment(
            bucket_id,
            item_id,
            &filename,
            &media_type,
            content,
            &digest,
            actor_session_id,
            now_unix_ms(),
        )?)
    }

    pub async fn attach_item_file(
        &self,
        session_id: u64,
        bucket_id: u64,
        item_id: u64,
        path: &str,
        filename_override: Option<&str>,
        media_type: Option<&str>,
    ) -> Result<ItemAttachment, DaemonError> {
        self.attachment_item_in_bucket(bucket_id, item_id)?;
        let session = self.storage().get_session(session_id)?;
        let project = self.storage().get_project(session.project_id)?;
        if project.bucket_id != bucket_id || !session.items_api {
            return Err(DaemonError::Rejected(
                "the session cannot attach files to this item".into(),
            ));
        }
        let (content, source_filename) =
            if session.worker_id == pm_protocol::domain::LOCAL_WORKER_ID {
                scoped_local_file(&session.cwd, path)?
            } else {
                let result = self
                    .worker_file_read(
                        session.worker_id,
                        session.cwd.clone(),
                        path.to_owned(),
                        ITEM_ATTACHMENT_FILE_MAX as u64,
                    )
                    .await?;
                if !result.ok {
                    return Err(DaemonError::Rejected(result.error));
                }
                (result.content, result.filename)
            };
        self.attach_item_bytes(
            bucket_id,
            item_id,
            filename_override.unwrap_or(&source_filename),
            media_type,
            &content,
            Some(session_id),
        )
    }

    pub fn list_item_attachments(
        &self,
        bucket_id: u64,
        item_id: u64,
    ) -> Result<Vec<ItemAttachment>, DaemonError> {
        self.attachment_item_in_bucket(bucket_id, item_id)?;
        Ok(self.storage().list_item_attachments(bucket_id, item_id)?)
    }

    pub fn get_item_attachment(
        &self,
        bucket_id: u64,
        item_id: u64,
        attachment_id: u64,
    ) -> Result<ItemAttachmentContent, DaemonError> {
        self.attachment_item_in_bucket(bucket_id, item_id)?;
        let attachment = self.storage().get_item_attachment(attachment_id)?;
        if attachment.metadata.bucket_id != bucket_id || attachment.metadata.item_id != item_id {
            return Err(DaemonError::Rejected(
                "attachment does not belong to this item".into(),
            ));
        }
        Ok(attachment)
    }

    pub fn delete_item_attachment(
        &self,
        bucket_id: u64,
        item_id: u64,
        attachment_id: u64,
    ) -> Result<ItemAttachment, DaemonError> {
        self.attachment_item_in_bucket(bucket_id, item_id)?;
        Ok(self.storage().delete_item_attachment(
            bucket_id,
            item_id,
            attachment_id,
            None,
            now_unix_ms(),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_and_active_media_are_safe() {
        assert_eq!(
            sanitize_filename(" résumé 版本.txt ").unwrap(),
            "résumé 版本.txt"
        );
        for invalid in ["", ".", "../secret", "folder/file", "bad\nname"] {
            assert!(sanitize_filename(invalid).is_err(), "accepted {invalid:?}");
        }
        assert_eq!(safe_media_type(Some("text/plain")), "text/plain");
        assert_eq!(
            safe_media_type(Some("text/html")),
            "application/octet-stream"
        );
        assert_eq!(safe_media_type(Some("invalid")), "application/octet-stream");
    }

    #[test]
    fn local_reads_stay_in_root_and_reject_symlinks() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.bin"), b"bytes").unwrap();
        assert_eq!(
            scoped_local_file(root.path().to_str().unwrap(), "file.bin")
                .unwrap()
                .0,
            b"bytes"
        );
        assert!(scoped_local_file(root.path().to_str().unwrap(), "../file.bin").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("file.bin"), root.path().join("link"))
                .unwrap();
            assert!(scoped_local_file(root.path().to_str().unwrap(), "link").is_err());
        }
    }
}
