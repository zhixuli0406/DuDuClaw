//! Bounded, streaming VT prefilter. String controls never reach vte's buffers.
const MAX_CSI: usize = 128;
const MAX_PARAMS: usize = 32;

#[derive(Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Charset(u8),
    String { osc: bool, dcs: bool },
    Csi,
    DiscardCsi,
}

pub(super) struct Filter {
    state: State,
    csi: [u8; MAX_CSI],
    len: usize,
    params: usize,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_left: u8,
    utf8_min: u8,
    utf8_max: u8,
}
impl Default for Filter {
    fn default() -> Self {
        Self { state: State::Ground, csi: [0; MAX_CSI], len: 0, params: 1,
            utf8: [0; 4], utf8_len: 0, utf8_left: 0, utf8_min: 0x80, utf8_max: 0xbf }
    }
}
impl Filter {
    // Buffer a whole scalar before emitting it. Overlong encodings, surrogates,
    // out-of-range scalars and stray continuation/C1 bytes never reach vte.
    fn ground(&mut self, byte: u8, push: &mut impl FnMut(u8)) {
        if self.utf8_left > 0 {
            if (self.utf8_min..=self.utf8_max).contains(&byte) {
                self.utf8[self.utf8_len] = byte;
                self.utf8_len += 1;
                self.utf8_left -= 1;
                self.utf8_min = 0x80;
                self.utf8_max = 0xbf;
                if self.utf8_left == 0 {
                    for &byte in &self.utf8[..self.utf8_len] { push(byte); }
                    self.utf8_len = 0;
                }
                return;
            }
            // Drop the incomplete scalar; reconsider this byte as fresh input.
            self.utf8_left = 0;
            self.utf8_len = 0;
        }
        self.utf8_min = 0x80;
        self.utf8_max = 0xbf;
        match byte {
            0x1b => self.state = State::Escape,
            0x00..=0x7f => push(byte),
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
        if self.utf8_left > 0 {
            self.utf8[0] = byte;
            self.utf8_len = 1;
        }
    }
    fn start_csi(&mut self) {
        self.state = State::Csi;
        self.len = 0;
        self.params = 1;
    }
    /// Output is delivered in fixed-size batches; neither input size nor an
    /// unfinished sequence can grow this filter's storage. ESC is emitted only
    /// after an entire whitelisted sequence has been validated.
    pub fn process(&mut self, bytes: &[u8], rows: u16, cols: u16, mut emit: impl FnMut(&[u8])) {
        let mut output = [0; 4096];
        let mut used = 0;
        let mut push = |byte| {
            output[used] = byte;
            used += 1;
            if used == output.len() { emit(&output); used = 0; }
        };
        for &byte in bytes {
            // vte cancels every non-ground state on CAN/SUB and enters Escape
            // on ESC (including inside OSC, DCS, SOS, PM and APC). No partial
            // sequence has been emitted, so the downstream parser stays Ground.
            if !matches!(self.state, State::Ground) {
                match byte {
                    0x18 | 0x1a => { self.state = State::Ground; push(byte); continue; }
                    0x1b => { self.state = State::Escape; continue; }
                    _ => {},
                }
            }
            match self.state {
                State::Ground => self.ground(byte, &mut push),
                State::Escape => match byte {
                    b']' | b'P' | b'_' | b'^' | b'X' => {
                        self.state = State::String { osc: byte == b']', dcs: byte == b'P' };
                    }
                    b'[' => self.start_csi(),
                    b'(' | b')' | b'*' | b'+' => self.state = State::Charset(byte),
                    // vt100 handles 7, 8, =, >, M, c and visual bell g;
                    // the remaining listed VT escapes also finish vte's Escape.
                    b'7' | b'8' | b'=' | b'>' | b'c' | b'D' | b'E' | b'H' |
                    b'M' | b'Z' | b'\\' | b'g' => {
                        push(0x1b); push(byte); self.state = State::Ground;
                    }
                    _ => {
                        self.state = State::Ground;
                        self.ground(byte, &mut push);
                    }
                },
                State::Charset(designator) => {
                    self.state = State::Ground;
                    if (0x30..=0x7e).contains(&byte) {
                        push(0x1b); push(designator); push(byte);
                    } else {
                        self.ground(byte, &mut push);
                    }
                }
                State::String { osc, dcs } => {
                    if (osc && byte == 7) || (dcs && byte == 0x9c) {
                        self.state = State::Ground;
                    }
                }
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
                        // Invalid bytes taint the entire CSI; keep dropping to
                        // a final or the cancellation/restart handled above.
                        self.state = State::DiscardCsi;
                    }
                    if matches!(self.state, State::DiscardCsi) && (0x40..=0x7e).contains(&byte) {
                        self.state = State::Ground;
                    }
                }
                State::DiscardCsi => {
                    if (0x40..=0x7e).contains(&byte) { self.state = State::Ground; }
                }
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
    #[derive(Default)]
    struct StringCalls { osc: usize, hook: usize, put: usize }
    impl vte::Perform for StringCalls {
        fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) { self.osc += 1; }
        fn hook(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) { self.hook += 1; }
        fn put(&mut self, _: u8) { self.put += 1; }
    }
    fn assert_no_strings(output: &[u8], parser: &mut vte::Parser, calls: &mut StringCalls) {
        assert!(!output.windows(2).any(|pair| pair[0] == 0x1b && b"]PX^_".contains(&pair[1])));
        parser.advance(calls, output);
        // Terminate a potential unfinished OSC/DCS too, exposing hidden state.
        parser.advance(calls, b"qpayload\x07\x1b\\");
        assert_eq!((calls.osc, calls.hook, calls.put), (0, 0, 0));
    }
    #[test]
    fn escape_desync_regressions_at_every_boundary() {
        for middle in [b"\xc2\x9d".as_slice(), b"\x00", b"\x07", b"\x1f", b"\x7f", b"\xff", b"\x1b"] {
            for &intro in b"]PX^_" {
                let input = [b"\x1b".as_slice(), middle, &[intro], b"payload\x07\x1b\\ok"].concat();
                for split in 0..=input.len() {
                    let output = filtered(&[&input[..split], &input[split..]], 24, 80);
                    assert_no_strings(&output, &mut vte::Parser::new(), &mut StringCalls::default());
                }
            }
        }
    }
    #[test]
    fn seeded_random_streams_never_reach_vte_string_handlers() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(0x4553_435f_5554_4638);
        let biased = b"\x1b\x1b]P^_X\x18\x1a\x7f\x90\x98\x9b\x9c\x9d\x9e\x9f\x80\xbf\xc0\xc2\xe0\xed\xf0\xf4\xff[();012m\x07\\";
        for _ in 0..128 {
            let input: Vec<u8> = (0..4096).map(|_| {
                if rng.gen_ratio(4, 5) { biased[rng.gen_range(0..biased.len())] } else { rng.r#gen() }
            }).collect();
            let mut filter = Filter::default();
            let mut parser = vte::Parser::new();
            let mut calls = StringCalls::default();
            let mut output = Vec::new();
            let mut offset = 0;
            while offset < input.len() {
                let end = (offset + rng.gen_range(1..=97)).min(input.len());
                filter.process(&input[offset..end], 24, 80, |safe| {
                    parser.advance(&mut calls, safe);
                    output.extend_from_slice(safe);
                });
                assert_eq!((calls.osc, calls.hook, calls.put), (0, 0, 0));
                offset = end;
            }
            assert_no_strings(&output, &mut vte::Parser::new(), &mut StringCalls::default());
            parser.advance(&mut calls, b"qpayload\x07\x1b\\");
            assert_eq!((calls.osc, calls.hook, calls.put), (0, 0, 0));
        }
    }
    #[test]
    fn every_chunk_boundary_clamps_count_and_position_parameters() {
        let input = b"before\x1b[65535;99999:12345;00002Hafter\x1b[99999L";
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
        for intro in [b"\x1b]".as_slice(), b"\x1bP", b"\x1b_", b"\x1b^", b"\x1bX"] {
            let input = [b"ok", intro, b"hidden", b"\x1b\\done"].concat();
            for split in 0..=input.len() {
                assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), b"ok\x1b\\done");
            }
            assert_eq!(filtered(&input.chunks(1).collect::<Vec<_>>(), 24, 80), b"ok\x1b\\done");
        }
        assert_eq!(filtered(&[b"\x1b]hidden\x07ok"], 24, 80), b"ok");
        assert_eq!(filtered(&[b"\x1bPhidden\x07still hidden\x1b\\ok"], 24, 80), b"\x1b\\ok");
        assert_eq!(filtered(&[b"\x1bPhidden\x9cok"], 24, 80), b"ok");
        // OSC ignores C1 ST, and SOS/PM/APC only leave on CAN/SUB/ESC in vte.
        for intro in [b"\x1b]".as_slice(), b"\x1bX", b"\x1b^", b"\x1b_"] {
            assert_eq!(filtered(&[intro, b"hidden\x9cstill hidden\x18ok"], 24, 80), b"\x18ok");
        }
    }
    #[test]
    fn cancellation_and_escape_restart_follow_vte_in_all_pending_states() {
        let long_csi = [b"\x1b[".as_slice(), &[b'1'; MAX_CSI + 1]].concat();
        for pending in [b"\x1b".as_slice(), b"\x1b(", b"\x1b[12", &long_csi, b"\x1b]hidden", b"\x1bPqhidden", b"\x1bXhidden", b"\x1b^hidden", b"\x1b_hidden"] {
            for cancel in [0x18, 0x1a] {
                let input = [pending, &[cancel], b"ok"].concat();
                for split in 0..=input.len() {
                    assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), [vec![cancel], b"ok".to_vec()].concat());
                }
            }
            let input = [pending, b"\x1b[31mok"].concat();
            assert_eq!(filtered(&input.chunks(1).collect::<Vec<_>>(), 24, 80), b"\x1b[31mok");
        }
    }
    #[test]
    fn only_complete_whitelisted_escapes_and_valid_utf8_are_emitted() {
        for &final_byte in b"78=>cDEHMZ\\g" {
            let input = [0x1b, final_byte];
            assert_eq!(filtered(&[&input[..1]], 24, 80), b"");
            assert_eq!(filtered(&[&input[..1], &input[1..]], 24, 80), input);
        }
        for &designator in b"()*+" {
            let input = [0x1b, designator, b'B'];
            assert_eq!(filtered(&[&input[..2]], 24, 80), b"");
            for split in 0..=input.len() {
                assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), input);
            }
        }
        for input in [b"\xc0\x80".as_slice(), b"\xed\xa0\x80", b"\xe0\x80\x80", b"\xf4\x90\x80\x80", b"\xf5\x80\x80\x80", b"\xc2", b"\xe0\xa0", b"\xf0\x90\x80"] {
            for split in 0..=input.len() {
                assert_eq!(filtered(&[&input[..split], &input[split..]], 24, 80), b"");
            }
        }
        let c1: Vec<_> = (0x80..=0x9f).collect();
        assert_eq!(filtered(&[&c1], 24, 80), b"");
        assert_eq!(filtered(&[b"\xc2\x1b[31mok"], 24, 80), b"\x1b[31mok");
        assert_eq!(filtered(&[b"\x1b\xc2\x9d]text"], 24, 80), b"\xc2\x9d]text");
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
