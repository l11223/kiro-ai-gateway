/// Minimal protobuf wire-format utilities for reading legacy database blobs.
///
/// Only supports the subset needed to extract refresh tokens from
/// Antigravity's `state.vscdb` SQLite database.

/// Read a varint starting at `offset`. Returns `(value, new_offset)`.
pub fn read_varint(data: &[u8], offset: usize) -> Result<(u64, usize), String> {
    let mut result = 0u64;
    let mut shift = 0;
    let mut pos = offset;

    loop {
        if pos >= data.len() {
            return Err("incomplete varint data".to_string());
        }
        let byte = data[pos];
        result |= ((byte & 0x7F) as u64) << shift;
        pos += 1;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err("varint too large".to_string());
        }
    }

    Ok((result, pos))
}

/// Skip over a protobuf field value based on its wire type.
pub fn skip_field(data: &[u8], offset: usize, wire_type: u8) -> Result<usize, String> {
    match wire_type {
        0 => {
            // Varint
            let (_, new_offset) = read_varint(data, offset)?;
            Ok(new_offset)
        }
        1 => Ok(offset + 8), // 64-bit fixed
        2 => {
            // Length-delimited
            let (length, content_offset) = read_varint(data, offset)?;
            let end = content_offset + length as usize;
            if end > data.len() {
                return Err("length-delimited field exceeds data".to_string());
            }
            Ok(end)
        }
        5 => Ok(offset + 4), // 32-bit fixed
        _ => Err(format!("unknown wire type: {}", wire_type)),
    }
}

/// Find a length-delimited (wire type 2) field by field number.
/// Returns the raw bytes of the field value, or `None` if not found.
pub fn find_field(data: &[u8], target_field: u32) -> Result<Option<Vec<u8>>, String> {
    let mut offset = 0;

    while offset < data.len() {
        let (tag, new_offset) = match read_varint(data, offset) {
            Ok(v) => v,
            Err(_) => break,
        };

        let wire_type = (tag & 7) as u8;
        let field_num = (tag >> 3) as u32;

        if field_num == target_field && wire_type == 2 {
            let (length, content_offset) = read_varint(data, new_offset)?;
            let end = content_offset + length as usize;
            if end > data.len() {
                return Err("field data exceeds buffer".to_string());
            }
            return Ok(Some(data[content_offset..end].to_vec()));
        }

        offset = skip_field(data, new_offset, wire_type)?;
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_varint_single_byte() {
        let data = [0x08]; // value = 8
        let (val, off) = read_varint(&data, 0).unwrap();
        assert_eq!(val, 8);
        assert_eq!(off, 1);
    }

    #[test]
    fn test_read_varint_multi_byte() {
        // 300 = 0b100101100 → varint bytes: [0xAC, 0x02]
        let data = [0xAC, 0x02];
        let (val, off) = read_varint(&data, 0).unwrap();
        assert_eq!(val, 300);
        assert_eq!(off, 2);
    }

    #[test]
    fn test_find_field_present() {
        // Manually encode: field 3, wire type 2, length 5, "hello"
        let mut data = Vec::new();
        // tag = (3 << 3) | 2 = 26
        data.push(26);
        // length = 5
        data.push(5);
        data.extend_from_slice(b"hello");

        let result = find_field(&data, 3).unwrap();
        assert_eq!(result, Some(b"hello".to_vec()));
    }

    #[test]
    fn test_find_field_missing() {
        let mut data = Vec::new();
        // field 1, wire type 2, length 3, "abc"
        data.push((1 << 3) | 2);
        data.push(3);
        data.extend_from_slice(b"abc");

        let result = find_field(&data, 5).unwrap();
        assert_eq!(result, None);
    }
}
