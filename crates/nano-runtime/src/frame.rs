//! Bounded NDJSON reader for the host transport.
//!
//! The reader never buffers past the declared frame limit. A newline ends a
//! frame; UTF-8 sequences may span transport chunks because the reader
//! accumulates bytes until the newline, then validates once.

use std::io::{self, BufRead};

use crate::MAX_FRAME_BYTES;

/// A failure while reading a frame.
#[derive(Debug)]
pub enum FrameReadError {
    /// Underlying I/O failed.
    Io(io::Error),
    /// The frame exceeded the declared byte limit.
    TooLarge(usize),
    /// The stream ended in the middle of a frame.
    Unterminated,
    /// The frame bytes are not valid UTF-8.
    InvalidUtf8,
    /// A blank line is not a frame.
    Empty,
}

impl std::fmt::Display for FrameReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::TooLarge(bytes) => write!(formatter, "frame of {bytes} bytes exceeds the limit"),
            Self::Unterminated => write!(formatter, "stream ended inside a frame"),
            Self::InvalidUtf8 => write!(formatter, "frame is not valid UTF-8"),
            Self::Empty => write!(formatter, "empty frame"),
        }
    }
}

impl std::error::Error for FrameReadError {}

impl From<io::Error> for FrameReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Read newline-delimited frames from a buffered byte stream.
pub struct FrameReader<R: BufRead> {
    reader: R,
    max_frame_bytes: usize,
}

impl<R: BufRead> FrameReader<R> {
    /// Create a reader with the contract frame limit.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }

    /// Create a reader with an explicit limit, used by focused tests.
    pub fn with_limit(reader: R, max_frame_bytes: usize) -> Self {
        Self {
            reader,
            max_frame_bytes,
        }
    }

    /// Read one frame without its terminator.
    ///
    /// Returns `Ok(None)` only at a clean EOF between frames.
    pub fn read_frame(&mut self) -> Result<Option<String>, FrameReadError> {
        let mut line = Vec::new();
        loop {
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                if line.is_empty() {
                    return Ok(None);
                }
                return Err(FrameReadError::Unterminated);
            }
            if let Some(index) = available.iter().position(|byte| *byte == b'\n') {
                if line.len() + index > self.max_frame_bytes {
                    return Err(FrameReadError::TooLarge(line.len() + index));
                }
                line.extend_from_slice(&available[..index]);
                self.reader.consume(index + 1);
                break;
            }
            if line.len() + available.len() > self.max_frame_bytes {
                return Err(FrameReadError::TooLarge(line.len() + available.len()));
            }
            let length = available.len();
            line.extend_from_slice(available);
            self.reader.consume(length);
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            return Err(FrameReadError::Empty);
        }
        String::from_utf8(line)
            .map(Some)
            .map_err(|_| FrameReadError::InvalidUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    struct Chunked {
        data: Vec<u8>,
        position: usize,
        chunk: usize,
    }

    impl Read for Chunked {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let remaining = self.data.len() - self.position;
            let take = remaining.min(self.chunk).min(buffer.len());
            buffer[..take].copy_from_slice(&self.data[self.position..self.position + take]);
            self.position += take;
            Ok(take)
        }
    }

    fn reader(data: &[u8], chunk: usize) -> FrameReader<impl BufRead> {
        FrameReader::new(io::BufReader::new(Chunked {
            data: data.to_vec(),
            position: 0,
            chunk,
        }))
    }

    #[test]
    fn reads_multiple_frames() {
        let mut frames = reader(b"{\"a\":1}\n{\"b\":2}\n", 3);
        assert_eq!(frames.read_frame().unwrap().as_deref(), Some("{\"a\":1}"));
        assert_eq!(frames.read_frame().unwrap().as_deref(), Some("{\"b\":2}"));
        assert_eq!(frames.read_frame().unwrap(), None);
    }

    #[test]
    fn handles_utf8_across_chunks() {
        let mut frames = reader("{\"k\":\"中文\"}\n".as_bytes(), 2);
        assert_eq!(frames.read_frame().unwrap().as_deref(), Some("{\"k\":\"中文\"}"));
    }

    #[test]
    fn rejects_unterminated_frame() {
        let mut frames = reader(b"{\"a\":1}", 4);
        assert!(matches!(frames.read_frame(), Err(FrameReadError::Unterminated)));
    }

    #[test]
    fn rejects_invalid_utf8() {
        let mut frames = reader(&[b'{', 0xff, b'}', b'\n'], 4);
        assert!(matches!(frames.read_frame(), Err(FrameReadError::InvalidUtf8)));
    }

    #[test]
    fn rejects_oversize_frame() {
        let mut frames = FrameReader::with_limit(io::BufReader::new(Cursor::new(vec![b'a'; 10])), 4);
        assert!(matches!(frames.read_frame(), Err(FrameReadError::TooLarge(_))));
    }

    #[test]
    fn rejects_empty_frame() {
        let mut frames = reader(b"\n", 1);
        assert!(matches!(frames.read_frame(), Err(FrameReadError::Empty)));
    }
}
