//! A text that stays in its file. Opening reads it once to find the line starts and to check that
//! it is UTF-8; after that, only the ranges that are asked for are read, through a small cache of
//! blocks. A very large file therefore costs a few bytes per thousand lines, not its own size.
//!
//! The text is read only: nothing here changes the file. The editing core sees it as a
//! `TextSource`.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Mutex};

use loog_editor_core::text::{FileIndex, IndexBuilder, TextSource};

const BLOCK: usize = 64 * 1024;
const CACHED_BLOCKS: usize = 8;
const SCAN_CHUNK: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct FileText {
    file: File,
    /// Bytes before the text itself: a UTF-8 byte order mark.
    skip: usize,
    index: FileIndex,
    cache: Mutex<VecDeque<(usize, Arc<Vec<u8>>)>>,
}

#[cfg(windows)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buffer, offset)
}
#[cfg(unix)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buffer, offset)
}

fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let read = read_at(file, buffer, offset)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        offset += read as u64;
        buffer = &mut buffer[read..];
    }
    Ok(())
}

fn byte_order_mark(file: &File) -> io::Result<usize> {
    let mut head = [0u8; 3];
    let head_len = read_at(file, &mut head, 0)?;
    Ok(if head[..head_len].starts_with(&[0xef, 0xbb, 0xbf]) {
        3
    } else {
        0
    })
}

impl FileText {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with(path, IndexBuilder::new())
    }

    fn open_with(path: &Path, mut builder: IndexBuilder) -> io::Result<Self> {
        let file = File::open(path)?;
        let total = file.metadata()?.len() as usize;
        let mut chunk = vec![0u8; SCAN_CHUNK.min(total.max(1)) + 4];
        let skip = byte_order_mark(&file)?;
        let len = total - skip;
        // A UTF-8 sequence cut by the end of a chunk is carried over to the next one.
        let mut carry = Vec::<u8>::new();
        let mut at = 0usize;
        while at < len {
            let want = (chunk.len() - 4).min(len - at);
            let read = &mut chunk[carry.len()..carry.len() + want];
            read_exact_at(&file, read, (skip + at) as u64)?;
            chunk[..carry.len()].copy_from_slice(&carry);
            let filled = carry.len() + want;
            let valid = match std::str::from_utf8(&chunk[..filled]) {
                Ok(_) => filled,
                Err(error) if error.error_len().is_none() && at + want < len => error.valid_up_to(),
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "file is not valid UTF-8",
                    ));
                }
            };
            builder.feed(&chunk[..valid]);
            carry.clear();
            carry.extend_from_slice(&chunk[valid..filled]);
            at += want;
        }
        let index = builder.finish();
        debug_assert_eq!(index.len(), len);
        Ok(Self::assemble(file, skip, index))
    }

    /// A file whose lines are already known, from writing it: it is opened without being read.
    /// An error means the file is not the size it was said to be, and `open` should be used.
    pub fn open_indexed(path: &Path, index: FileIndex) -> io::Result<Self> {
        let file = File::open(path)?;
        let total = file.metadata()?.len() as usize;
        let skip = byte_order_mark(&file)?;
        if total != skip + index.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the file is not what was written",
            ));
        }
        Ok(Self::assemble(file, skip, index))
    }

    fn assemble(file: File, skip: usize, index: FileIndex) -> Self {
        Self {
            file,
            skip,
            index,
            cache: Mutex::default(),
        }
    }

    /// The text of `range`. The file was checked to be UTF-8 when it was opened, so a range on
    /// character boundaries always decodes; a range that is not (or a file that changed since)
    /// shows replacement characters instead of failing.
    pub fn text(&self, range: Range<usize>) -> String {
        match self.read(range) {
            Ok(bytes) => String::from_utf8(bytes)
                .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()),
            Err(_) => String::new(),
        }
    }

    fn block(&self, index: usize) -> io::Result<Arc<Vec<u8>>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(position) = cache.iter().position(|(i, _)| *i == index) {
            let entry = cache.remove(position).unwrap();
            let block = entry.1.clone();
            cache.push_front(entry);
            return Ok(block);
        }
        let start = index * BLOCK;
        let mut bytes = vec![0u8; BLOCK.min(self.index.len().saturating_sub(start))];
        read_exact_at(&self.file, &mut bytes, (self.skip + start) as u64)?;
        let block = Arc::new(bytes);
        cache.push_front((index, block.clone()));
        cache.truncate(CACHED_BLOCKS);
        Ok(block)
    }
}

impl TextSource for FileText {
    fn len(&self) -> usize {
        self.index.len()
    }
    fn has_bom(&self) -> bool {
        self.skip > 0
    }
    fn line_count(&self) -> usize {
        self.index.line_count()
    }
    fn longest_line(&self) -> usize {
        self.index.longest_line()
    }
    fn first_line_is_crlf(&self) -> bool {
        self.index.first_line_is_crlf()
    }

    /// The bytes of `range`, read through the block cache.
    fn read(&self, range: Range<usize>) -> io::Result<Vec<u8>> {
        let len = self.index.len();
        let range = range.start.min(len)..range.end.min(len);
        let mut out = Vec::with_capacity(range.len());
        let mut at = range.start;
        while at < range.end {
            let index = at / BLOCK;
            let block = self.block(index)?;
            let from = at - index * BLOCK;
            let to = (range.end - index * BLOCK).min(block.len());
            if to <= from {
                break;
            }
            out.extend_from_slice(&block[from..to]);
            at = index * BLOCK + to;
        }
        Ok(out)
    }

    /// Read `range` straight from the file into `buffer`, past the cache: for copying a whole
    /// file, which would only push out the blocks the display uses.
    fn read_into(&self, range: Range<usize>, buffer: &mut [u8]) -> io::Result<()> {
        let len = self.index.len();
        let range = range.start.min(len)..range.end.min(len);
        read_exact_at(
            &self.file,
            &mut buffer[..range.len()],
            (self.skip + range.start) as u64,
        )
    }

    fn line_start(&self, line: usize) -> usize {
        self.index.line_start(line, &|range| self.read(range))
    }

    fn line_of(&self, offset: usize) -> usize {
        self.index.line_of(offset, &|range| self.read(range))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loog_editor_core::text::{MemorySource, TextBuffer};
    use std::io::Write;

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("loog-file-text-{}-{name}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        path
    }

    #[test]
    fn lines_match_an_in_memory_scan_across_checkpoints_and_blocks() {
        let mut text = String::new();
        for i in 0..5000 {
            text.push_str(&format!("row {i} 中文 {}\n", "x".repeat(i % 53)));
        }
        text.push_str("last line without a break");
        let path = temp("lines", text.as_bytes());
        let file = FileText::open(&path).unwrap();
        let starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        assert_eq!(file.line_count(), starts.len());
        assert_eq!(file.len(), text.len());
        for (line, &start) in starts.iter().enumerate().step_by(37) {
            assert_eq!(file.line_start(line), start, "line {line}");
            assert_eq!(file.line_of(start), line);
        }
        assert_eq!(file.line_of(text.len()), starts.len() - 1);
        assert_eq!(file.line_start(starts.len()), text.len());
        // A range across several blocks reads back exactly.
        let range = 1000..200_000;
        assert_eq!(file.read(range.clone()).unwrap(), &text.as_bytes()[range]);
        let longest = text.lines().map(str::len).max().unwrap();
        assert_eq!(file.longest_line(), longest);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn byte_order_mark_crlf_and_trailing_break_are_understood() {
        let path = temp("bom", b"\xef\xbb\xbfone\r\ntwo\r\n");
        let file = FileText::open(&path).unwrap();
        assert!(file.has_bom() && file.first_line_is_crlf());
        assert_eq!((file.len(), file.line_count()), (10, 3));
        assert_eq!(file.text(0..3), "one");
        assert_eq!(file.line_start(2), 10);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn invalid_utf8_is_refused_even_when_split_by_a_scan_chunk() {
        let mut bytes = "é".repeat(SCAN_CHUNK).into_bytes();
        bytes.push(0xff);
        let path = temp("bad", &bytes);
        assert_eq!(
            FileText::open(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // Multi-byte characters straddling chunk boundaries are fine.
        let good = "é".repeat(SCAN_CHUNK);
        let path2 = temp("good", good.as_bytes());
        assert_eq!(FileText::open(&path2).unwrap().len(), good.len());
        std::fs::remove_file(path).ok();
        std::fs::remove_file(path2).ok();
    }

    /// The lines learned while writing a text are those of reading the file it makes.
    #[test]
    fn lines_learned_while_writing_equal_lines_found_by_reading() {
        let text = "alpha\r\nbeta\n中文 gamma\r\n\r\nlast".repeat(400);
        let path = temp("indexed", text.as_bytes());
        let buffer = TextBuffer::from_source(Arc::new(MemorySource::from_text(&text)));
        let pieces = buffer.file_pieces().unwrap();
        let mut written = Vec::new();
        let index = pieces.write_indexed(&mut written).unwrap();
        assert_eq!(written, text.as_bytes());
        let learned = FileText::open_indexed(&path, index).unwrap();
        let read = FileText::open(&path).unwrap();
        assert_eq!(
            (learned.len(), learned.line_count(), learned.longest_line()),
            (read.len(), read.line_count(), read.longest_line())
        );
        for line in (0..read.line_count()).step_by(11) {
            assert_eq!(learned.line_start(line), read.line_start(line));
        }
        // A file that is not the size said is refused.
        let other = pieces.write_indexed(&mut Vec::new()).unwrap();
        std::fs::write(&path, b"short").unwrap();
        assert!(FileText::open_indexed(&path, other).is_err());
        std::fs::remove_file(path).ok();
    }

    /// `LOOG_FILE_TEXT_PROBE=<path>` times opening a real file. Not run by default.
    #[test]
    #[ignore = "reads the file named by LOOG_FILE_TEXT_PROBE"]
    fn open_time_of_a_real_file() {
        let path = std::env::var("LOOG_FILE_TEXT_PROBE").expect("set LOOG_FILE_TEXT_PROBE");
        let started = std::time::Instant::now();
        let file = FileText::open(Path::new(&path)).unwrap();
        println!(
            "FILE_TEXT opened {} bytes, {} lines, longest {} in {:?}",
            file.len(),
            file.line_count(),
            file.longest_line(),
            started.elapsed()
        );
        let started = std::time::Instant::now();
        let middle = file.line_start(file.line_count() / 2);
        let text = file.text(middle..middle + 200);
        println!(
            "FILE_TEXT middle line lookup {:?}: {:?}",
            started.elapsed(),
            &text[..text.len().min(60)]
        );
    }
}
