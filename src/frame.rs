use std::convert::TryInto;
use std::io;
use std::mem::size_of;
use std::ops::Range;

use tokio::io::AsyncReadExt;
use serde::de::DeserializeOwned;
use serde::Serialize;

const BUF_SIZE: usize = 4096;
const MAX_BUF_SIZE: usize = BUF_SIZE * 100;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum Header {
    Unset,
    Small, // Content length is  u8::MAX
    Large, // Content length is u32::MAX
}

impl Header {
    const fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Header::Unset),
            1 => Some(Header::Small),
            2 => Some(Header::Large),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Frame {
    buffer: Vec<u8>,
    bytes_read: usize,
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { self.buffer.set_len(self.bytes_read) }
    }
}

impl Frame {
    pub fn empty() -> Self {
        let mut buffer = Vec::with_capacity(BUF_SIZE);
        unsafe { buffer.set_len(BUF_SIZE) };

        Self {
            buffer,
            bytes_read: 0,
        }
    }

    pub async fn read<T: AsyncReadExt + Unpin>(&mut self, reader: &mut T) -> io::Result<usize> {
        let slice = self.slice();
        let res = reader.read(slice).await;
        if let Ok(n) = res {
            self.bytes_read += n;
            if self.bytes_read < BUF_SIZE && self.buffer.capacity() > BUF_SIZE {
                unsafe { self.buffer.set_len(BUF_SIZE) };
                self.buffer.shrink_to_fit();
            }
        }
        res
    }

    pub fn frame_message<T: Serialize>(msg: T) -> Vec<u8> {
        let mut data = serde_json::to_vec(&msg).unwrap();
        let header = if data.len() <= u8::MAX as usize {
            data.insert(0, data.len() as u8);
            Header::Small
        } else {
            let mut size = (data.len() as u32).to_be_bytes().to_vec();
            size.append(&mut data);
            data = size;
            Header::Large
        } as u8;

        data.insert(0, header);
        data
    }

    pub fn try_msg<T: DeserializeOwned>(&mut self) -> Option<T> {
        let range = self.range()?;
        if range.end > self.bytes_read {
            return None;
        }

        let res = serde_json::from_slice(&self.buffer[range.clone()]);

        self.shift_down(range.end);

        if self.bytes_read <= BUF_SIZE && self.buffer.capacity() > BUF_SIZE {
            unsafe { self.buffer.set_len(BUF_SIZE) };
            self.buffer.shrink_to_fit();
        }

        res.ok()
    }

    pub fn safe_slice(&self) -> &[u8] {
        &self.buffer[..self.bytes_read]
    }

    fn slice(&mut self) -> &mut [u8] {
        let slice = &mut self.buffer[self.bytes_read..];
        if slice.len() == 0 && self.buffer.capacity() < MAX_BUF_SIZE {
            self.buffer.reserve(BUF_SIZE);
            unsafe {
                self.buffer.set_len(self.buffer.len() + BUF_SIZE);
            }
        }

        &mut self.buffer[self.bytes_read..]
    }

    fn range(&self) -> Option<Range<usize>> {
        if self.bytes_read == 0 {
            return None;
        }

        let header = Header::from_u8(self.buffer[0])?;

        match header {
            Header::Small if self.bytes_read >= size_of::<u8>() + 1 => {
                let offset = size_of::<u8>() + 1;
                let size = self.buffer[1] as usize;
                Some(offset..size + offset)
            }
            Header::Large if self.bytes_read >= size_of::<u32>() + 1 => {
                let offset = size_of::<u32>() + 1;
                let bytes: [u8; 4] = self.buffer[1..offset].try_into().unwrap();
                let size = u32::from_be_bytes(bytes) as usize;
                Some(offset..size + offset)
            }
            Header::Large | Header::Small => None,
            Header::Unset => None,
        }
    }

    fn shift_down(&mut self, end: usize) {
        unsafe {
            //     s     e 
            // h l 1 1 1 h l
            // ? ? ? ? ? ? ?
            // range len   = 3
            // br          = 7
            // e           = 5
            // s           = 2
            let src = self.buffer.as_ptr().add(end);
            let dst = self.buffer.as_mut_ptr();
            let amount = self.bytes_read - end;
            std::ptr::copy(src, dst, amount);
            self.bytes_read -= end;
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::io::{Read, Result};

    struct PretendStream(Vec<u8>);

    impl Read for PretendStream {
        fn read(&mut self, bytes: &mut [u8]) -> Result<usize> {
            let bytes_len = bytes.len();
            let self_len = self.0.len();
            let end = bytes.len().min(self.0.len());
            let vec: Vec<u8> = self.0.drain(..end).collect();
            bytes[..end].copy_from_slice(&vec);
            Ok(end)
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Message {
        data: Vec<u8>,
    }

    impl Message {
        fn new(data: Vec<u8>) -> Self {
            Self { data }
        }
    }

    #[test]
    fn frame_message() {
        let data = Frame::frame_message(Message::new(Vec::new()));
        let mut stream = PretendStream(data);
        let mut f = Frame::empty();

        f.read(&mut stream);
        let m: Option<Message> = f.try_msg();
        assert!(m.is_some());
    }

    #[test]
    fn frame_multiple() {
        let mut data = Vec::new();
        let message_count = 3;
        // Add three messages to the fake stream
        for _ in 0..message_count {
            data.append(&mut Frame::frame_message(Message::new(Vec::new())));
        }
        let mut stream = PretendStream(data);
        let mut f = Frame::empty();
        f.read(&mut stream);

        // Make sure we can get all messages out
        for _ in 0..message_count {
            let m: Option<Message> = f.try_msg();
            assert!(m.is_some());
        }

        // Make sure there are no residual data
        let m: Option<Message> = f.try_msg();
        assert!(m.is_none());
    }

    #[test]
    fn auto_grow_buffer() {
        let message = Message::new(vec![1; BUF_SIZE / 2 + 1]);
        let data = Frame::frame_message(message);
        let message_len = data.len();
        let mut stream = PretendStream(data);
        let mut f = Frame::empty();

        let n_bytes = f.read(&mut stream).unwrap();
        assert_eq!(n_bytes, BUF_SIZE);

        let d = f.read(&mut stream).unwrap();
        assert_eq!(f.buffer.len(), BUF_SIZE * 2);
        assert_eq!(d, message_len - BUF_SIZE);

        let message = f.try_msg::<Message>();
        assert!(message.is_some());
    }

    #[test]
    fn read_four_times_buf_size() {
        let data = Frame::frame_message(Message::new(vec![1; BUF_SIZE * 2 + 1]));
        let mut stream = PretendStream(data);
        let mut f = Frame::empty();

        let n_bytes = f.read(&mut stream).unwrap();
        let n_bytes = f.read(&mut stream).unwrap();
        let n_bytes = f.read(&mut stream).unwrap();
        let n_bytes = f.read(&mut stream).unwrap();

        let n_bytes = f.read(&mut stream).unwrap();
        assert_eq!(n_bytes, 17);

        let message = f.try_msg::<Message>();
        assert!(message.is_some());
    }

    #[test]
    fn auto_shrink_buffer() {
        let data = Frame::frame_message(Message::new(vec![1; BUF_SIZE + 1]));
        let mut stream = PretendStream(data);
        let mut f = Frame::empty();

        f.read(&mut stream); // read max buf size
        f.read(&mut stream); // resize the buffer to fit the entire
        f.read(&mut stream); // resize the buffer to fit the entire
        f.try_msg::<Message>().unwrap();

        assert_eq!(f.buffer.len(), BUF_SIZE);
    }
}

