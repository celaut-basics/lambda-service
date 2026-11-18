//! Just enough of the protobuf wire format to change one length-delimited field of a
//! message and keep every other byte as it is.
//!
//! The id of a service is the SHA3-256 of its bytes. Decoding the Service of the base
//! child and encoding it again would drop the fields that this build of `celaut-proto`
//! does not know (a newer node writes newer fields), and give another id for the same
//! service. So the parent never encodes the base again: it splices bytes.

/// One field of a message, as bytes of the message.
struct Field {
    number: u64,
    wire_type: u8,
    /// The whole field: key, length (if any) and payload.
    start: usize,
    end: usize,
    /// The payload of a length-delimited field.
    payload: Option<(usize, usize)>,
}

fn varint(bytes: &[u8], at: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *bytes.get(*at).ok_or("a varint runs past the end")?;
        *at += 1;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("a varint is longer than 10 bytes".into())
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn fields(bytes: &[u8]) -> Result<Vec<Field>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        let key = varint(bytes, &mut at)?;
        let (number, wire_type) = (key >> 3, (key & 7) as u8);
        let mut payload = None;
        match wire_type {
            0 => {
                varint(bytes, &mut at)?;
            }
            1 => at += 8,
            5 => at += 4,
            2 => {
                let len = usize::try_from(varint(bytes, &mut at)?).map_err(|_| "a length overflows")?;
                payload = Some((at, at + len));
                at += len;
            }
            other => return Err(format!("wire type {other} is not supported")),
        }
        if at > bytes.len() {
            return Err("a field runs past the end".into());
        }
        out.push(Field { number, wire_type, start, end: at, payload });
    }
    Ok(out)
}

/// The payload of the only length-delimited field `number` of `message`.
pub fn get(message: &[u8], number: u64) -> Result<&[u8], String> {
    let mut found = fields(message)?
        .into_iter()
        .filter(|f| f.number == number && f.wire_type == 2);
    match (found.next(), found.next()) {
        (Some(f), None) => {
            let (a, b) = f.payload.unwrap();
            Ok(&message[a..b])
        }
        (None, _) => Err(format!("no field {number}")),
        _ => Err(format!("field {number} is repeated")),
    }
}

/// `message` with the payload of its only length-delimited field `number` replaced by
/// `payload`. Every other field keeps its bytes and its place.
pub fn replace(message: &[u8], number: u64, payload: &[u8]) -> Result<Vec<u8>, String> {
    get(message, number)?; // exactly one
    let mut out = Vec::with_capacity(message.len() + payload.len());
    for f in fields(message)? {
        if f.number == number && f.wire_type == 2 {
            put_varint(&mut out, (number << 3) | 2);
            put_varint(&mut out, payload.len() as u64);
            out.extend_from_slice(payload);
        } else {
            out.extend_from_slice(&message[f.start..f.end]);
        }
    }
    Ok(out)
}

/// One more length-delimited field `number` with `payload`, as bytes to append to a
/// message (a repeated field gets one more entry).
pub fn field(number: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    put_varint(&mut out, (number << 3) | 2);
    put_varint(&mut out, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_keeps_the_other_fields_and_their_order() {
        // field 1 varint 150, field 2 "ab", field 3 fixed32
        let m = [0x08, 0x96, 0x01, 0x12, 0x02, b'a', b'b', 0x1d, 1, 2, 3, 4];
        assert_eq!(get(&m, 2).unwrap(), b"ab");
        let r = replace(&m, 2, b"xyz").unwrap();
        assert_eq!(r, [0x08, 0x96, 0x01, 0x12, 0x03, b'x', b'y', b'z', 0x1d, 1, 2, 3, 4]);
        assert!(get(&m, 4).is_err());
    }

    #[test]
    fn an_appended_field_is_one_more_entry() {
        let mut m = field(1, b"a");
        m.extend(field(1, b"b"));
        assert!(get(&m, 1).is_err()); // repeated
        assert_eq!(fields(&m).unwrap().len(), 2);
    }
}
