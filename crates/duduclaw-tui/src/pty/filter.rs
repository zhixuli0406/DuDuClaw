//! Bounded, streaming VT prefilter. String controls never reach vte's buffers.
const MAX_CSI: usize = 128;
const MAX_PARAMS: usize = 32;

#[derive(Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    String { osc: bool, escape: bool },
    Csi,
    DiscardCsi,
}

pub(super) struct Filter {
    state: State,
    csi: [u8; MAX_CSI],
    len: usize,
    params: usize,
    utf8_left: u8,
    utf8_min: u8,
    utf8_max: u8,
}
impl Default for Filter {
    fn default() -> Self {
        Self { state: State::Ground, csi: [0; MAX_CSI], len: 0, params: 1,
            utf8_left: 0, utf8_min: 0x80, utf8_max: 0xbf }
    }
}
impl Filter {
    // C1 bytes inside valid UTF-8 are text, not control introducers/terminators.
    fn continuation(&mut self, byte: u8) -> bool {
        if self.utf8_left > 0 && (self.utf8_min..=self.utf8_max).contains(&byte) {
            self.utf8_left -= 1;
            self.utf8_min = 0x80;
            self.utf8_max = 0xbf;
            return true;
        }
        self.utf8_left = 0;
        self.utf8_min = 0x80;
        self.utf8_max = 0xbf;
        match byte {
            0xc2..=0xdf => self.utf8_left = 1,
            0xe0..=0xef => {
                self.utf8_left = 2;
                if byte == 0xe0 { self.utf8_min = 0xa0; }
                if byte == 0xed { self.utf8_max = 0x9f; }
            }
            0xf0..=0xf4 => {
                self.utf8_left = 3;
                if byte == 0xf0 { self.utf8_min = 0x90; }
                if byte == 0xf4 { self.utf8_max = 0x8f; }
            }
            _ => {},
        }
        false
    }
    fn start_csi(&mut self) {
        self.state = State::Csi;
        self.len = 0;
        self.params = 1;
    }
    /// Output is delivered in fixed-size batches; neither input size nor an
    /// unfinished sequence can grow this filter's storage.
    pub fn process(&mut self, bytes: &[u8], rows: u16, cols: u16, mut emit: impl FnMut(&[u8])) {
        let mut output = [0; 4096];
        let mut used = 0;
        let mut push = |byte| {
            output[used] = byte;
            used += 1;
            if used == output.len() { emit(&output); used = 0; }
        };
        for &byte in bytes {
            let continuation = self.continuation(byte);
            if let State::String { osc, escape } = self.state {
                if !continuation && (byte == 0x9c || (osc && byte == 7) || (escape && byte == b'\\')) {
                    self.state = State::Ground;
                } else {
                    self.state = State::String { osc, escape: byte == 0x1b };
                }
                continue;
            }
            if !continuation {
                match byte {
                    0x90 | 0x98 | 0x9d..=0x9f => {
                        self.state = State::String { osc: byte == 0x9d, escape: false };
                        continue;
                    }
                    0x9b => { self.start_csi(); continue; }
                    0x9c => { self.state = State::Ground; continue; }
                    0x1b => { self.state = State::Escape; continue; }
                    0x18 | 0x1a => { self.state = State::Ground; push(byte); continue; }
                    _ => {},
                }
            }
            match self.state {
                State::Ground => push(byte),
                State::Escape => match byte {
                    b']' | b'P' | b'_' | b'^' | b'X' => {
                        self.state = State::String { osc: byte == b']', escape: false };
                    }
                    b'[' => self.start_csi(),
                    _ => { push(0x1b); push(byte); self.state = State::Ground; }
                },
                State::Csi => {
                    if matches!(byte, b';' | b':') { self.params += 1; }
                    if self.len == MAX_CSI || self.params > MAX_PARAMS {
                        self.state = State::DiscardCsi;
                    } else if (0x20..=0x7e).contains(&byte) {
                        self.csi[self.len] = byte;
                        self.len += 1;
                        if (0x40..=0x7e).contains(&byte) {
                            self.emit_csi(rows, cols, &mut push);
                            self.state = State::Ground;
                        }
                    } else {
                        // Do not let invalid or embedded controls reach a partial CSI.
                        self.state = State::DiscardCsi;
                    }
                    if matches!(self.state, State::DiscardCsi) && (0x40..=0x7e).contains(&byte) {
                        self.state = State::Ground;
                    }
                }
                State::DiscardCsi => {
                    if (0x40..=0x7e).contains(&byte) { self.state = State::Ground; }
                }
                State::String { .. } => unreachable!(),
            }
        }
        if used > 0 { emit(&output[..used]); }
    }
    fn emit_csi(&self, rows: u16, cols: u16, push: &mut impl FnMut(u8)) {
        push(0x1b);
        push(b'[');
        // Only count/position commands are bounded. Mode identifiers, SGR
        // colors and private extensions must retain their original parameters.
        let final_byte = self.csi[self.len - 1];
        let clamp = matches!(final_byte,
            b'A' | b'B' | b'C' | b'D' | b'E' | b'F' | b'G' | b'H' | b'I' |
            b'L' | b'M' | b'P' | b'R' | b'S' | b'T' | b'U' | b'V' | b'X' | b'Y' | b'Z' |
            b'@' | b'^' | b'`' | b'a' | b'b' | b'd' | b'e' | b'f' | b'j' | b'k' | b'r')
            && !self.csi[..self.len].iter().any(|byte| matches!(byte, b'?' | b'>' | b'=' | b'<'));
        if !clamp {
            for &byte in &self.csi[..self.len] { push(byte); }
            return;
        }
        let limit = if final_byte == b'r' { rows.max(1) } else { rows.max(cols).max(1) };
        let mut i = 0;
        while i < self.len {
            if self.csi[i].is_ascii_digit() {
                let start = i;
                let mut value = 0u16;
                while i < self.len && self.csi[i].is_ascii_digit() {
                    value = value.saturating_mul(10).saturating_add(u16::from(self.csi[i] - b'0'));
                    i += 1;
                }
                if value <= limit {
                    for &byte in &self.csi[start..i] { push(byte); }
                } else {
                    let mut digits = [0; 5];
                    let mut n = limit;
                    let mut pos = digits.len();
                    loop {
                        pos -= 1; digits[pos] = b'0' + (n % 10) as u8; n /= 10;
                        if n == 0 { break; }
                    }
                    for &byte in &digits[pos..] { push(byte); }
                }
            } else {
                push(self.csi[i]); i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn filtered(chunks: &[&[u8]], rows: u16, cols: u16) -> Vec<u8> {
        let mut filter = Filter::default();
        let mut output = Vec::new();
        for chunk in chunks { filter.process(chunk, rows, cols, |bytes| output.extend_from_slice(bytes)); }
        output
    }
    #[test]
    fn every_chunk_boundary_clamps_count_and_position_parameters() {
        let input = b"before\x1b[65535;99999:12345;00002Hafter\x9b99999L";
        let expected = b"before\x1b[80;80:80;00002Hafter\x1b[80L";
        for split in 0..=input.len() {
            assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), expected, "split {split}");
        }
        let chunks: Vec<_> = input.chunks(1).collect();
        assert_eq!(filtered(&chunks, 24, 80), expected);
        assert_eq!(filtered(&[b"\x1b[99L"], 0, 0), b"\x1b[1L");
        assert_eq!(filtered(&[b"\x1b[999L"], 120, 20), b"\x1b[120L");
        // Use the size at completion, including a resize during a sequence.
        let mut filter = Filter::default();
        filter.process(b"\x1b[655", 200, 500, |_| panic!("unfinished CSI emitted"));
        let mut output = Vec::new();
        filter.process(b"35L", 6, 20, |safe| output.extend_from_slice(safe));
        assert_eq!(output, b"\x1b[20L");
    }
    #[test]
    fn modes_colors_and_other_non_count_sequences_are_unchanged() {
        for input in [
            b"\x1b[?1049h".as_slice(), b"\x1b[?2004h", b"\x1b[?25l",
            b"\x1b[38;5;208m", b"\x1b[38;2;255;128;0m",
            b"\x1b[38:2::255:128:0m", b"\x1b[>65535A", b"\x1b[=65535L",
            b"\x1b[<65535T", b"\x1b[?65535r",
            b"\x1b[65535h", b"\x1b[65535l", b"\x1b[65535n", b"\x1b[65535c",
            b"\x1b[65535t", b"\x1b[65535s", b"\x1b[65535u", b"\x1b[65535z",
        ] {
            for split in 0..=input.len() {
                assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), input, "input {input:?}, split {split}");
            }
            assert_eq!(filtered(&input.chunks(1).collect::<Vec<_>>(), 24, 80), input);
        }
    }
    #[test]
    fn count_sequences_still_clamp_and_scroll_region_stays_within_rows() {
        for &final_byte in b"ABCDEFGHILMPRSTUVXYZ@^`abdefjkr" {
            let input = [b'\x1b', b'[', b'6', b'5', b'5', b'3', b'5', final_byte];
            let limit = if final_byte == b'r' { b"24" } else { b"80" };
            let expected = [b"\x1b[".as_slice(), limit, &[final_byte]].concat();
            for split in 0..=input.len() {
                assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), expected, "final {final_byte}, split {split}");
            }
        }
        assert_eq!(filtered(&[b"\x1b[1;65535r"], 24, 80), b"\x1b[1;24r");
    }
    #[test]
    fn all_string_controls_drop_without_buffering_across_every_boundary() {
        for intro in [b"\x1b]".as_slice(), b"\x1bP", b"\x1b_", b"\x1b^", b"\x1bX", b"\x9d", b"\x90", b"\x9f", b"\x9e", b"\x98"] {
            for end in [b"\x1b\\".as_slice(), b"\x9c"] {
                let input = [b"ok", intro, b"hidden\x1b[65535L\x1b]nested", end, b"done"].concat();
                for split in 0..=input.len() {
                    assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), b"okdone");
                }
                assert_eq!(filtered(&input.chunks(1).collect::<Vec<_>>(), 24, 80), b"okdone");
            }
        }
        assert_eq!(filtered(&[b"\x9dhidden\x07ok"], 24, 80), b"ok");
        assert_eq!(filtered(&[b"\x1bPhidden\x07still hidden\x1b\\ok"], 24, 80), b"ok");
    }
    #[test]
    fn multi_mib_unterminated_osc_has_constant_storage_and_zero_parser_input() {
        let mut filter = Filter::default();
        let mut parser = vt100::Parser::new(24, 80, 0);
        let fixed_size = std::mem::size_of_val(&filter);
        assert!(fixed_size <= 256);
        filter.process(b"\x1b]52;", 24, 80, |_| panic!("OSC reached parser"));
        let chunk = [b'x'; 4096];
        for _ in 0..2048 {
            filter.process(&chunk, 24, 80, |safe| { parser.process(safe); panic!("OSC reached parser"); });
            assert_eq!(filter.len, 0);
            assert_eq!(std::mem::size_of_val(&filter), fixed_size);
        }
        // Filter has no heap fields; the parser receives no payload at all.
        assert_eq!(parser.screen().contents(), "");
        filter.process(b"\x1b\\ok", 24, 80, |safe| parser.process(safe));
        assert_eq!(parser.screen().contents(), "ok");
    }
    #[test]
    fn csi_length_and_parameter_count_are_bounded() {
        for payload in [vec![b'9'; 4 * 1024 * 1024], b"1;".repeat(100), b"1:".repeat(100)] {
            let input = [b"\x1b[", payload.as_slice(), b"Lsafe"].concat();
            let mut filter = Filter::default();
            let mut output = Vec::new();
            for chunk in input.chunks(37) {
                filter.process(chunk, 24, 80, |safe| output.extend_from_slice(safe));
                assert!(filter.len <= MAX_CSI);
                assert!(filter.params <= MAX_PARAMS + 1);
            }
            assert_eq!(output, b"safe");
        }
    }
    #[test]
    fn repeated_large_csi_filter_and_parser_finish_within_time_bound() {
        let mut filter = Filter::default();
        let mut parser = vt100::Parser::new(24, 80, 0);
        let start = Instant::now();
        for _ in 0..1000 {
            filter.process(b"\x1b[65535L\x1b[65535T", 24, 80, |safe| parser.process(safe));
        }
        assert!(start.elapsed() < Duration::from_secs(3), "elapsed {:?}", start.elapsed());
    }
    #[test]
    fn shell_output_colors_and_utf8_are_unchanged_at_every_boundary() {
        let input = "user$ echo 繁體\r\n\x1b[1;31m紅色\x1b[0m café e\u{301} 🌍 🜀 🜛 🜜 🜝 🜞 🜟\r\n".as_bytes();
        for split in 0..=input.len() {
            assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), input);
        }
        assert_eq!(filtered(&input.chunks(1).collect::<Vec<_>>(), 24, 80), input);
        // A UTF-8 continuation equal to ST must not end a dropped string.
        assert_eq!(filtered(&["\x1b]🜜hidden\x07ok".as_bytes()], 24, 80), b"ok");
    }
}
