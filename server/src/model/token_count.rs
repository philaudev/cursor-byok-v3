//! Estimates and records model token usage.
pub(crate) fn parse_token_count(value: &str) -> Option<u64> {
    let value = value.trim().to_ascii_lowercase();
    let (number, multiplier) = match value.chars().last()? {
        'k' => (&value[..value.len() - 1], 1_000),
        'm' => (&value[..value.len() - 1], 1_000_000),
        _ => (value.as_str(), 1),
    };
    number.parse::<u64>().ok()?.checked_mul(multiplier)
}

pub(crate) fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 && tokens.is_multiple_of(1_000_000) {
        format!("{}M", tokens / 1_000_000)
    } else if tokens >= 1_000 && tokens.is_multiple_of(1_000) {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}
