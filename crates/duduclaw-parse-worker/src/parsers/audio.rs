//! Bounded container/frame inspection. No codec, external process or network.
//! This verifies structure, not that compressed audio is safe to decode elsewhere.
use duduclaw_isolation::worker::{AudioFormat, ParseError, ParsedItem};

fn invalid<T>() -> Result<T, ParseError> { Err(ParseError::InvalidContent) }
fn u16le(bytes: &[u8]) -> Result<u16, ParseError> {
    Ok(u16::from_le_bytes(bytes.try_into().map_err(|_| ParseError::InvalidContent)?))
}
fn u32le(bytes: &[u8]) -> Result<u32, ParseError> {
    Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| ParseError::InvalidContent)?))
}
pub(super) fn inspect(bytes: &[u8], slot: u16) -> Result<ParsedItem, ParseError> {
    if bytes.len() > 25 * 1024 * 1024 { return Err(ParseError::LimitExceeded); }
    if bytes.get(..4) == Some(b"RIFF") { wav(bytes, slot) } else { mp3(bytes, slot) }
}
fn wav(bytes: &[u8], slot: u16) -> Result<ParsedItem, ParseError> {
    if bytes.len() < 12 || bytes.get(8..12) != Some(b"WAVE")
        || u32le(&bytes[4..8])? as usize != bytes.len() - 8 { return invalid(); }
    let mut cursor = 12usize;
    let mut format = None;
    let mut data = None;
    let mut chunks = 0;
    while cursor < bytes.len() {
        chunks += 1;
        if chunks > 4096 { return Err(ParseError::LimitExceeded); }
        let header = bytes.get(cursor..cursor + 8).ok_or(ParseError::InvalidContent)?;
        let size = u32le(&header[4..8])? as usize;
        cursor += 8;
        let end = cursor.checked_add(size).ok_or(ParseError::InvalidContent)?;
        let body = bytes.get(cursor..end).ok_or(ParseError::InvalidContent)?;
        match &header[..4] {
            b"fmt " => {
                if format.is_some() || body.len() < 16 || !matches!(u16le(&body[..2])?, 1 | 3) { return invalid(); }
                let channels = u16le(&body[2..4])?;
                let rate = u32le(&body[4..8])?;
                let align = u16le(&body[12..14])?;
                let bits = u16le(&body[14..16])?;
                if !(1..=8).contains(&channels) || !(8000..=192000).contains(&rate)
                    || !matches!(bits, 8 | 16 | 24 | 32) || align != channels * (bits / 8)
                    || u32le(&body[8..12])? != rate * u32::from(align)
                    || (u16le(&body[..2])? == 3 && bits != 32) { return invalid(); }
                format = Some((channels, rate, align));
            }
            b"data" => { if data.replace(size).is_some() { return invalid(); } }
            _ => {},
        }
        cursor = end.checked_add(size % 2).ok_or(ParseError::InvalidContent)?;
        if cursor > bytes.len() { return invalid(); }
    }
    let (channels, sample_rate, align) = format.ok_or(ParseError::InvalidContent)?;
    let data = data.ok_or(ParseError::InvalidContent)?;
    if data == 0 || data % usize::from(align) != 0 { return invalid(); }
    Ok(ParsedItem::Audio { input_slot: slot, format: AudioFormat::Wav, sample_rate, channels,
        frames: (data / usize::from(align)) as u64 })
}
fn mp3(bytes: &[u8], slot: u16) -> Result<ParsedItem, ParseError> {
    let mut cursor = 0usize;
    if bytes.get(..3) == Some(b"ID3") {
        let tag = bytes.get(..10).ok_or(ParseError::InvalidContent)?;
        if !matches!(tag[3], 3 | 4) || tag[4] != 0 || tag[5] & 0x0f != 0
            || tag[6..10].iter().any(|b| b & 0x80 != 0) { return invalid(); }
        let size = tag[6..10].iter().fold(0usize, |size, byte| (size << 7) | usize::from(*byte));
        cursor = 10 + size + if tag[3] == 4 && tag[5] & 0x10 != 0 { 10 } else { 0 };
        if cursor > bytes.len() { return invalid(); }
    }
    let mut audio = None;
    let mut frames = 0u64;
    while cursor < bytes.len() {
        if bytes.len() - cursor == 128 && bytes.get(cursor..cursor + 3) == Some(b"TAG") { break; }
        let h = bytes.get(cursor..cursor + 4).ok_or(ParseError::InvalidContent)?;
        if h[0] != 0xff || h[1] & 0xe0 != 0xe0 || h[1] & 6 != 2 { return invalid(); }
        let version = (h[1] >> 3) & 3;
        let bitrate = (h[2] >> 4) as usize;
        let frequency = ((h[2] >> 2) & 3) as usize;
        if version == 1 || bitrate == 0 || bitrate == 15 || frequency == 3 || h[3] & 3 == 2 { return invalid(); }
        let rate = [44100, 48000, 32000][frequency] / match version { 3 => 1, 2 => 2, _ => 4 };
        let kbps = if version == 3 { [0,32,40,48,56,64,80,96,112,128,160,192,224,256,320][bitrate] }
            else { [0,8,16,24,32,40,48,56,64,80,96,112,128,144,160][bitrate] };
        let channels = if h[3] >> 6 == 3 { 1 } else { 2 };
        let current = (rate, channels);
        if audio.replace(current).is_some_and(|old| old != current) { return invalid(); }
        let length = (if version == 3 { 144000 } else { 72000 }) * kbps / rate + u32::from((h[2] >> 1) & 1);
        if length < 4 { return invalid(); }
        cursor = cursor.checked_add(length as usize).ok_or(ParseError::InvalidContent)?;
        if cursor > bytes.len() { return invalid(); }
        frames += if version == 3 { 1152 } else { 576 };
    }
    let (sample_rate, channels) = audio.ok_or(ParseError::UnsupportedFormat)?;
    Ok(ParsedItem::Audio { input_slot: slot, format: AudioFormat::Mp3, sample_rate, channels, frames })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn i1c_audio_checks_every_mp3_frame_and_truncation() {
        let mut bytes = vec![0; 417 * 2];
        bytes[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0]);
        bytes[417..421].copy_from_slice(&[0xff, 0xfb, 0x90, 0]);
        assert!(matches!(inspect(&bytes, 0).unwrap(), ParsedItem::Audio { frames: 2304, .. }));
        bytes.pop();
        assert!(inspect(&bytes, 0).is_err());
        assert!(inspect(b"ID3", 0).is_err());
        assert!(inspect(b"not audio", 0).is_err());
    }
    #[test]
    fn i1c_audio_rejects_fake_riff_and_inconsistent_pcm() {
        let mut bytes = Vec::from(&b"RIFF"[..]);
        bytes.extend(38u32.to_le_bytes()); bytes.extend(b"WAVEfmt "); bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes()); bytes.extend(1u16.to_le_bytes()); bytes.extend(16000u32.to_le_bytes());
        bytes.extend(32000u32.to_le_bytes()); bytes.extend(2u16.to_le_bytes()); bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data"); bytes.extend(2u32.to_le_bytes()); bytes.extend([0,0]);
        assert!(matches!(inspect(&bytes, 0).unwrap(), ParsedItem::Audio { frames: 1, .. }));
        bytes[28] = 1; assert!(inspect(&bytes, 0).is_err());
        assert!(inspect(b"RIFFxxxxWAVE", 0).is_err());
    }
}
