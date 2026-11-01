use super::StorageError;
use base64::{Engine, engine::general_purpose::STANDARD};
use concord_protocol::{ChatImage, MAX_CHAT_IMAGE_BYTES};

pub(super) fn decode(encoded: &str) -> Result<(Vec<u8>, ChatImage), StorageError> {
    if encoded.len() > MAX_CHAT_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(StorageError::Invalid("A imagem deve ter no máximo 2 MiB."));
    }
    let data = STANDARD
        .decode(encoded)
        .map_err(|_| StorageError::Invalid("Imagem inválida."))?;
    if data.len() > MAX_CHAT_IMAGE_BYTES {
        return Err(StorageError::Invalid("A imagem deve ter no máximo 2 MiB."));
    }
    let (content_type, width, height) = dimensions(&data).ok_or(StorageError::Invalid(
        "Envie uma imagem PNG, JPEG ou WebP válida.",
    ))?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 20_000_000
    {
        return Err(StorageError::Invalid(
            "A imagem excede as dimensões permitidas.",
        ));
    }
    let image = ChatImage {
        content_type: content_type.to_owned(),
        bytes: data.len() as u64,
        width,
        height,
    };
    Ok((data, image))
}

fn dimensions(data: &[u8]) -> Option<(&'static str, u32, u32)> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") && data.len() >= 33 && &data[12..16] == b"IHDR" {
        return Some((
            "image/png",
            u32::from_be_bytes(data[16..20].try_into().ok()?),
            u32::from_be_bytes(data[20..24].try_into().ok()?),
        ));
    }
    if data.starts_with(&[0xff, 0xd8]) && data.ends_with(&[0xff, 0xd9]) {
        let mut offset = 2;
        while offset + 4 <= data.len() {
            if data[offset] != 0xff {
                return None;
            }
            while data.get(offset) == Some(&0xff) {
                offset += 1;
            }
            let marker = *data.get(offset)?;
            offset += 1;
            if marker == 0xd9 || marker == 0xda {
                break;
            }
            if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                continue;
            }
            let len = u16::from_be_bytes(data.get(offset..offset + 2)?.try_into().ok()?) as usize;
            if len < 2 || offset.checked_add(len)? > data.len() {
                return None;
            }
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                if len < 8 {
                    return None;
                }
                let height =
                    u16::from_be_bytes(data.get(offset + 3..offset + 5)?.try_into().ok()?) as u32;
                let width =
                    u16::from_be_bytes(data.get(offset + 5..offset + 7)?.try_into().ok()?) as u32;
                return Some(("image/jpeg", width, height));
            }
            offset += len;
        }
    }
    if data.len() >= 30 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        let length = u32::from_le_bytes(data[4..8].try_into().ok()?) as usize;
        if length.checked_add(8)? != data.len() {
            return None;
        }
        let (width, height) = match &data[12..16] {
            b"VP8 " if &data[23..26] == b"\x9d\x01\x2a" => (
                u32::from(u16::from_le_bytes(data[26..28].try_into().ok()?) & 0x3fff),
                u32::from(u16::from_le_bytes(data[28..30].try_into().ok()?) & 0x3fff),
            ),
            b"VP8L" if data[20] == 0x2f => {
                let bits = u32::from_le_bytes(data[21..25].try_into().ok()?);
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8X" if data[20] & 2 == 0 => (
                u32::from_le_bytes([data[24], data[25], data[26], 0]) + 1,
                u32::from_le_bytes([data[27], data[28], data[29], 0]) + 1,
            ),
            _ => return None,
        };
        return Some(("image/webp", width, height));
    }
    None
}
