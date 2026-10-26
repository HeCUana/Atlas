//! 把字节流切分为逻辑行。
//!
//! ## 规则
//!
//! - `\n` 结束一个**完整行**，直接产出。
//! - `\r` 结束一个**进度片段**。连续的进度片段互相覆盖（进度条的语义），
//!   只有该轮最后一个片段会被产出；被覆盖的片段丢弃。
//! - `\r` 之后紧跟 `\n`：这是 `\r\n` 行尾（Windows 风格），被 `\r` 截断的片段
//!   就是这一行的完整内容，**不会**额外产出一条空行。
//! - `flush()` 产出缓冲区中剩余的内容（进程退出时调用）。
//!
//! ## UTF-8
//!
//! 输入按 UTF-8 解码。多字节字符**可能被 chunk 边界劈开**，此时切分器保留末尾
//! 不完整的字节序列，等下一片数据补全后再解码，因此既不会产生乱码也不会丢字。
//! 真正非法的字节用 U+FFFD 替换字符兜底，同样不丢内容。

/// 行切分器。
///
/// ```
/// use atlas_server::LineSplitter;
///
/// let mut splitter = LineSplitter::new();
/// let lines = splitter.push(b"[12:34:56] 10%\r[12:34:56] 100%\n");
/// assert_eq!(lines, vec!["[12:34:56] 100%".to_string()]);
/// ```
#[derive(Debug, Default)]
pub struct LineSplitter {
    /// 当前片段的文本（已完成 UTF-8 解码）。
    buffer: String,
    /// 被 `\r` 截断的上一片段，等待判定是被后续文本覆盖还是构成完整行。
    fragment: Option<String>,
    /// 末尾不完整的多字节 UTF-8 序列，等待下一片数据补全。
    pending_bytes: Vec<u8>,
}

impl LineSplitter {
    /// 创建空切分器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段字节，返回本次可以确定的完整行。
    ///
    /// 输入按 UTF-8 解码；不完整的多字节字符会被缓冲到下次调用。
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        self.pending_bytes.extend_from_slice(chunk);
        let bytes = std::mem::take(&mut self.pending_bytes);
        let (text, consumed) = decode_prefix(&bytes);
        // 末尾不完整的字节序列留到下一片数据补全。
        self.pending_bytes.extend_from_slice(&bytes[consumed..]);
        self.consume(&text, &mut out);
        out
    }

    /// 消费一段已解码文本。
    fn consume(&mut self, text: &str, out: &mut Vec<String>) {
        for ch in text.chars() {
            match ch {
                '\n' => {
                    // `\r\n`：被截断的片段就是这一行的内容；否则行内容是当前 buffer。
                    let line = match self.fragment.take() {
                        Some(fragment) if self.buffer.is_empty() => fragment,
                        _ => std::mem::take(&mut self.buffer),
                    };
                    out.push(line);
                }
                '\r' => {
                    // 当前片段被 `\r` 截断：暂存为候选行，等后继文本判断是否被覆盖。
                    // 空片段不会擦掉已有内容（与终端行为一致）。
                    if !self.buffer.is_empty() {
                        self.fragment = Some(std::mem::take(&mut self.buffer));
                    }
                }
                _ => {
                    // `\r` 之后出现了新文本：上一片段被覆盖（进度条语义），丢弃。
                    if self.buffer.is_empty() {
                        self.fragment = None;
                    }
                    self.buffer.push(ch);
                }
            }
        }
    }

    /// 刷新残留内容。
    ///
    /// 进程退出、输入流关闭时调用，产出最后一行（若有）。
    pub fn flush(&mut self) -> Option<String> {
        // 末尾不完整的多字节序列：用替换字符兜底，不丢内容。
        if !self.pending_bytes.is_empty() {
            let bytes = std::mem::take(&mut self.pending_bytes);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            self.buffer.push_str(&text);
        }
        if let Some(fragment) = self.fragment.take() {
            return Some(fragment);
        }
        if self.buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buffer))
        }
    }
}

/// 解码字节前缀，返回 `(已解码文本, 已消费的字节数)`。
///
/// - 末尾不完整的多字节 UTF-8 序列不会被消费，等待下一片数据补全；
/// - 非法字节用 U+FFFD 替换字符兜底，保证内容不丢。
fn decode_prefix(bytes: &[u8]) -> (String, usize) {
    let mut out = String::with_capacity(bytes.len());
    let mut offset = 0usize;
    loop {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(text) => {
                out.push_str(text);
                offset = bytes.len();
                break;
            }
            Err(err) => {
                let valid = err.valid_up_to();
                out.push_str(&String::from_utf8_lossy(&bytes[offset..offset + valid]));
                offset += valid;
                match err.error_len() {
                    // 末尾是不完整的多字节序列：保留给下一片数据。
                    None => break,
                    // 非法字节：替换字符兜底后继续扫描剩余部分。
                    Some(bad) => {
                        out.push_str(&String::from_utf8_lossy(&bytes[offset..offset + bad]));
                        offset += bad;
                    }
                }
            }
        }
    }
    (out, offset)
}

#[cfg(test)]
mod tests {
    use super::LineSplitter;

    /// 依次喂入若干片段，返回全部产出的行。
    fn push_all(splitter: &mut LineSplitter, chunks: &[&[u8]]) -> Vec<String> {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(splitter.push(chunk));
        }
        out
    }

    #[test]
    fn splits_lines_on_newline() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"first\nsecond\nthird\n");
        assert_eq!(lines, vec!["first", "second", "third"]);
    }

    #[test]
    fn keeps_empty_lines() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"\n\na\n\n");
        assert_eq!(lines, vec!["", "", "a", ""]);
    }

    #[test]
    fn progress_bar_overwrites_previous_segment() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"[12:34:56] 10%\r[12:34:56] 50%\r[12:34:56] 100%\n");
        assert_eq!(lines, vec!["[12:34:56] 100%"]);
    }

    #[test]
    fn progress_segment_is_not_emitted_before_newline() {
        // 进度片段在遇到 `\n` 之前不应产出任何行。
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"10%\r50%");
        assert!(lines.is_empty(), "进度片段不应提前产出: {lines:?}");
    }

    #[test]
    fn crlf_terminates_single_line() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"hello\r\n");
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn crlf_does_not_produce_extra_empty_line() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"a\r\nb\r\n");
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn repeated_cr_keeps_last_non_empty_segment() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"a\r\r\n");
        assert_eq!(lines, vec!["a"]);
    }

    #[test]
    fn text_after_cr_replaces_segment() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"old\rnew\n");
        assert_eq!(lines, vec!["new"]);
    }

    #[test]
    fn splits_multiple_lines_within_one_chunk() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"a\nb\nc\nd\n");
        assert_eq!(lines, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn splits_one_line_across_multiple_chunks() {
        let mut splitter = LineSplitter::new();
        let lines = push_all(&mut splitter, &[b"he", b"ll", b"o\n", b"wor", b"ld\n"]);
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn joins_utf8_characters_split_across_chunks() {
        let mut splitter = LineSplitter::new();
        let text = "服务器启动完成\n".as_bytes();
        // 在多字节字符中间切开。
        let cut = 4;
        let lines = push_all(&mut splitter, &[&text[..cut], &text[cut..]]);
        assert_eq!(lines, vec!["服务器启动完成"]);
    }

    #[test]
    fn joins_utf8_characters_split_byte_by_byte() {
        let mut splitter = LineSplitter::new();
        let text = "服务器启动完成\n".as_bytes();
        let chunks: Vec<&[u8]> = text.chunks(1).collect();
        let lines = push_all(&mut splitter, &chunks);
        assert_eq!(lines, vec!["服务器启动完成"]);
    }

    #[test]
    fn joins_utf8_characters_split_at_carriage_return_boundary() {
        let mut splitter = LineSplitter::new();
        let text = "进度 50%\r完成\n".as_bytes();
        let cut = text.len() - "完成\n".len() - 1;
        let lines = push_all(&mut splitter, &[&text[..cut], &text[cut..]]);
        assert_eq!(lines, vec!["完成"]);
    }

    #[test]
    fn invalid_utf8_bytes_are_replaced_not_dropped() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(&[b'a', 0xFF, b'b', b'\n']);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with('a'));
        assert!(lines[0].ends_with('b'));
    }

    #[test]
    fn flush_emits_trailing_line() {
        let mut splitter = LineSplitter::new();
        assert!(splitter.push(b"no newline").is_empty());
        assert_eq!(splitter.flush(), Some("no newline".to_string()));
    }

    #[test]
    fn flush_returns_none_when_buffer_empty() {
        let mut splitter = LineSplitter::new();
        splitter.push(b"done\n");
        assert_eq!(splitter.flush(), None);
    }

    #[test]
    fn flush_emits_carriage_return_fragment() {
        let mut splitter = LineSplitter::new();
        assert!(splitter.push(b"50%\r").is_empty());
        assert_eq!(splitter.flush(), Some("50%".to_string()));
    }

    #[test]
    fn flush_emits_incomplete_utf8_as_replacement() {
        let mut splitter = LineSplitter::new();
        // "服" 的前两个字节，缺少第三个字节。
        let lines = splitter.push(&[0xE6, 0x9C]);
        assert!(lines.is_empty());
        let flushed = splitter.flush().expect("残留内容应被产出");
        assert!(!flushed.is_empty());
    }

    #[test]
    fn mixed_newline_and_progress_lines() {
        let mut splitter = LineSplitter::new();
        let lines = splitter.push(b"boot\n[12:34:56] 10%\r[12:34:56] 100%\nready\n");
        assert_eq!(lines, vec!["boot", "[12:34:56] 100%", "ready"]);
    }
}
