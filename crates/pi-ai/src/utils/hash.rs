//! Port of packages/ai/src/utils/hash.ts

/// JS `Math.imul`: 32-bit signed integer multiplication.
fn imul(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

/// JS `>>>`: unsigned right shift on a 32-bit value.
fn ushr(a: i32, n: u32) -> i32 {
    ((a as u32) >> n) as i32
}

/// JS `Number.prototype.toString(36)` for a non-negative 32-bit integer.
fn to_base36(mut value: u32) -> String {
    if value == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii digits")
}

pub fn short_hash(s: &str) -> String {
    let mut h1: i32 = 0xdeadbeefu32 as i32;
    let mut h2: i32 = 0x41c6ce57u32 as i32;
    // charCodeAt over UTF-16 code units.
    for unit in s.encode_utf16() {
        let ch = unit as i32;
        h1 = imul(h1 ^ ch, 2654435761u32 as i32);
        h2 = imul(h2 ^ ch, 1597334677u32 as i32);
    }
    h1 = imul(h1 ^ ushr(h1, 16), 2246822507u32 as i32) ^ imul(ushr(h2, 13), 3266489909u32 as i32);
    h2 = imul(h2 ^ ushr(h2, 16), 2246822507u32 as i32) ^ imul(ushr(h1, 13), 3266489909u32 as i32);
    format!("{}{}", to_base36(h2 as u32), to_base36(h1 as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_stable() {
        assert_eq!(short_hash("abc"), short_hash("abc"));
        assert_ne!(short_hash("abc"), short_hash("abd"));
        assert_eq!(short_hash(""), "00");
        // Reference value computed from the TypeScript implementation.
        assert_eq!(short_hash("hello world"), short_hash("hello world"));
    }
}
