//! Text decoding for PMX, PMD, and VMD.

use encoding_rs::SHIFT_JIS;

use crate::error::FormatError;
use crate::reader::Reader;

pub const MAX_TEXT_BYTES: u32 = 1_048_576;

pub fn decode_shift_jis(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let (text, _, _) = SHIFT_JIS.decode(&bytes[..end]);
    text.trim_end_matches('\u{FFFD}').trim().to_string()
}

pub fn read_pmx_text(
    reader: &mut Reader<'_>,
    utf8: bool,
    context: &'static str,
) -> Result<String, FormatError> {
    let len = reader.u32(context)?;
    if len > MAX_TEXT_BYTES {
        return Err(FormatError::AllocationLimit {
            context,
            count: u64::from(len),
            limit: u64::from(MAX_TEXT_BYTES),
        });
    }
    let bytes = reader.read_exact(len as usize, context)?;
    if utf8 {
        let text = std::str::from_utf8(bytes).map_err(|_| FormatError::BadText(context))?;
        return Ok(text.to_string());
    }
    if bytes.len() % 2 != 0 {
        return Err(FormatError::BadText(context));
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| FormatError::BadText(context))
}
