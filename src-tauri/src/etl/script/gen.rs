//! `Gen.XXX` 產生器的執行期實作。
//!
//! - GUID / ULID / 雜湊：每列產生新值
//! - 日期時間類：以任務開始時間為準（同一次執行內所有列一致）

use chrono::{DateTime, Local, SecondsFormat};
use md5::Md5;
use sha2::{Digest, Sha256, Sha512};
use uuid::Uuid;

use crate::etl::script::ast::GenKind;
use crate::models::value::CellValue;

/// 產生器執行情境（任務開始時建立一次）。
pub struct GenContext {
    now: DateTime<Local>,
}

impl GenContext {
    pub fn new() -> Self {
        GenContext { now: Local::now() }
    }
}

impl Default for GenContext {
    fn default() -> Self {
        Self::new()
    }
}

/// 依產生器種類產生一個值；`row` 為來源列（雜湊類的輸入）。
pub fn generate(kind: GenKind, row: &[CellValue], ctx: &GenContext) -> CellValue {
    match kind {
        GenKind::Guid | GenKind::GuidText => CellValue::Text(Uuid::new_v4().to_string()),
        GenKind::Ulid => CellValue::Text(new_ulid(ctx.now.timestamp_millis().max(0) as u64)),
        GenKind::Date => CellValue::Date(ctx.now.date_naive()),
        GenKind::DateText => CellValue::Text(ctx.now.format("%Y-%m-%d").to_string()),
        GenKind::DateTime => CellValue::DateTime(ctx.now.naive_local()),
        GenKind::DateTimeText => CellValue::Text(ctx.now.format("%Y-%m-%d %H:%M:%S").to_string()),
        GenKind::DateTimeOffset | GenKind::DateTimeOffsetText => {
            CellValue::Text(ctx.now.to_rfc3339_opts(SecondsFormat::Millis, false))
        }
        GenKind::Sha256 => CellValue::Text(hex(&Sha256::digest(row_fingerprint(row)))),
        GenKind::Sha512 => CellValue::Text(hex(&Sha512::digest(row_fingerprint(row)))),
        GenKind::Md5 => CellValue::Text(hex(&Md5::digest(row_fingerprint(row)))),
    }
}

/// 來源整列的雜湊輸入：各欄位文字表示以 US（unit separator）串接，NULL 為空字串。
fn row_fingerprint(row: &[CellValue]) -> Vec<u8> {
    let parts: Vec<String> = row
        .iter()
        .map(|c| match c {
            CellValue::Null => String::new(),
            CellValue::Text(s) => s.clone(),
            CellValue::Int(v) => v.to_string(),
            CellValue::Float(v) => v.to_string(),
            CellValue::Bool(v) => v.to_string(),
            CellValue::DateTime(v) => v.format("%Y-%m-%d %H:%M:%S").to_string(),
            CellValue::Date(v) => v.format("%Y-%m-%d").to_string(),
        })
        .collect();
    parts.join("\u{1f}").into_bytes()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// ULID：48-bit 毫秒時間戳 + 80-bit 亂數（取自 UUID v4），Crockford base32 編碼 26 字元。
/// 同毫秒內不保證單調遞增（本工具用途為唯一鍵，足夠）。
fn new_ulid(timestamp_ms: u64) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let rand_bytes = Uuid::new_v4().into_bytes();
    let mut rand80: u128 = 0;
    for &b in &rand_bytes[..10] {
        rand80 = (rand80 << 8) | b as u128;
    }
    let mut value: u128 = ((timestamp_ms as u128 & 0xFFFF_FFFF_FFFF) << 80) | rand80;
    let mut out = [0u8; 26];
    for slot in out.iter_mut().rev() {
        *slot = ALPHABET[(value & 0x1f) as usize];
        value >>= 5;
    }
    String::from_utf8(out.to_vec()).expect("ULID 字元集為 ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_and_ulid_are_unique_per_call() {
        let ctx = GenContext::new();
        let g1 = generate(GenKind::Guid, &[], &ctx);
        let g2 = generate(GenKind::Guid, &[], &ctx);
        assert_ne!(g1, g2);
        let CellValue::Text(s) = g1 else { panic!() };
        assert_eq!(s.len(), 36); // xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx

        let u1 = generate(GenKind::Ulid, &[], &ctx);
        let u2 = generate(GenKind::Ulid, &[], &ctx);
        assert_ne!(u1, u2);
        let CellValue::Text(s) = u1 else { panic!() };
        assert_eq!(s.len(), 26);
        assert!(s
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b)));
    }

    #[test]
    fn ulid_orders_by_timestamp() {
        let a = new_ulid(1_000);
        let b = new_ulid(2_000);
        assert!(a < b); // Crockford base32 保持時間字典序
    }

    #[test]
    fn datetime_kinds_use_context_snapshot() {
        let ctx = GenContext::new();
        let d1 = generate(GenKind::DateTimeText, &[], &ctx);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let d2 = generate(GenKind::DateTimeText, &[], &ctx);
        assert_eq!(d1, d2); // 同一任務內時間一致

        let CellValue::Text(off) = generate(GenKind::DateTimeOffset, &[], &ctx) else {
            panic!()
        };
        assert!(off.contains('T') && (off.contains('+') || off.contains('-')));
    }

    #[test]
    fn hashes_are_deterministic_and_row_sensitive() {
        let ctx = GenContext::new();
        let row_a = vec![CellValue::Text("alice".into()), CellValue::Int(1)];
        let row_b = vec![CellValue::Text("bob".into()), CellValue::Int(2)];
        let h1 = generate(GenKind::Sha256, &row_a, &ctx);
        let h2 = generate(GenKind::Sha256, &row_a, &ctx);
        let h3 = generate(GenKind::Sha256, &row_b, &ctx);
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        let CellValue::Text(s) = h1 else { panic!() };
        assert_eq!(s.len(), 64);

        let CellValue::Text(s) = generate(GenKind::Sha512, &row_a, &ctx) else {
            panic!()
        };
        assert_eq!(s.len(), 128);
        let CellValue::Text(s) = generate(GenKind::Md5, &row_a, &ctx) else {
            panic!()
        };
        assert_eq!(s.len(), 32);
    }
}
