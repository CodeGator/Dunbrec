//! Intel HEX, the text format AVR toolchains use for firmware images.
//!
//! Each line is a record `:LLAAAATT<data>CC`: byte count, address, record
//! type, data and a checksum. Only data, end-of-file and the two address
//! extension records matter for flash images.

use std::fmt::Write;

/// Parse an Intel HEX file into a flat image starting at address 0.
/// Unwritten bytes are 0xFF (erased flash).
pub fn parse(text: &str) -> Result<Vec<u8>, String> {
    let mut image: Vec<u8> = Vec::new();
    let mut base: u32 = 0;
    let mut saw_eof = false;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let line_no = n + 1;
        let body = line
            .strip_prefix(':')
            .ok_or_else(|| format!("line {line_no}: records must start with ':'"))?;
        if body.len() % 2 != 0 || body.len() < 10 {
            return Err(format!("line {line_no}: malformed record"));
        }
        let bytes = (0..body.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&body[i..i + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| format!("line {line_no}: not hexadecimal"))?;
        let count = bytes[0] as usize;
        if bytes.len() != count + 5 {
            return Err(format!("line {line_no}: byte count does not match"));
        }
        if bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 {
            return Err(format!("line {line_no}: checksum error"));
        }
        let address = u16::from_be_bytes([bytes[1], bytes[2]]) as u32;
        let data = &bytes[4..4 + count];
        match bytes[3] {
            0x00 => {
                let start = (base + address) as usize;
                let end = start + count;
                if end > 0x40_0000 {
                    return Err(format!("line {line_no}: address out of range"));
                }
                if image.len() < end {
                    image.resize(end, 0xFF);
                }
                image[start..end].copy_from_slice(data);
            }
            0x01 => {
                saw_eof = true;
                break;
            }
            0x02 if count == 2 => base = (u16::from_be_bytes([data[0], data[1]]) as u32) << 4,
            0x04 if count == 2 => base = (u16::from_be_bytes([data[0], data[1]]) as u32) << 16,
            // Start addresses: irrelevant for AVR, which starts at 0.
            0x03 | 0x05 => {}
            t => return Err(format!("line {line_no}: unsupported record type {t:02X}")),
        }
    }
    if !saw_eof {
        return Err("missing end-of-file record (is the file complete?)".into());
    }
    if image.is_empty() {
        return Err("the file contains no data".into());
    }
    Ok(image)
}

/// Write an image as Intel HEX (16 data bytes per record).
pub fn write(image: &[u8]) -> String {
    let mut out = String::new();
    let mut record = |kind: u8, address: u16, data: &[u8]| {
        let mut sum = data.len() as u8;
        sum = sum.wrapping_add((address >> 8) as u8);
        sum = sum.wrapping_add(address as u8);
        sum = sum.wrapping_add(kind);
        let _ = write!(out, ":{:02X}{address:04X}{kind:02X}", data.len());
        for &b in data {
            sum = sum.wrapping_add(b);
            let _ = write!(out, "{b:02X}");
        }
        let _ = writeln!(out, "{:02X}", sum.wrapping_neg());
    };
    let mut segment = 0usize;
    for (i, chunk) in image.chunks(16).enumerate() {
        let addr = i * 16;
        if addr >> 16 != segment {
            segment = addr >> 16;
            record(0x04, 0, &(segment as u16).to_be_bytes());
        }
        record(0x00, addr as u16, chunk);
    }
    record(0x01, 0, &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_avr_file() {
        // The start of an avr-gcc blink: a jump table entry and some code.
        let text = ":100000000C9434000C943E000C943E000C943E0082\n\
                    :00000001FF\n";
        let image = parse(text).unwrap();
        assert_eq!(image.len(), 16);
        assert_eq!(&image[..4], &[0x0C, 0x94, 0x34, 0x00]);
    }

    #[test]
    fn round_trip_and_errors() {
        let image: Vec<u8> = (0..70u32).map(|i| (i * 7) as u8).collect();
        assert_eq!(parse(&write(&image)).unwrap(), image);
        assert!(
            parse(":0100000000FE\n:00000001FF")
                .unwrap_err()
                .contains("checksum")
        );
        assert!(parse(":100000000C9434000C943E000C943E000C943E0082").is_err());
        assert!(parse("hello").is_err());
        // Gaps are erased flash.
        let gap = ":0100040011EA\n:00000001FF\n";
        assert_eq!(parse(gap).unwrap(), vec![0xFF, 0xFF, 0xFF, 0xFF, 0x11]);
    }
}
