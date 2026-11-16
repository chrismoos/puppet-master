use bytes::Bytes;
use serde::Serialize;
use std::{
    collections::HashMap,
    ops::Range,
    time::{Duration, Instant},
};

const ENVELOPE_BYTES: usize = 10;
const RECORD_HEADER_BYTES: usize = 10;
const TAG_PAGE: u16 = 3;
const TAG_READY: u16 = 5;
const TAG_FINISH: u16 = 6;
const TAG_IMAGE_PAYLOAD: u16 = 9;
const PAGE_ROWS_OFFSET: usize = 2;
const PAGE_HEADER_BYTES: usize = 20;
const CACHE_BYTES: usize = 128 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(120);
const PAGE_BATCH_ROWS: usize = 200;

#[derive(Debug)]
pub(crate) struct SnapshotIndex {
    ready: usize,
    steps: Vec<(Range<usize>, usize)>,
}

impl SnapshotIndex {
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..ENVELOPE_BYTES)? != b"GHOSTSNP\x01\x00" {
            return None;
        }
        let mut offset = ENVELOPE_BYTES;
        let mut ready = None;
        let mut start = 0;
        let mut steps = Vec::new();
        let mut finished = false;
        while offset < bytes.len() {
            let header = bytes.get(offset..offset.checked_add(RECORD_HEADER_BYTES)?)?;
            let tag = u16::from_le_bytes(header[..2].try_into().ok()?);
            let length = u32::from_le_bytes(header[2..6].try_into().ok()?) as usize;
            let payload = offset.checked_add(RECORD_HEADER_BYTES)?;
            let end = payload.checked_add(length)?;
            let body = bytes.get(payload..end)?;
            if finished {
                return None;
            }
            if tag == TAG_READY {
                if ready.is_some() {
                    return None;
                }
                ready = Some(end);
                start = end;
            } else if ready.is_some() && (tag == TAG_PAGE || tag == TAG_IMAGE_PAYLOAD) {
                let rows = if tag == TAG_PAGE {
                    if body.len() < PAGE_HEADER_BYTES {
                        return None;
                    }
                    usize::from(u16::from_le_bytes(
                        body[PAGE_ROWS_OFFSET..PAGE_ROWS_OFFSET + 2]
                            .try_into()
                            .ok()?,
                    ))
                } else {
                    0
                };
                // PAGE decoding reads the following record header before yielding.
                let boundary = if tag == TAG_PAGE {
                    end.checked_add(RECORD_HEADER_BYTES)?
                } else {
                    end
                };
                bytes.get(..boundary)?;
                steps.push((start..boundary, rows));
                start = boundary;
            } else if tag == TAG_FINISH {
                ready?;
                steps.push((start..end, 0));
                finished = true;
            }
            offset = end;
        }
        if !finished {
            return None;
        }
        Some(Self {
            ready: ready?,
            steps,
        })
    }

    fn batch_end(&self, cursor: usize, rows: usize) -> Option<usize> {
        if cursor > self.steps.len() {
            return None;
        }
        let mut end = cursor;
        let mut count = 0;
        while end < self.steps.len() && (count < rows || self.steps[end].1 == 0) {
            count += self.steps[end].1;
            end += 1;
        }
        Some(end)
    }
}

struct CachedSnapshot {
    terminal: u64,
    generation: u64,
    bytes: Bytes,
    index: SnapshotIndex,
    used: Instant,
}

#[derive(Default)]
pub(crate) struct HistoryCache {
    snapshots: HashMap<String, CachedSnapshot>,
    bytes: usize,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryCursor {
    pub snapshot_id: String,
    pub cursor: usize,
    pub steps: usize,
    pub has_more: bool,
}

impl HistoryCache {
    fn expire(&mut self) {
        let now = Instant::now();
        self.snapshots
            .retain(|_, entry| now.duration_since(entry.used) < CACHE_TTL);
        self.bytes = self.snapshots.values().map(|entry| entry.bytes.len()).sum();
    }

    pub(crate) fn insert(
        &mut self,
        terminal: u64,
        generation: u64,
        bytes: Bytes,
        rows: usize,
    ) -> Option<(HistoryCursor, Bytes)> {
        self.expire();
        if bytes.len() > CACHE_BYTES {
            return None;
        }
        let index = SnapshotIndex::parse(&bytes)?;
        let end = index.batch_end(0, rows)?;
        while self.bytes + bytes.len() > CACHE_BYTES {
            let key = self
                .snapshots
                .iter()
                .min_by_key(|(_, value)| value.used)?
                .0
                .clone();
            let removed = self.snapshots.remove(&key)?;
            self.bytes -= removed.bytes.len();
        }
        let snapshot_id = format!("{:032x}", rand::random::<u128>());
        let cursor = HistoryCursor {
            snapshot_id: snapshot_id.clone(),
            cursor: end,
            steps: end,
            has_more: end < index.steps.len(),
        };
        let prefix_end = if end == 0 {
            index.ready
        } else {
            index.steps[end - 1].0.end
        };
        let prefix = bytes.slice(..prefix_end);
        self.bytes += bytes.len();
        self.snapshots.insert(
            snapshot_id,
            CachedSnapshot {
                terminal,
                generation,
                bytes,
                index,
                used: Instant::now(),
            },
        );
        Some((cursor, prefix))
    }

    pub(crate) fn page(
        &mut self,
        terminal: u64,
        generation: u64,
        id: &str,
        cursor: usize,
        rows: Option<usize>,
    ) -> Option<(HistoryCursor, Bytes)> {
        self.expire();
        let entry = self.snapshots.get_mut(id)?;
        if entry.terminal != terminal || entry.generation != generation {
            return None;
        }
        let end = entry.index.batch_end(
            cursor,
            rows.unwrap_or(PAGE_BATCH_ROWS).clamp(1, PAGE_BATCH_ROWS),
        )?;
        let start = if cursor == 0 {
            entry.index.ready
        } else {
            entry.index.steps.get(cursor - 1)?.0.end
        };
        let finish = if end == 0 {
            start
        } else {
            entry.index.steps[end - 1].0.end
        };
        entry.used = Instant::now();
        Some((
            HistoryCursor {
                snapshot_id: id.to_owned(),
                cursor: end,
                steps: end - cursor,
                has_more: end < entry.index.steps.len(),
            },
            entry.bytes.slice(start..finish),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Bytes {
        let mut bytes = b"GHOSTSNP\x01\x00".to_vec();
        for (tag, length) in [
            (TAG_READY, 0),
            (TAG_PAGE, PAGE_HEADER_BYTES),
            (TAG_PAGE, PAGE_HEADER_BYTES),
            (TAG_FINISH, 0),
        ] {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&(length as u32).to_le_bytes());
            bytes.extend_from_slice(&[0; 4]);
            if length > 0 {
                let mut page = vec![0; length];
                page[PAGE_ROWS_OFFSET..PAGE_ROWS_OFFSET + 2].copy_from_slice(&250u16.to_le_bytes());
                bytes.extend(page);
            }
        }
        bytes.into()
    }
    #[test]
    fn pages_partition_the_original_stream_and_reject_other_terminals() {
        let source = snapshot();
        let mut cache = HistoryCache::default();
        let (mut cursor, prefix) = cache.insert(1, 2, source.clone(), 0).unwrap();
        assert!(!prefix.is_empty());
        assert_eq!(cursor.steps, 0);
        assert!(cache.page(2, 2, &cursor.snapshot_id, 0, None).is_none());
        assert!(cache.page(1, 3, &cursor.snapshot_id, 0, None).is_none());
        assert!(cache
            .page(1, 2, &cursor.snapshot_id, usize::MAX, None)
            .is_none());
        let mut rebuilt = prefix.to_vec();
        while cursor.has_more {
            let (next, bytes) = cache
                .page(1, 2, &cursor.snapshot_id, cursor.cursor, None)
                .unwrap();
            assert!(next.cursor > cursor.cursor);
            rebuilt.extend_from_slice(&bytes);
            cursor = next;
        }
        assert_eq!(rebuilt, source);
    }
    #[test]
    fn indexes_native_snapshots_without_sending_the_entire_history() {
        let mut terminal = crate::term_model::TerminalModel::new(80, 24);
        for line in 0..3000 {
            terminal.advance(format!("history {line:04}\r\n").as_bytes());
        }
        let source = Bytes::from(terminal.snapshot());
        let mut cache = HistoryCache::default();
        let (mut cursor, prefix) = cache.insert(1, 1, source.clone(), 0).unwrap();
        assert!(cursor.has_more);
        assert!(prefix.len() < source.len() / 2);
        let mut rebuilt = prefix.to_vec();
        while cursor.has_more {
            let (next, bytes) = cache
                .page(1, 1, &cursor.snapshot_id, cursor.cursor, None)
                .unwrap();
            rebuilt.extend_from_slice(&bytes);
            cursor = next;
        }
        assert_eq!(rebuilt, source);
    }

    #[test]
    fn rejects_truncated_records_and_missing_finish() {
        let bytes = snapshot();
        for end in 0..bytes.len() {
            assert!(SnapshotIndex::parse(&bytes[..end]).is_none());
        }
        assert!(SnapshotIndex::parse(&bytes).is_some());
    }
}
