/// A tightly packed BGRA8 video frame (`data.len() == width * height * 4`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("buffer length {got} does not match {width}x{height} BGRA (expected {expected})")]
    BadLength {
        width: u32,
        height: u32,
        expected: usize,
        got: usize,
    },
}

impl Frame {
    /// Opaque black frame.
    pub fn new_black(width: u32, height: u32) -> Frame {
        let mut data = vec![0u8; width as usize * height as usize * 4];
        for px in data.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Frame {
            width,
            height,
            data,
        }
    }

    pub fn from_bgra(width: u32, height: u32, data: Vec<u8>) -> Result<Frame, FrameError> {
        let expected = width as usize * height as usize * 4;
        if data.len() != expected {
            return Err(FrameError::BadLength {
                width,
                height,
                expected,
                got: data.len(),
            });
        }
        Ok(Frame {
            width,
            height,
            data,
        })
    }

    /// BGRA value at (x, y). Panics if out of bounds.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_black_is_opaque_black() {
        let f = Frame::new_black(4, 2);
        assert_eq!(f.data.len(), 32);
        assert_eq!(f.pixel(3, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn from_bgra_rejects_wrong_len() {
        assert!(Frame::from_bgra(2, 2, vec![0; 15]).is_err());
        assert!(Frame::from_bgra(2, 2, vec![0; 16]).is_ok());
    }
}
