//! Speed Editor feature-report handshake, based on bmdse 0.1.0 (MIT).
//! Protocol research credited by bmdse to Sylvain "tnt" Munaut.
//!
//! Copyright (c) 2025 Bauke Westendorp
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this
//! software and associated documentation files (the "Software"), to deal in the Software
//! without restriction, including without limitation the rights to use, copy, modify, merge,
//! publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons
//! to whom the Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all copies or
//! substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
//! INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
//! PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE
//! FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
//! OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! DEALINGS IN THE SOFTWARE.

use std::time::Duration;

trait FeatureIo {
    fn send(&self, bytes: &[u8]) -> Result<(), String>;
    fn get(&self, bytes: &mut [u8]) -> Result<usize, String>;
}

impl FeatureIo for hidapi::HidDevice {
    fn send(&self, bytes: &[u8]) -> Result<(), String> {
        self.send_feature_report(bytes).map_err(|e| e.to_string())
    }
    fn get(&self, bytes: &mut [u8]) -> Result<usize, String> {
        self.get_feature_report(bytes).map_err(|e| e.to_string())
    }
}

pub(super) fn authenticate(device: &hidapi::HidDevice) -> Result<Duration, String> {
    handshake(device).map_err(|e| format!("USB authentication failed: {e}"))
}

fn feature(device: &impl FeatureIo, expected: u8) -> Result<[u8; 8], String> {
    let mut bytes = [0u8; 10];
    bytes[0] = 6;
    let n = device.get(&mut bytes)?;
    let [id, op, payload @ ..] = bytes;
    if n != 10 || id != 6 || op != expected {
        return Err(format!("invalid authentication response (step {expected}, length {n})"));
    }
    Ok(payload)
}

fn handshake(device: &impl FeatureIo) -> Result<Duration, String> {
    device.send(&[6, 0, 0, 0, 0, 0, 0, 0, 0, 0])?;
    let challenge = u64::from_le_bytes(feature(device, 0)?);
    device.send(&[6, 1, 0, 0, 0, 0, 0, 0, 0, 0])?;
    feature(device, 2)?;
    let [a, b, c, d, e, f, g, h] = response(challenge).to_le_bytes();
    device.send(&[6, 3, a, b, c, d, e, f, g, h])?;
    let [lo, hi, ..] = feature(device, 4)?;
    // Reauthenticate before expiry, and reset the deadline after every handshake. Reject
    // unusably short leases instead of spinning or underflowing an unsigned subtraction.
    let seconds = u16::from_le_bytes([lo, hi]);
    if seconds <= 5 {
        return Err("device returned an authentication lease of five seconds or less".into());
    }
    Ok(Duration::from_secs(u64::from(seconds - 5)))
}

fn response(challenge: u64) -> u64 {
    let n = (challenge & 7) as u32;
    let (even, odd) = match n {
        0 => (0x3ae1206f97c10bc8, 0x3e22b34f502e7fde),
        1 => (0x2a9ab32bebf244c6, 0x24656b981875ab1c),
        2 => (0x20a6f8b8df9adf0a, 0xa17f3456df7bf8c3),
        3 => (0xaf80ece52cfc1719, 0x6df72e1941aef698),
        4 => (0xec2ee2f7414fd151, 0x72226f011e66ab94),
        5 => (0xb055adfd73344a15, 0x3831a3c606296b42),
        6 => (0xa63d2e3059001187, 0xfd7ff81881332c89),
        _ => (0x751bf623f42e0dde, 0x61a3f6474ff236c6),
    };
    let mut v = challenge.rotate_right(n * 8);
    let key = if (v & 1) == ((0x78 >> n) & 1) {
        even
    } else {
        v ^= v.rotate_right(8);
        odd
    };
    v ^ (v.rotate_right(8) & 0xa79a63f585d37bf0) ^ key
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct Fake {
        replies: RefCell<VecDeque<Vec<u8>>>,
        sent: RefCell<Vec<Vec<u8>>>,
    }
    impl FeatureIo for Fake {
        fn send(&self, bytes: &[u8]) -> Result<(), String> {
            self.sent.borrow_mut().push(bytes.to_vec());
            Ok(())
        }
        fn get(&self, bytes: &mut [u8]) -> Result<usize, String> {
            let next = self.replies.borrow_mut().pop_front().ok_or("no reply")?;
            for (to, from) in bytes.iter_mut().zip(&next) {
                *to = *from;
            }
            Ok(next.len())
        }
    }
    fn fake() -> Fake {
        Fake {
            replies: RefCell::new(VecDeque::from([
                vec![6, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                vec![6, 2, 0, 0, 0, 0, 0, 0, 0, 0],
                vec![6, 4, 88, 2, 0, 0, 0, 0, 0, 0],
            ])),
            sent: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn handshake_computes_response_and_renews_before_expiry() {
        let device = fake();
        assert_eq!(handshake(&device).unwrap(), Duration::from_secs(595));
        assert_eq!(response(0), 0x3ae1206f97c10bc8);
        let expected = [vec![6, 0, 0, 0, 0, 0, 0, 0, 0, 0], vec![6, 1, 0, 0, 0, 0, 0, 0, 0, 0], vec![6, 3, 0xc8, 0x0b, 0xc1, 0x97, 0x6f, 0x20, 0xe1, 0x3a]];
        assert_eq!(*device.sent.borrow(), expected);
    }

    #[test]
    fn truncated_wrong_steps_and_hostile_leases_return_errors() {
        for step in 0..3 {
            for n in 0..10 {
                let device = fake();
                device.replies.borrow_mut()[step].truncate(n);
                assert!(handshake(&device).is_err());
            }
            let device = fake();
            device.replies.borrow_mut()[step][1] = 255;
            assert!(handshake(&device).is_err());
        }
        for lease in 0..=5 {
            let device = fake();
            device.replies.borrow_mut()[2][2] = lease;
            device.replies.borrow_mut()[2][3] = 0;
            assert!(handshake(&device).is_err());
        }
    }
}
