//! Entry-name decoding policy for this product.
//!
//! The portable candidate ladder (strict UTF-8, then regional decoders, then
//! CP437) belongs to `data-binary-core`. Only the regional GBK fallback is
//! product policy: it matches the archives this app's users create on Chinese
//! Windows, and other locales would pass a different decoder.

use data_binary_core::{NameTerminal, RegionalDecoder, decode_entry_name};

/// Decode an archive entry name from raw bytes.
///
/// Zip writers disagree about the UTF-8 flag: some store UTF-8 without setting
/// it, older Windows tools store GBK in the same field, and legacy DOS tools use
/// CP437. The shared ladder tries strict UTF-8 first, then GBK here, then CP437
/// so Chinese names round-trip instead of turning into mojibake.
pub fn decode_name(raw: &[u8]) -> String {
    decode_entry_name(raw, &[GBK], NameTerminal::Cp437)
}

/// GBK regional decoder: `None` when the bytes are not valid GBK, so the
/// ladder continues to the CP437 terminal.
fn decode_gbk(raw: &[u8]) -> Option<String> {
    let (decoded, _, had_errors) = encoding_rs::GBK.decode(raw);
    if had_errors { None } else { Some(decoded.into_owned()) }
}

const GBK: RegionalDecoder = &decode_gbk;

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

    #[test]
    fn empty_names_stay_empty() {
        assert_eq!(decode_name(&[]), "");
    }
}
