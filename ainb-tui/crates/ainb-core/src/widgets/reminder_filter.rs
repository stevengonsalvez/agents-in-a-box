// ABOUTME: Intelligent filtering system for reducing redundant system reminders
// while preserving important security and error notifications

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct ReminderFilter {
    seen_reminders: HashMap<u64, (Instant, u32)>, // hash -> (last_seen, count)
    suppression_duration: Duration,
}

impl ReminderFilter {
    pub fn new() -> Self {
        Self {
            seen_reminders: HashMap::new(),
            suppression_duration: Duration::from_secs(300), // 5 minutes
        }
    }

    pub fn should_show(&mut self, content: &str) -> bool {
        // Always show security warnings
        if content.contains("malicious") || content.contains("security") {
            return true;
        }

        // Check for duplicate suppression
        let hash = self.hash_content(content);

        if let Some((last_seen, count)) = self.seen_reminders.get_mut(&hash) {
            if last_seen.elapsed() < self.suppression_duration {
                *count += 1;
                // Show every 5th occurrence even during suppression
                return *count % 5 == 0;
            }
        }

        // Update seen tracker
        self.seen_reminders.insert(hash, (Instant::now(), 1));
        true
    }

    fn hash_content(&self, content: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }

    /// Clean up old entries to prevent memory growth
    pub fn cleanup_old_entries(&mut self) {
        let now = Instant::now();
        self.seen_reminders.retain(|_, (last_seen, _)| {
            now.duration_since(*last_seen) < Duration::from_secs(3600) // Keep for 1 hour
        });
    }
}

impl Default for ReminderFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_security_warnings_always_shown() {
        let mut filter = ReminderFilter::new();

        let security_msg = "This file looks malicious, be careful!";
        assert!(filter.should_show(security_msg));
        assert!(filter.should_show(security_msg)); // Should show again
        assert!(filter.should_show(security_msg)); // And again
    }

    #[test]
    fn test_duplicate_suppression() {
        let mut filter = ReminderFilter::new();

        let msg = "TodoWrite tool hasn't been used recently";
        assert!(filter.should_show(msg)); // First time shows
        assert!(!filter.should_show(msg)); // Second time suppressed
        assert!(!filter.should_show(msg)); // Third time suppressed
        assert!(!filter.should_show(msg)); // Fourth time suppressed
        assert!(filter.should_show(msg)); // Fifth time shows (every 5th)
    }

    #[test]
    fn test_different_messages_not_suppressed() {
        let mut filter = ReminderFilter::new();

        let msg1 = "TodoWrite tool hasn't been used recently";
        let msg2 = "Consider using the Read tool";

        assert!(filter.should_show(msg1));
        assert!(filter.should_show(msg2)); // Different message shows
        assert!(!filter.should_show(msg1)); // First message suppressed
        assert!(!filter.should_show(msg2)); // Second message suppressed
    }

    #[test]
    fn test_suppression_duration() {
        let mut filter = ReminderFilter::new();
        filter.suppression_duration = Duration::from_millis(100); // Short duration for testing

        let msg = "Test reminder";
        assert!(filter.should_show(msg)); // First time shows
        assert!(!filter.should_show(msg)); // Suppressed

        thread::sleep(Duration::from_millis(150)); // Wait for suppression to expire
        assert!(filter.should_show(msg)); // Shows again after duration
    }

    #[test]
    fn test_cleanup_old_entries() {
        let mut filter = ReminderFilter::new();

        // Add some entries
        filter.should_show("Message 1");
        filter.should_show("Message 2");
        filter.should_show("Message 3");

        assert_eq!(filter.seen_reminders.len(), 3);

        // Cleanup should not remove recent entries
        filter.cleanup_old_entries();
        assert_eq!(filter.seen_reminders.len(), 3);
    }
}