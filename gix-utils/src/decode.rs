use std::io::Read;

/// Decode Git's biased base-128 offset integer from a `Read` implementation.
/// Numerical overflow is rejected before reading an impossible continuation;
/// all representable u64 values require at most ten bytes.
#[inline]
pub fn leb64_from_read(mut r: impl Read) -> Result<(u64, usize), std::io::Error> {
    let mut byte = [0u8; 1];
    r.read_exact(&mut byte)?;
    let mut c = byte[0];
    let mut i = 1;
    let mut value = u64::from(c) & 0x7f;
    while c & 0x80 != 0 {
        let high = value
            .checked_add(1)
            .and_then(|value| value.checked_mul(128))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "LEB64 value overflowed"))?;
        r.read_exact(&mut byte)?;
        c = byte[0];
        i += 1;
        value = high
            .checked_add(u64::from(c) & 0x7f)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "LEB64 value overflowed"))?;
    }
    Ok((value, i))
}
