use anyhow::{Result, bail};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

#[derive(Default, Clone, Copy, PartialEq)]
enum State {
    #[default]
    Begin,
    Dot,
    DotCr,
    Cr,
    Data,
    End,
}

/// Incremental SMTP dot decoding; preserves EML bytes and the following command.
#[derive(Default)]
pub struct Decoder {
    state: State,
    line_limit: usize,
    line_length: usize,
}

impl Decoder {
    pub fn bounded() -> Self {
        Self {
            line_limit: 2000,
            line_length: 1,
            ..Self::default()
        }
    }

    fn check_lines(&mut self, mut bytes: &[u8]) -> Result<()> {
        if self.line_limit == 0 {
            return Ok(());
        }
        while let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
            if self.line_length + end > self.line_limit {
                bail!("SMTP DATA line too long");
            }

            self.line_length = 1;
            bytes = &bytes[end + 1..];
        }

        self.line_length += bytes.len();
        if self.line_length > self.line_limit {
            bail!("SMTP DATA line too long");
        }

        Ok(())
    }

    pub async fn read<R: AsyncBufRead + Unpin>(
        &mut self,
        reader: &mut R,
        out: &mut [u8],
    ) -> Result<usize> {
        let mut written = 0;
        while written < out.len() && self.state != State::End {
            let input =
                tokio::time::timeout(std::time::Duration::from_secs(120), reader.fill_buf())
                    .await??;
            if input.is_empty() {
                bail!("incomplete SMTP DATA");
            }
            if self.state == State::Data {
                let available = input.len().min(out.len() - written);
                let count = input[..available]
                    .iter()
                    .position(|&byte| byte == b'\r')
                    .unwrap_or(available);
                if count > 0 {
                    self.check_lines(&input[..count])?;
                    out[written..written + count].copy_from_slice(&input[..count]);
                    reader.consume(count);
                    written += count;
                    continue;
                }
            }

            let ch = input[0];
            self.check_lines(&[ch])?;
            reader.consume(1);
            match self.state {
                State::Begin if ch == b'.' => {
                    self.state = State::Dot;
                    continue;
                }
                State::Begin if ch == b'\r' => self.state = State::Cr,
                State::Begin => self.state = State::Data,
                State::Dot if ch == b'\r' => {
                    self.state = State::DotCr;
                    continue;
                }
                State::Dot => self.state = State::Data,
                State::DotCr if ch == b'\n' => {
                    self.state = State::End;
                    continue;
                }
                State::DotCr => self.state = State::Data,
                State::Cr if ch == b'\n' => self.state = State::Begin,
                State::Cr => self.state = State::Data,
                State::Data if ch == b'\r' => self.state = State::Cr,
                _ => {}
            }

            out[written] = ch;
            written += 1;
        }

        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, BufReader};

    #[tokio::test]
    async fn data_line_limit_across_buffers() {
        for capacity in [1, 7, 4096] {
            for (length, valid) in [(998, true), (1998, true), (2000, false), (3000, false)] {
                let wire = format!("{}\r\n.\r\n", "x".repeat(length));
                let mut reader = BufReader::with_capacity(capacity, wire.as_bytes());
                let result = Decoder::bounded().read(&mut reader, &mut [0; 4096]).await;

                assert_eq!(result.is_ok(), valid);
            }
        }
    }

    #[tokio::test]
    async fn fragmented_data_and_following_command() {
        for raw in [
            "".to_string(),
            "x\r\n".into(),
            ".\r\n".into(),
            "a\r\n.\r\n..x\r\n".into(),
            "bare\n.dot\r\n".into(),
            format!("{}\r\n.end\r\n", "x".repeat(65535)),
        ] {
            let mut wire = raw.replace("\r\n.", "\r\n..");
            if wire.starts_with('.') {
                wire.insert(0, '.');
            }

            wire.push_str(".\r\nNOOP\r\n");

            for capacity in [1, 2, 7, 4096] {
                for size in [1, 2, 7, 65536] {
                    let mut input = BufReader::with_capacity(capacity, wire.as_bytes());
                    let mut decoder = Decoder::default();
                    let mut got = Vec::new();
                    let mut out = vec![0; size];

                    loop {
                        let count = decoder.read(&mut input, &mut out).await.unwrap();
                        if count == 0 {
                            break;
                        }

                        got.extend_from_slice(&out[..count]);
                    }

                    assert_eq!(got, raw.as_bytes());
                    let mut tail = String::new();
                    input.read_to_string(&mut tail).await.unwrap();

                    assert_eq!(tail, "NOOP\r\n");
                }
            }
        }

        let mut decoder = Decoder::default();

        assert!(
            decoder
                .read(&mut BufReader::new(b"incomplete".as_slice()), &mut [0; 32])
                .await
                .is_err()
        );
    }
}
