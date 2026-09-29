//! gRPC length-prefix framing: one message becomes `flag | len | bytes`.

use anyhow::{Result, bail};

/// Frame one message. The flag is 0: h5i does not compress what it sends.
pub fn frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + message.len());
    out.push(0);
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out
}

/// Split a body into its messages. A server-streaming reply is several in a row.
pub fn unframe(buf: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if i + 5 > buf.len() {
            bail!("truncated gRPC frame header");
        }
        let compressed = buf[i];
        let len = u32::from_be_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]) as usize;
        i += 5;
        if compressed != 0 {
            bail!("this frame is compressed, which h5i does not decode yet");
        }
        if i + len > buf.len() {
            bail!("truncated gRPC frame body: header said {len} bytes");
        }
        out.push(buf[i..i + len].to_vec());
        i += len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let framed = frame(b"hello");
        assert_eq!(framed[0], 0);
        assert_eq!(&framed[1..5], &[0, 0, 0, 5]);
        assert_eq!(unframe(&framed).unwrap(), vec![b"hello".to_vec()]);
    }

    #[test]
    fn several_frames_split_into_several_messages() {
        let mut body = frame(b"one");
        body.extend(frame(b"two"));
        assert_eq!(unframe(&body).unwrap(), vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn a_truncated_frame_is_an_error_not_a_panic() {
        assert!(unframe(&[0, 0, 0, 0, 9, 1, 2]).is_err());
    }
}
