//! Convert source byte offsets to one-based UTF-16 coordinates per module.

use std::collections::BTreeMap;

pub(crate) struct SourceLines {
    starts: Vec<(usize, usize)>,
    // Byte boundary after a non-ASCII scalar and cumulative UTF-8 excess.
    // Sparse corrections avoid rescanning the prefix of a minified line.
    corrections: Vec<(usize, usize)>,
    positions: BTreeMap<u32, (u32, u32)>,
}

impl SourceLines {
    pub(crate) fn new(source: &str) -> Self {
        let mut starts = vec![(0, 0)];
        let mut corrections = Vec::new();
        let mut excess = 0;
        let mut chars = source.char_indices().peekable();
        while let Some((offset, ch)) = chars.next() {
            let end = offset + ch.len_utf8();
            if !ch.is_ascii() {
                excess += ch.len_utf8() - ch.len_utf16();
                corrections.push((end, excess));
            }
            match ch {
                '\r' if chars.next_if(|&(_, next)| next == '\n').is_some() => {
                    starts.push((offset + 2, excess));
                }
                '\r' | '\n' | '\u{2028}' | '\u{2029}' => starts.push((end, excess)),
                _ => {}
            }
        }
        Self {
            starts,
            corrections,
            positions: BTreeMap::new(),
        }
    }

    pub(crate) fn position(&mut self, offset: u32) -> (u32, u32) {
        *self.positions.entry(offset).or_insert_with(|| {
            let offset = offset as usize;
            let line = self.starts.partition_point(|&(start, _)| start <= offset);
            let (start, start_excess) = self.starts[line - 1];
            let correction = self.corrections.partition_point(|&(end, _)| end <= offset);
            let excess = correction
                .checked_sub(1)
                .map_or(0, |i| self.corrections[i].1);
            let column = (offset - start) - (excess - start_excess) + 1;
            (line as u32, column as u32)
        })
    }
}
