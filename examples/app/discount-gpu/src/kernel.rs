//! Pure integer computation shared verbatim by the host and device builds.
//! Input: unit price, quantity, discount percent. Output: total, status
//! (0 = success, 1 = invalid percent, 2 = multiplication overflow).
pub fn quote(row: [u64; 3]) -> [u64; 2] {
    if row[2] > 100 {
        return [0, 1];
    }
    let total = match row[0].checked_mul(row[1]) {
        Some(total) => total,
        None => return [0, 2],
    };
    let reduction = total / 100 * row[2] + (total % 100 * row[2]) / 100;
    [total - reduction, 0]
}
