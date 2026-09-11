//! Port of packages/coding-agent/src/utils/file-lines.ts

use std::io::Read;

pub fn read_first_line_sync(file_path: &str, max_bytes: usize) -> Option<String> {
    const DEFAULT_MAX_BYTES: usize = 64 * 1024;
    let max_bytes = if max_bytes == 0 { DEFAULT_MAX_BYTES } else { max_bytes };

    let mut file = std::fs::File::open(file_path).ok()?;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut position = 0usize;
    let mut buffer = vec![0u8; 1024];

    while position < max_bytes {
        let bytes_to_read = buffer.len().min(max_bytes - position);
        let bytes_read = file.read(&mut buffer[..bytes_to_read]).ok()?;
        if bytes_read == 0 {
            break;
        }

        let chunk = &buffer[..bytes_read];
        match chunk.iter().position(|byte| *byte == 0x0a) {
            Some(newline_index) => {
                chunks.push(chunk[..newline_index].to_vec());
                let mut joined: Vec<u8> = Vec::new();
                for part in &chunks {
                    joined.extend_from_slice(part);
                }
                return Some(strip_trailing_cr(&String::from_utf8_lossy(&joined)));
            }
            None => {
                chunks.push(chunk.to_vec());
                position += bytes_read;
            }
        }
    }

    if chunks.is_empty() {
        return None;
    }
    let mut joined: Vec<u8> = Vec::new();
    for part in &chunks {
        joined.extend_from_slice(part);
    }
    Some(strip_trailing_cr(&String::from_utf8_lossy(&joined)))
}

fn strip_trailing_cr(value: &str) -> String {
    value.strip_suffix('\r').unwrap_or(value).to_string()
}

/// Read the bytes in [start, endExclusive), stopping early at EOF.
pub fn read_bytes_sync(file_path: &str, start: i64, end_exclusive: i64) -> Vec<u8> {
    let length = (end_exclusive - start).max(0) as usize;
    let mut buffer = vec![0u8; length];
    let Ok(mut file) = std::fs::File::open(file_path) else {
        return Vec::new();
    };
    if start > 0 {
        use std::io::Seek;
        if file.seek(std::io::SeekFrom::Start(start as u64)).is_err() {
            return Vec::new();
        }
    }
    let mut offset = 0usize;
    while offset < length {
        match file.read(&mut buffer[offset..length]) {
            Ok(0) => break,
            Ok(bytes_read) => offset += bytes_read,
            Err(_) => break,
        }
    }
    buffer.truncate(offset);
    buffer
}

#[derive(Default, Clone)]
pub struct ReadLinesRange {
    pub start: Option<i64>,
    /// Inclusive, as in createReadStream: bounds the read to a stat() snapshot so a growing file cannot extend the scan.
    pub end: Option<i64>,
}

/// Port of `readLinesAsBuffers`. The TypeScript yields buffers from a read
/// stream; the Rust version reads the same byte range and yields the same
/// newline-delimited records, and reports every chunk length to `on_bytes_read`.
pub fn read_lines_as_buffers(
    file_path: &str,
    range: Option<&ReadLinesRange>,
    mut on_bytes_read: Option<&mut dyn FnMut(usize)>,
) -> std::io::Result<Vec<Vec<u8>>> {
    let range = range.cloned().unwrap_or_default();

    let mut file = std::fs::File::open(file_path)?;
    if let Some(start) = range.start {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(start.max(0) as u64))?;
    }
    let mut remaining: Option<usize> = match (range.start, range.end) {
        (Some(start), Some(end)) => Some((end - start + 1).max(0) as usize),
        (None, Some(end)) => Some((end + 1).max(0) as usize),
        _ => None,
    };

    let mut pending_parts: Vec<Vec<u8>> = Vec::new();
    let mut pending_bytes = 0usize;
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];

    loop {
        let read_size = match remaining {
            Some(0) => break,
            Some(limit) => chunk.len().min(limit),
            None => chunk.len(),
        };
        let bytes_read = file.read(&mut chunk[..read_size])?;
        if bytes_read == 0 {
            break;
        }
        if let Some(limit) = remaining.as_mut() {
            *limit -= bytes_read;
        }

        let buffer = &chunk[..bytes_read];
        // Read observation is disposable and must not change the stream.
        if let Some(observer) = on_bytes_read.as_mut() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(buffer.len())));
        }

        let mut start = 0usize;
        while start < buffer.len() {
            match buffer[start..].iter().position(|byte| *byte == 0x0a) {
                None => {
                    let part = buffer[start..].to_vec();
                    pending_bytes += part.len();
                    pending_parts.push(part);
                    break;
                }
                Some(offset) => {
                    let end = start + offset;
                    if !pending_parts.is_empty() {
                        let part = buffer[start..end].to_vec();
                        pending_bytes += part.len();
                        pending_parts.push(part);
                        let mut line: Vec<u8> = Vec::with_capacity(pending_bytes);
                        for piece in pending_parts.drain(..) {
                            line.extend_from_slice(&piece);
                        }
                        pending_bytes = 0;
                        lines.push(line);
                    } else {
                        lines.push(buffer[start..end].to_vec());
                    }
                    start = end + 1;
                }
            }
        }
    }

    if !pending_parts.is_empty() {
        let mut line: Vec<u8> = Vec::with_capacity(pending_bytes);
        for piece in pending_parts.drain(..) {
            line.extend_from_slice(&piece);
        }
        lines.push(line);
    }

    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, contents: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap();
        // Keep the temp dir alive for the test process lifetime.
        std::mem::forget(dir);
        path.to_string_lossy().to_string()
    }

    #[test]
    fn reads_the_first_line_and_strips_cr() {
        let path = write_temp("first.txt", b"hello\r\nsecond\n");
        assert_eq!(read_first_line_sync(&path, 0).unwrap(), "hello");
    }

    #[test]
    fn reads_the_first_line_without_a_newline() {
        let path = write_temp("no-newline.txt", b"only line");
        assert_eq!(read_first_line_sync(&path, 0).unwrap(), "only line");
    }

    #[test]
    fn returns_none_for_empty_files() {
        let path = write_temp("empty.txt", b"");
        assert_eq!(read_first_line_sync(&path, 0), None);
    }

    #[test]
    fn reads_a_byte_range_and_stops_at_eof() {
        let path = write_temp("range.txt", b"abcdefgh");
        assert_eq!(read_bytes_sync(&path, 2, 6), b"cdef".to_vec());
        assert_eq!(read_bytes_sync(&path, 6, 100), b"gh".to_vec());
    }

    #[test]
    fn splits_lines_and_reports_chunk_bytes() {
        let path = write_temp("lines.txt", b"one\ntwo\n");
        let mut observed: Vec<usize> = Vec::new();
        let mut observer = |bytes: usize| observed.push(bytes);
        let lines = read_lines_as_buffers(&path, None, Some(&mut observer)).unwrap();
        assert_eq!(lines, vec![b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(observed, vec![8]);
    }

    #[test]
    fn yields_the_trailing_partial_line() {
        let path = write_temp("partial.txt", b"one\ntwo");
        let lines = read_lines_as_buffers(&path, None, None).unwrap();
        assert_eq!(lines, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn honours_the_byte_range() {
        let path = write_temp("ranged-lines.txt", b"aa\nbb\ncc\n");
        let range = ReadLinesRange {
            start: Some(3),
            end: Some(5),
        };
        let lines = read_lines_as_buffers(&path, Some(&range), None).unwrap();
        assert_eq!(lines, vec![b"bb".to_vec()]);
    }

    #[test]
    fn joins_a_record_split_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let mut contents = vec![b'a'; 64 * 1024];
        contents.extend_from_slice(&vec![b'b'; 64 * 1024]);
        contents.push(b'\n');
        contents.extend_from_slice(b"tail\n");
        std::fs::write(&path, &contents).unwrap();

        let lines = read_lines_as_buffers(path.to_str().unwrap(), None, None).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 64 * 1024 * 2);
        assert_eq!(lines[1], b"tail".to_vec());
    }
}
