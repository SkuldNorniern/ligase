//! big csv and tsv files read in place: an index of where each row starts,
//! made in one pass, then only the rows in sight read from disk.

#![allow(dead_code)] // `split` is only used by the tests

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

/// bytes per read of the index pass.
const BUF: usize = 1 << 20;
/// progress and stop checks every this many bytes.
const REPORT: u64 = 8 << 20;
/// rows indexed at most; past it the last row runs to the end of the file.
const MAX_ROWS: usize = 50_000_000;
/// rows per checkpoint.
const CHECK_EVERY: usize = 64;
const BLOCK: u64 = 256 << 10;
const MAX_BLOCKS: usize = 16;
/// bytes of one row that are read; a longer row is cut.
const MAX_ROW: u64 = 1 << 20;
const MAX_CELLS: usize = 256;
const MAX_CELL: usize = 1024;

/// where each row of a csv or tsv file starts. a u64 per checkpoint (every 64
/// rows) and a u32 delta per row: about 4.25 bytes a row. a row whose delta
/// does not fit a u32 starts a checkpoint early.
#[derive(Debug)]
pub struct Index {
    pub path: PathBuf,
    /// file length when indexed.
    pub len: u64,
    pub modified: Option<SystemTime>,
    /// b',' or b'\t'.
    pub delimiter: u8,
    rows: usize,
    /// (row, its offset) for each checkpoint, rows ascending.
    checks: Vec<(usize, u64)>,
    /// offset from the row before; 0 on a checkpoint row.
    deltas: Vec<u32>,
}

impl Index {
    /// builds the index with one buffered pass. `progress` gets the bytes read
    /// every 8 MiB, and `stop` is polled at the same points. a blank line is an
    /// empty row; a last line break leaves no row after it.
    ///
    /// # Errors
    /// the file cannot be read, it changed while it was read, or `stop` is set
    /// ("stopped").
    pub fn build(
        path: &Path,
        mut progress: impl FnMut(u64),
        stop: &AtomicBool,
    ) -> Result<Index, String> {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let file = File::open(path).map_err(|e| format!("cannot open file: {e}"))?;
        let meta = file
            .metadata()
            .map_err(|e| format!("cannot read file: {e}"))?;
        let len = meta.len();
        let mut reader = BufReader::with_capacity(BUF, file);
        let mut b = Builder::default();
        let mut delimiter = None;
        let mut quote = Quote::Out;
        let mut empty = true;
        let mut at_start = true;
        let mut pos = 0u64;
        let mut next_report = REPORT;
        loop {
            let buf = reader
                .fill_buf()
                .map_err(|e| format!("cannot read file: {e}"))?;
            if buf.is_empty() {
                break;
            }
            let delim = *delimiter.get_or_insert_with(|| sniff(buf));
            for (k, &c) in buf.iter().enumerate() {
                if at_start && !b.push(pos + k as u64) {
                    break;
                }
                at_start = scan(&mut quote, &mut empty, c, delim);
            }
            if b.capped {
                break;
            }
            let n = buf.len();
            reader.consume(n);
            pos += n as u64;
            if pos >= next_report {
                progress(pos);
                if stop.load(Ordering::Relaxed) {
                    return Err("stopped".into());
                }
                next_report += REPORT;
            }
        }
        if !b.capped && pos != len {
            return Err("file changed".into());
        }
        Ok(Index {
            path: path.to_path_buf(),
            len,
            modified: meta.modified().ok(),
            delimiter: delimiter.unwrap_or(b','),
            rows: b.rows,
            checks: b.checks,
            deltas: b.deltas,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// byte range of row `i`: its start up to the next row's start, or to the
    /// file end for the last row. none past the end.
    pub fn span(&self, i: usize) -> Option<Range<u64>> {
        let start = self.start(i)?;
        let end = if i + 1 < self.rows {
            self.start(i + 1)?
        } else {
            self.len
        };
        Some(start..end)
    }

    /// offset of row `i`: its checkpoint plus the deltas up to it.
    fn start(&self, i: usize) -> Option<u64> {
        if i >= self.rows {
            return None;
        }
        let k = self
            .checks
            .partition_point(|&(row, _)| row <= i)
            .checked_sub(1)?;
        let (row, mut at) = self.checks[k];
        for d in &self.deltas[row + 1..=i] {
            at += u64::from(*d);
        }
        Some(at)
    }
}

/// the checkpoints and deltas of rows as the scan finds them.
#[derive(Default)]
struct Builder {
    checks: Vec<(usize, u64)>,
    deltas: Vec<u32>,
    rows: usize,
    last: u64,
    /// rows since the last checkpoint, the checkpoint row included.
    group: usize,
    /// the row cap was hit.
    capped: bool,
}

impl Builder {
    /// adds a row that starts at `at`; false when the row cap is hit.
    fn push(&mut self, at: u64) -> bool {
        if self.rows == MAX_ROWS {
            self.capped = true;
            return false;
        }
        let check =
            self.rows == 0 || self.group == CHECK_EVERY || at - self.last > u64::from(u32::MAX);
        if check {
            self.checks.push((self.rows, at));
            self.deltas.push(0);
            self.group = 0;
        } else {
            self.deltas.push((at - self.last) as u32);
        }
        self.group += 1;
        self.last = at;
        self.rows += 1;
        true
    }
}

/// quote state of the scan. `Pending` is a quote seen inside quotes: the next
/// byte decides between an escaped quote and the close.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    Out,
    In,
    Pending,
}

/// one byte of a record, the way `fields` reads it. `empty` is whether the
/// field so far has no bytes: a quote only opens an empty field. returns true
/// when the byte ends the record.
fn scan(quote: &mut Quote, empty: &mut bool, c: u8, delim: u8) -> bool {
    if *quote == Quote::Pending {
        if c == b'"' {
            *quote = Quote::In;
            *empty = false;
            return false;
        }
        *quote = Quote::Out;
    }
    match *quote {
        Quote::In => {
            if c == b'"' {
                *quote = Quote::Pending;
            } else {
                *empty = false;
            }
            false
        }
        _ => {
            if c == b'"' && *empty {
                *quote = Quote::In;
            } else if c == b'\n' {
                *empty = true;
                return true;
            } else {
                // a field is empty again right after a delimiter
                *empty = c == delim;
            }
            false
        }
    }
}

/// tab when the first line has more tabs than commas, else comma. same rule
/// as the table preview.
fn sniff(head: &[u8]) -> u8 {
    let line = head
        .split(|&c| c == b'\n' || c == b'\r')
        .next()
        .unwrap_or(&[]);
    let tabs = line.iter().filter(|&&c| c == b'\t').count();
    let commas = line.iter().filter(|&&c| c == b',').count();
    if tabs > commas { b'\t' } else { b',' }
}

/// reads rows of an indexed file with a cache of recently read blocks.
pub struct Reader {
    index: Arc<Index>,
    file: File,
    /// (block start, bytes), least recently used first.
    blocks: Vec<(u64, Vec<u8>)>,
}

impl Reader {
    /// # Errors
    /// the file cannot be opened.
    pub fn open(index: Arc<Index>) -> Result<Reader, String> {
        let file = File::open(&index.path).map_err(|e| format!("cannot open file: {e}"))?;
        Ok(Reader {
            index,
            file,
            blocks: Vec::new(),
        })
    }

    /// cells of the rows in `range`. rows past the end are left out.
    ///
    /// # Errors
    /// the file changed size since it was indexed, or a read fails.
    pub fn rows(&mut self, range: Range<usize>) -> Result<Vec<Vec<String>>, String> {
        let size = fs::metadata(&self.index.path)
            .map_err(|e| format!("cannot read file: {e}"))?
            .len();
        if size != self.index.len {
            return Err("file changed".into());
        }
        let end = range.end.min(self.index.rows());
        let start = range.start.min(end);
        let mut out = Vec::with_capacity(end - start);
        for i in start..end {
            let span = self.index.span(i).ok_or("row past the end")?;
            let to = span.end.min(span.start.saturating_add(MAX_ROW));
            let bytes = self.bytes(span.start, to)?;
            out.push(fields(&bytes, self.index.delimiter, MAX_CELLS, MAX_CELL));
        }
        Ok(out)
    }

    /// the bytes from `from` to `to`, through the block cache.
    fn bytes(&mut self, from: u64, to: u64) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity((to - from) as usize);
        let mut at = from;
        while at < to {
            let k = self.block(at)?;
            let (start, data) = &self.blocks[k];
            let start = *start;
            let lo = (at - start) as usize;
            let hi = (to - start).min(data.len() as u64) as usize;
            if hi <= lo {
                return Err("short read".into());
            }
            out.extend_from_slice(&data[lo..hi]);
            at = start + hi as u64;
        }
        Ok(out)
    }

    /// the cache slot of the block holding `at`, read in when missing.
    fn block(&mut self, at: u64) -> Result<usize, String> {
        let start = at - at % BLOCK;
        if let Some(k) = self.blocks.iter().position(|(s, _)| *s == start) {
            let hit = self.blocks.remove(k);
            self.blocks.push(hit);
        } else {
            let n = BLOCK.min(self.index.len.saturating_sub(start));
            if n == 0 {
                return Err("read past the end".into());
            }
            let mut data = vec![0u8; n as usize];
            self.file
                .seek(SeekFrom::Start(start))
                .map_err(|e| format!("cannot read file: {e}"))?;
            self.file
                .read_exact(&mut data)
                .map_err(|e| format!("cannot read file: {e}"))?;
            if self.blocks.len() == MAX_BLOCKS {
                self.blocks.remove(0);
            }
            self.blocks.push((start, data));
        }
        Ok(self.blocks.len() - 1)
    }
}

/// splits one record's bytes into cells: quotes, "" escapes, and the trailing
/// \r\n or \n taken off. a blank record has no cells.
pub fn split(record: &[u8], delimiter: u8) -> Vec<String> {
    fields(record, delimiter, usize::MAX, usize::MAX)
}

/// `split` with caps: at most `max_cells` cells, each cut to `max_cell` bytes.
fn fields(record: &[u8], delim: u8, max_cells: usize, max_cell: usize) -> Vec<String> {
    let rec = strip_eol(record);
    let mut out = Vec::new();
    if rec.is_empty() {
        return out;
    }
    let mut cell = Vec::new();
    let mut quoted = false;
    let mut i = 0;
    while i < rec.len() {
        let c = rec[i];
        i += 1;
        if quoted {
            if c != b'"' {
                cell.push(c);
            } else if rec.get(i) == Some(&b'"') {
                cell.push(b'"');
                i += 1;
            } else {
                quoted = false;
            }
        } else if c == b'"' && cell.is_empty() {
            quoted = true;
        } else if c == delim {
            out.push(text(&cell, max_cell));
            cell.clear();
            if out.len() == max_cells {
                return out;
            }
        } else {
            cell.push(c);
        }
    }
    out.push(text(&cell, max_cell));
    out
}

/// the record without its trailing \n, and the \r before it.
fn strip_eol(record: &[u8]) -> &[u8] {
    let mut r = record;
    if let Some(rest) = r.strip_suffix(b"\n") {
        r = rest;
        if let Some(rest) = r.strip_suffix(b"\r") {
            r = rest;
        }
    }
    r
}

/// a cell as text, lossy utf-8, cut to `max` bytes with a "…" on the cut.
fn text(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        return s.into_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + '…'.len_utf8());
    out.push_str(&s[..end]);
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufWriter, Write};
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    /// a temp file, removed on drop.
    struct Tmp(PathBuf);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    static COUNT: AtomicU64 = AtomicU64::new(0);

    fn tmp(bytes: &[u8]) -> Tmp {
        let n = COUNT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ligase_rows_{}_{n}.csv", std::process::id()));
        fs::write(&path, bytes).unwrap();
        Tmp(path)
    }

    fn index_of(t: &Tmp) -> Index {
        Index::build(&t.0, |_| {}, &AtomicBool::new(false)).unwrap()
    }

    fn read_rows(index: Index) -> Vec<Vec<String>> {
        let n = index.rows();
        let mut r = Reader::open(Arc::new(index)).unwrap();
        r.rows(0..n).unwrap()
    }

    fn cells(row: &[&str]) -> Vec<String> {
        row.iter().map(|s| s.to_string()).collect()
    }

    fn xorshift(mut x: u64) -> u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }

    #[test]
    fn basic_rows() {
        let t = tmp(b"a,b\nc,d\ne,f\n");
        let idx = index_of(&t);
        assert_eq!(idx.rows(), 3);
        assert_eq!(idx.delimiter, b',');
        assert_eq!(idx.len, 12);
        assert_eq!(
            read_rows(idx),
            vec![cells(&["a", "b"]), cells(&["c", "d"]), cells(&["e", "f"])]
        );
    }

    #[test]
    fn quoted_newline_stays_in_row() {
        let t = tmp(b"a,\"x\ny\"\nb,c\n");
        let idx = index_of(&t);
        assert_eq!(idx.rows(), 2);
        assert_eq!(idx.span(0), Some(0..8));
        assert_eq!(idx.span(1), Some(8..12));
        assert_eq!(
            read_rows(idx),
            vec![cells(&["a", "x\ny"]), cells(&["b", "c"])]
        );
    }

    #[test]
    fn crlf_rows() {
        let t = tmp(b"a,b\r\nc,d\r\n");
        let idx = index_of(&t);
        assert_eq!(idx.rows(), 2);
        assert_eq!(idx.span(0), Some(0..5));
        assert_eq!(read_rows(idx), vec![cells(&["a", "b"]), cells(&["c", "d"])]);
    }

    #[test]
    fn no_trailing_newline_counts_last_row() {
        let t = tmp(b"a,b\nc,d");
        let idx = index_of(&t);
        assert_eq!(idx.rows(), 2);
        assert_eq!(idx.span(1), Some(4..7));
        assert_eq!(read_rows(idx), vec![cells(&["a", "b"]), cells(&["c", "d"])]);
    }

    #[test]
    fn blank_line_is_empty_row() {
        let t = tmp(b"a\n\nb\n");
        let idx = index_of(&t);
        assert_eq!(idx.rows(), 3);
        assert_eq!(read_rows(idx), vec![cells(&["a"]), vec![], cells(&["b"])]);
    }

    #[test]
    fn tsv_rows() {
        let t = tmp(b"a\tb,c\td\n1\t2\t3\n");
        let idx = index_of(&t);
        assert_eq!(idx.delimiter, b'\t');
        assert_eq!(
            read_rows(idx),
            vec![cells(&["a", "b,c", "d"]), cells(&["1", "2", "3"])]
        );
    }

    #[test]
    fn quote_mid_field_is_plain_text() {
        let t = tmp(b"a,5\" tall\nb,c\n");
        let idx = index_of(&t);
        assert_eq!(idx.rows(), 2);
        assert_eq!(
            read_rows(idx),
            vec![cells(&["a", "5\" tall"]), cells(&["b", "c"])]
        );
    }

    #[test]
    fn row_spans() {
        let mut bytes = Vec::new();
        for i in 0..1000 {
            bytes.extend_from_slice(format!("row {i}\n").as_bytes());
        }
        let t = tmp(&bytes);
        let idx = index_of(&t);
        assert_eq!(idx.rows(), 1000);
        for i in [0, 1, 500, 999] {
            let s = idx.span(i).unwrap();
            assert_eq!(
                &bytes[s.start as usize..s.end as usize],
                format!("row {i}\n").as_bytes()
            );
        }
        let mut last = 0;
        for i in 0..1000 {
            let s = idx.span(i).unwrap();
            assert_eq!(s.start, last);
            last = s.end;
        }
        assert_eq!(last, idx.len);
        assert_eq!(idx.span(1000), None);
    }

    #[test]
    fn checkpoints_and_big_gap() {
        // one gap past u32::MAX forces a checkpoint off the 64 grid
        let mut starts = Vec::new();
        let mut at = 0u64;
        for i in 0..1000u64 {
            starts.push(at);
            at += if i == 500 { 5_000_000_000 } else { 3 + i % 7 };
        }
        let len = at + 5;
        starts.push(len);
        let mut b = Builder::default();
        for &s in &starts[..1000] {
            assert!(b.push(s));
        }
        let idx = Index {
            path: PathBuf::new(),
            len,
            modified: None,
            delimiter: b',',
            rows: b.rows,
            checks: b.checks,
            deltas: b.deltas,
        };
        for i in 0..1000 {
            assert_eq!(idx.span(i), Some(starts[i]..starts[i + 1]), "row {i}");
        }
        assert_eq!(idx.span(1000), None);
    }

    #[test]
    fn ranges_across_blocks() {
        let mut text = String::new();
        for i in 0..60_000 {
            text.push_str(&format!("{i},row {i} {}\n", "y".repeat(i % 50)));
        }
        let t = tmp(text.as_bytes());
        let idx = index_of(&t);
        let n = idx.rows();
        assert_eq!(n, 60_000);
        let edge = (0..n)
            .find(|&i| idx.span(i).unwrap().start >= 256 * 1024)
            .unwrap();
        let mut r = Reader::open(Arc::new(idx)).unwrap();
        let got = r.rows(edge - 3..edge + 3).unwrap();
        assert_eq!(got.len(), 6);
        for (k, row) in got.iter().enumerate() {
            let i = edge - 3 + k;
            assert_eq!(row[0], i.to_string());
            assert_eq!(row[1], format!("row {i} {}", "y".repeat(i % 50)));
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..200 {
            seed = xorshift(seed);
            let i = (seed % n as u64) as usize;
            let row = r.rows(i..i + 1).unwrap();
            assert_eq!(row[0][0], i.to_string());
        }
        let all = r.rows(0..n).unwrap();
        assert_eq!(all.len(), n);
        assert!(r.blocks.len() <= MAX_BLOCKS);
    }

    #[test]
    fn cache_hit_serves_cached_block() {
        let t = tmp(b"a,1\nb,2\n");
        let mut r = Reader::open(Arc::new(index_of(&t))).unwrap();
        let first = r.rows(0..2).unwrap();
        assert_eq!(first, vec![cells(&["a", "1"]), cells(&["b", "2"])]);
        // same length, new bytes: the cached block still answers with the old ones.
        fs::write(&t.0, b"z,9\nz,9\n").unwrap();
        assert_eq!(r.rows(0..2).unwrap(), first);
        assert_eq!(r.blocks.len(), 1);
    }

    #[test]
    fn file_changed_after_append() {
        let t = tmp(b"a\nb\n");
        let mut r = Reader::open(Arc::new(index_of(&t))).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&t.0)
            .unwrap()
            .write_all(b"c\n")
            .unwrap();
        assert_eq!(r.rows(0..2).unwrap_err(), "file changed");
    }

    #[test]
    fn stop_flag_returns_early() {
        let t = tmp(b"a,b\n");
        let stop = AtomicBool::new(true);
        assert_eq!(Index::build(&t.0, |_| {}, &stop).unwrap_err(), "stopped");

        let big = tmp(&b"abc,def\n".repeat(1_250_000));
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        let got = Index::build(
            &big.0,
            |_| {
                calls += 1;
                stop.store(true, Ordering::Relaxed);
            },
            &stop,
        );
        assert_eq!(got.unwrap_err(), "stopped");
        assert_eq!(calls, 1);
    }

    #[test]
    fn cell_caps() {
        let wide = format!("{}x\n", "x,".repeat(299));
        let long = format!("{},z\n", "q".repeat(5000));
        let han = format!("{},z\n", "가".repeat(2000));
        let huge = format!("{}\nb\n", "x".repeat(1_500_000));
        let t = tmp(format!("{wide}{long}{han}{huge}").as_bytes());
        let rows = read_rows(index_of(&t));
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].len(), 256);
        assert_eq!(rows[1][0].len(), 1024);
        assert!(rows[1][0].ends_with('…'));
        assert!(rows[2][0].len() <= 1024);
        assert!(rows[2][0].ends_with('…'));
        assert!(rows[2][0].trim_end_matches('…').chars().all(|c| c == '가'));
        // the 1.5 MiB row is cut at 1 MiB, then its cell at 1 KiB
        assert_eq!(rows[3].len(), 1);
        assert!(rows[3][0].ends_with('…') && rows[3][0].len() <= 1024);
        assert_eq!(rows[4], cells(&["b"]));
    }

    #[test]
    fn split_cells() {
        assert_eq!(
            split(b"a,\"b,\"\"c\"\"\",d\n", b','),
            cells(&["a", "b,\"c\"", "d"])
        );
        assert_eq!(split(b"", b','), Vec::<String>::new());
        assert_eq!(split(b"a,\r\n", b','), cells(&["a", ""]));
        assert_eq!(split(b"\"\"\"\"", b','), cells(&["\""]));
        assert_eq!(split(b"a\tb", b'\t'), cells(&["a", "b"]));
    }

    #[test]
    fn fuzz_small_files() {
        let toks = [",", "\"", "\n", "\r", "\t", "a", "1", "가", "\r\n", "\"\""];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for n in 0..300 {
            seed = xorshift(seed);
            let count = (seed % 64) as usize;
            let mut bytes = Vec::new();
            for _ in 0..count {
                seed = xorshift(seed);
                bytes.extend_from_slice(toks[(seed % toks.len() as u64) as usize].as_bytes());
            }
            // split must not panic on any bytes either
            let _ = split(&bytes, b',');
            let _ = split(&bytes, b'\t');

            let t = tmp(&bytes);
            let idx = index_of(&t);
            let rows = idx.rows();
            let mut last = 0;
            for i in 0..rows {
                let s = idx.span(i).unwrap();
                assert!(s.start < s.end && s.end <= idx.len, "file {n} row {i}");
                assert_eq!(s.start, last, "file {n} row {i}");
                last = s.end;
            }
            assert!(idx.span(rows).is_none());
            let mut r = Reader::open(Arc::new(idx)).unwrap();
            assert_eq!(r.rows(0..rows).unwrap().len(), rows, "file {n}");
        }
    }

    #[test]
    #[ignore]
    fn index_200mb_perf() {
        let path =
            std::env::temp_dir().join(format!("ligase_rows_perf_{}.csv", std::process::id()));
        let _guard = Tmp(path.clone());
        {
            let mut w = BufWriter::new(File::create(&path).unwrap());
            let mut written = 0u64;
            let mut i = 0u64;
            while written < 200 << 20 {
                let line = format!("{i},name {i},\"quoted, text {i}\",{}\n", i * 3);
                w.write_all(line.as_bytes()).unwrap();
                written += line.len() as u64;
                i += 1;
            }
            w.flush().unwrap();
        }
        let start = Instant::now();
        let idx = Index::build(&path, |_| {}, &AtomicBool::new(false)).unwrap();
        let took = start.elapsed();
        let bytes = idx.deltas.len() * 4 + idx.checks.len() * 16;
        println!(
            "rows {} in {:?}, index {} bytes ({:.2} per row)",
            idx.rows(),
            took,
            bytes,
            bytes as f64 / idx.rows() as f64
        );
    }
}
