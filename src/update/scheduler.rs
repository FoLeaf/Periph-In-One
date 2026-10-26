use std::time::Duration;

pub const FIRST_CHECK: Duration = Duration::from_secs(120);

pub fn delay(failures: u32, jitter: u64, server: Option<Duration>) -> Duration {
    let seconds = match failures {
        0 => 12 * 3600 + jitter % 1801,
        1 => 15 * 60,
        2 => 3600,
        _ => 6 * 3600,
    };
    Duration::from_secs(seconds).max(server.unwrap_or_default())
}

pub fn idle(
    hidden_seconds: u64,
    visible: bool,
    editing: bool,
    settings: bool,
    busy: bool,
    saved: bool,
) -> bool {
    hidden_seconds >= 30 && !visible && !editing && !settings && !busy && saved
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_and_idle_policy_respect_server_and_user_activity() {
        assert_eq!(delay(1, 0, None).as_secs(), 900);
        assert_eq!(delay(2, 0, None).as_secs(), 3600);
        assert_eq!(
            delay(3, 0, Some(Duration::from_secs(25000))).as_secs(),
            25000
        );
        assert!(idle(30, false, false, false, false, true));
        assert!(!idle(29, false, false, false, false, true));
        assert!(!idle(31, true, false, false, false, true));
        assert!(!idle(31, false, true, false, false, true));
        assert!(!idle(31, false, false, true, false, true));
        assert!(!idle(31, false, false, false, true, true));
        assert!(!idle(31, false, false, false, false, false));
    }
}
