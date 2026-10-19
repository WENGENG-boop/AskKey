use std::io::{Read, Write};

use serde::{de::DeserializeOwned, Serialize};

use crate::Limits;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("connection closed")]
    Closed,
    #[error("frame exceeds the size limit")]
    TooLarge,
    #[error("malformed frame")]
    Malformed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Writes one length-prefixed JSON frame.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    let body = serde_json::to_vec(value).map_err(|_| FrameError::Malformed)?;
    if body.len() > Limits::MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    writer.write_all(&(body.len() as u32).to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Reads one length-prefixed JSON frame, rejecting oversized frames before
/// reading their body.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(FrameError::Closed)
        }
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > Limits::MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|_| FrameError::Malformed)
}

/// Parses a frame body that was read by an async transport.
pub fn decode_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    serde_json::from_slice(body).map_err(|_| FrameError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrokerRequest, PROTOCOL_VERSION};

    #[test]
    fn round_trips_a_request() {
        let request = BrokerRequest::simple("health");
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &request).unwrap();
        let decoded: BrokerRequest = read_frame(&mut buffer.as_slice()).unwrap();
        assert_eq!(decoded.method, "health");
        assert_eq!(decoded.version, PROTOCOL_VERSION);
    }

    #[test]
    fn rejects_oversized_header_without_reading_body() {
        let mut buffer = ((Limits::MAX_FRAME_BYTES + 1) as u32)
            .to_be_bytes()
            .to_vec();
        buffer.extend_from_slice(b"{}");
        let result: Result<BrokerRequest, _> = read_frame(&mut buffer.as_slice());
        assert!(matches!(result, Err(FrameError::TooLarge)));
    }

    #[test]
    fn reports_closed_connection() {
        let result: Result<BrokerRequest, _> = read_frame(&mut [].as_slice());
        assert!(matches!(result, Err(FrameError::Closed)));
    }
}
