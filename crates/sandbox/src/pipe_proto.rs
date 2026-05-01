//! Binary wire protocol for Windows named-pipe sandbox IPC.
//!
//! Wire format (little-endian):
//! `[op: u8][pid: u32 LE][path_len: u16 LE][path_utf16: path_len × 2 bytes]`
//!
//! `path_len` is the number of UTF-16 code units, **not** bytes.

use crate::event::AccessEvent;

/// Operation code for a file-read event.
pub const OP_READ: u8 = 0x01;

/// Operation code for a file-write event.
pub const OP_WRITE: u8 = 0x02;

/// Length of the fixed wire header in bytes (`op` + `pid` + `path_len`).
pub const HEADER_LEN: usize = 7;

/// Encodes an [`AccessEvent`] into the binary wire format, appending bytes to `buf`.
///
/// Layout: `[op: u8][pid: u32 LE][path_len: u16 LE][path_utf16: path_len × 2 bytes]`
pub fn encode_event(event: &AccessEvent, buf: &mut Vec<u8>) {
    use byteorder::WriteBytesExt;

    let (op, path, pid) = match event {
        AccessEvent::Read { path, pid } => (OP_READ, path, *pid),
        AccessEvent::Write { path, pid } => (OP_WRITE, path, *pid),
    };

    let utf16: Vec<u16> = path.encode_utf16().collect();
    let path_len = utf16.len() as u16;

    buf.write_u8(op).expect("Vec write is infallible");
    buf.write_u32::<byteorder::LittleEndian>(pid)
        .expect("Vec write is infallible");
    buf.write_u16::<byteorder::LittleEndian>(path_len)
        .expect("Vec write is infallible");
    for word in &utf16 {
        buf.write_u16::<byteorder::LittleEndian>(*word)
            .expect("Vec write is infallible");
    }
}

/// Decodes one [`AccessEvent`] from the front of `buf`.
///
/// Returns `Some((event, consumed))` where `consumed` is the number of bytes
/// read from `buf`, or `None` if the buffer is too short or contains an unknown
/// op-code.  Does not panic on any input.
pub fn decode_event(buf: &[u8]) -> Option<(AccessEvent, usize)> {
    use byteorder::{ByteOrder, LittleEndian};

    if buf.len() < HEADER_LEN {
        return None;
    }

    let op = buf[0];
    let pid = LittleEndian::read_u32(&buf[1..5]);
    let path_words = LittleEndian::read_u16(&buf[5..7]) as usize;
    let path_bytes = path_words * 2;
    let total = HEADER_LEN + path_bytes;

    if buf.len() < total {
        return None;
    }

    let utf16: Vec<u16> = buf[HEADER_LEN..total]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let path = String::from_utf16_lossy(&utf16).to_string();

    let event = match op {
        OP_READ => AccessEvent::Read { path, pid },
        OP_WRITE => AccessEvent::Write { path, pid },
        _ => return None,
    };

    Some((event, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AccessEvent;

    // ------------------------------------------------------------------
    // helpers
    // ------------------------------------------------------------------

    fn make_read(path: &str, pid: u32) -> AccessEvent {
        AccessEvent::Read {
            path: path.to_owned(),
            pid,
        }
    }

    fn make_write(path: &str, pid: u32) -> AccessEvent {
        AccessEvent::Write {
            path: path.to_owned(),
            pid,
        }
    }

    fn event_path(e: &AccessEvent) -> &str {
        match e {
            AccessEvent::Read { path, .. } => path,
            AccessEvent::Write { path, .. } => path,
        }
    }

    fn event_pid(e: &AccessEvent) -> u32 {
        match e {
            AccessEvent::Read { pid, .. } => *pid,
            AccessEvent::Write { pid, .. } => *pid,
        }
    }

    fn is_read(e: &AccessEvent) -> bool {
        matches!(e, AccessEvent::Read { .. })
    }

    fn is_write(e: &AccessEvent) -> bool {
        matches!(e, AccessEvent::Write { .. })
    }

    // ------------------------------------------------------------------
    // roundtrip tests
    // ------------------------------------------------------------------

    #[test]
    fn roundtrip_read_event() {
        let original = make_read(r"C:\Users\test\file.txt", 1234);
        let mut buf = Vec::new();
        encode_event(&original, &mut buf);

        let (decoded, consumed) = decode_event(&buf).expect("should decode successfully");

        assert_eq!(consumed, buf.len(), "consumed should equal the full buffer");
        assert!(is_read(&decoded));
        assert_eq!(event_pid(&decoded), 1234);
        assert_eq!(event_path(&decoded), r"C:\Users\test\file.txt");
    }

    #[test]
    fn roundtrip_write_event() {
        let original = make_write("/tmp/output.txt", 5678);
        let mut buf = Vec::new();
        encode_event(&original, &mut buf);

        let (decoded, consumed) = decode_event(&buf).expect("should decode successfully");

        assert_eq!(consumed, buf.len(), "consumed should equal the full buffer");
        assert!(is_write(&decoded));
        assert_eq!(event_pid(&decoded), 5678);
        assert_eq!(event_path(&decoded), "/tmp/output.txt");
    }

    #[test]
    fn roundtrip_unicode_path() {
        let path = "C:\\Ür\\ñäme\\文件.txt";
        let original = make_read(path, 9999);
        let mut buf = Vec::new();
        encode_event(&original, &mut buf);

        let (decoded, consumed) = decode_event(&buf).expect("should decode successfully");

        assert_eq!(consumed, buf.len());
        assert!(is_read(&decoded));
        assert_eq!(
            event_path(&decoded),
            path,
            "Unicode path must survive UTF-16 round-trip"
        );
    }

    // ------------------------------------------------------------------
    // partial / invalid buffer tests
    // ------------------------------------------------------------------

    #[test]
    fn empty_buffer_returns_none() {
        assert!(decode_event(&[]).is_none());
    }

    #[test]
    fn partial_header_returns_none() {
        // 4 bytes — header is 7 bytes, so this is incomplete
        assert!(decode_event(&[0x01, 0xD2, 0x04, 0x00]).is_none());
    }

    #[test]
    fn partial_path_returns_none() {
        // Build a header that claims path_len = 5 UTF-16 words (10 path bytes)
        // but only supply 3 path bytes after the header.
        let mut buf = Vec::new();
        buf.push(OP_READ); // op
        buf.extend_from_slice(&1234u32.to_le_bytes()); // pid
        buf.extend_from_slice(&5u16.to_le_bytes()); // path_len = 5 words
        buf.extend_from_slice(&[0x00, 0x01, 0x00]); // only 3 of the required 10 path bytes

        assert!(decode_event(&buf).is_none());
    }

    #[test]
    fn unknown_op_returns_none() {
        let mut buf = Vec::new();
        buf.push(0xFF); // unknown op
        buf.extend_from_slice(&1234u32.to_le_bytes()); // pid
        buf.extend_from_slice(&0u16.to_le_bytes()); // path_len = 0 (no path bytes follow)

        assert!(decode_event(&buf).is_none());
    }

    // ------------------------------------------------------------------
    // multi-event sequential decode
    // ------------------------------------------------------------------

    #[test]
    fn decode_multiple_sequential_events() {
        let event1 = make_read(r"C:\first.txt", 111);
        let event2 = make_write("/second.bin", 222);

        let mut buf = Vec::new();
        encode_event(&event1, &mut buf);
        encode_event(&event2, &mut buf);

        let total_len = buf.len();

        // Decode first event
        let (decoded1, consumed1) = decode_event(&buf).expect("first decode");
        assert!(is_read(&decoded1));
        assert_eq!(event_path(&decoded1), r"C:\first.txt");
        assert_eq!(event_pid(&decoded1), 111);

        // Decode second event using the consumed offset
        let (decoded2, consumed2) = decode_event(&buf[consumed1..]).expect("second decode");
        assert!(is_write(&decoded2));
        assert_eq!(event_path(&decoded2), "/second.bin");
        assert_eq!(event_pid(&decoded2), 222);

        // Both events together consume exactly the whole buffer
        assert_eq!(consumed1 + consumed2, total_len);
    }
}
