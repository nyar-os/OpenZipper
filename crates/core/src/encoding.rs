/// Decode an archive entry name from raw bytes.
///
/// Zip writers disagree about the UTF-8 flag: some store UTF-8 without setting
/// it, older Windows tools store GBK in the same field, and legacy DOS tools use
/// CP437. The decoder tries strict UTF-8 first, then GBK, then CP437 so Chinese
/// names round-trip instead of turning into mojibake.
pub fn decode_name(raw: &[u8]) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if let Ok(text) = std::str::from_utf8(raw) {
        return text.to_string();
    }
    let (decoded, _, had_errors) = encoding_rs::GBK.decode(raw);
    if !had_errors {
        return decoded.into_owned();
    }
    decode_cp437(raw)
}

/// CP437 mapping for bytes `0x80..=0xFF`.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢',
    '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖',
    '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙',
    '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

fn decode_cp437(raw: &[u8]) -> String {
    let mut text = String::with_capacity(raw.len());
    for &byte in raw {
        if byte < 0x80 {
            text.push(char::from(byte));
        }
        else {
            text.push(CP437_HIGH[(byte - 0x80) as usize]);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_names_round_trip() {
        assert_eq!(decode_name("文档/报告.txt".as_bytes()), "文档/报告.txt");
    }

    #[test]
    fn gbk_names_are_decoded() {
        // GBK encoding of `中文.txt`.
        let gbk = [0xD6, 0xD0, 0xCE, 0xC4, b'.', b't', b'x', b't'];
        assert_eq!(decode_name(&gbk), "中文.txt");
    }

    #[test]
    fn cp437_is_the_last_resort() {
        // 0x82 is `é` in CP437 and not a valid GBK lead byte pair on its own.
        assert_eq!(decode_name(&[0x82]), "é");
    }
}
