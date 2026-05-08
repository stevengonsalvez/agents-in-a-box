// ABOUTME: Thread-safe store for caching tool results
// Provides concurrent access to tool call tracking and result storage

use super::ToolResult;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Information about a pending tool call
#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub timestamp: Instant,
}

/// Thread-safe store for tool results
#[derive(Debug, Clone)]
pub struct ToolResultStore {
    /// Map from tool_use_id to tool results
    results: Arc<RwLock<HashMap<String, ToolResult>>>,
    /// Map from tool_use_id to pending tool calls
    pending: Arc<RwLock<HashMap<String, PendingToolCall>>>,
    /// Optional: track timestamps for cleanup
    timestamps: Arc<RwLock<HashMap<String, Instant>>>,
}

impl ToolResultStore {
    /// Create a new empty tool result store
    pub fn new() -> Self {
        Self {
            results: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
            timestamps: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new tool call that we're waiting for results from
    pub fn register_tool_call(&self, id: String, name: String, input: Value) -> Result<(), String> {
        let mut pending = self.pending.write()
            .map_err(|e| format!("Failed to acquire write lock on pending: {}", e))?;

        let mut timestamps = self.timestamps.write()
            .map_err(|e| format!("Failed to acquire write lock on timestamps: {}", e))?;

        let now = Instant::now();
        pending.insert(id.clone(), PendingToolCall {
            id: id.clone(),
            name,
            input,
            timestamp: now,
        });
        timestamps.insert(id, now);

        Ok(())
    }

    /// Store a tool result
    pub fn store_result(&self, tool_use_id: String, content: Value, is_error: bool) -> Result<(), String> {
        // Remove from pending if it exists
        if let Ok(mut pending) = self.pending.write() {
            pending.remove(&tool_use_id);
        }

        let mut results = self.results.write()
            .map_err(|e| format!("Failed to acquire write lock on results: {}", e))?;

        let mut timestamps = self.timestamps.write()
            .map_err(|e| format!("Failed to acquire write lock on timestamps: {}", e))?;

        results.insert(tool_use_id.clone(), ToolResult {
            tool_use_id: tool_use_id.clone(),
            content,
            is_error,
        });

        timestamps.insert(tool_use_id, Instant::now());

        Ok(())
    }

    /// Get a tool result by ID
    pub fn get_result(&self, tool_use_id: &str) -> Option<ToolResult> {
        self.results.read()
            .ok()
            .and_then(|results| results.get(tool_use_id).cloned())
    }

    /// Get a pending tool call by ID
    pub fn get_pending(&self, tool_use_id: &str) -> Option<PendingToolCall> {
        self.pending.read()
            .ok()
            .and_then(|pending| pending.get(tool_use_id).cloned())
    }

    /// Check if a tool call is pending
    pub fn is_pending(&self, tool_use_id: &str) -> bool {
        self.pending.read()
            .ok()
            .map(|pending| pending.contains_key(tool_use_id))
            .unwrap_or(false)
    }

    /// Get all pending tool calls
    pub fn get_all_pending(&self) -> Vec<PendingToolCall> {
        self.pending.read()
            .ok()
            .map(|pending| pending.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all stored results
    pub fn get_all_results(&self) -> Vec<ToolResult> {
        self.results.read()
            .ok()
            .map(|results| results.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Clear old results based on age
    pub fn clear_old_results(&self, max_age: Duration) -> Result<usize, String> {
        let now = Instant::now();

        let timestamps = self.timestamps.read()
            .map_err(|e| format!("Failed to acquire read lock on timestamps: {}", e))?;

        // Find IDs to remove
        let ids_to_remove: Vec<String> = timestamps
            .iter()
            .filter_map(|(id, timestamp)| {
                if now.duration_since(*timestamp) > max_age {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();

        drop(timestamps); // Release read lock

        let count = ids_to_remove.len();

        if count > 0 {
            let mut results = self.results.write()
                .map_err(|e| format!("Failed to acquire write lock on results: {}", e))?;
            let mut pending = self.pending.write()
                .map_err(|e| format!("Failed to acquire write lock on pending: {}", e))?;
            let mut timestamps = self.timestamps.write()
                .map_err(|e| format!("Failed to acquire write lock on timestamps: {}", e))?;

            for id in ids_to_remove {
                results.remove(&id);
                pending.remove(&id);
                timestamps.remove(&id);
            }
        }

        Ok(count)
    }

    /// Clear all stored data
    pub fn clear_all(&self) -> Result<(), String> {
        let mut results = self.results.write()
            .map_err(|e| format!("Failed to acquire write lock on results: {}", e))?;
        let mut pending = self.pending.write()
            .map_err(|e| format!("Failed to acquire write lock on pending: {}", e))?;
        let mut timestamps = self.timestamps.write()
            .map_err(|e| format!("Failed to acquire write lock on timestamps: {}", e))?;

        results.clear();
        pending.clear();
        timestamps.clear();

        Ok(())
    }

    /// Get the count of stored results
    pub fn result_count(&self) -> usize {
        self.results.read()
            .ok()
            .map(|results| results.len())
            .unwrap_or(0)
    }

    /// Get the count of pending tool calls
    pub fn pending_count(&self) -> usize {
        self.pending.read()
            .ok()
            .map(|pending| pending.len())
            .unwrap_or(0)
    }
}

impl Default for ToolResultStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_new_store() {
        let store = ToolResultStore::new();
        assert_eq!(store.result_count(), 0);
        assert_eq!(store.pending_count(), 0);
    }

    #[test]
    fn test_register_tool_call() {
        let store = ToolResultStore::new();
        let result = store.register_tool_call(
            "test-id-1".to_string(),
            "TestTool".to_string(),
            json!({"param": "value"}),
        );

        assert!(result.is_ok());
        assert_eq!(store.pending_count(), 1);
        assert!(store.is_pending("test-id-1"));

        let pending = store.get_pending("test-id-1");
        assert!(pending.is_some());
        let pending = pending.unwrap();
        assert_eq!(pending.id, "test-id-1");
        assert_eq!(pending.name, "TestTool");
        assert_eq!(pending.input, json!({"param": "value"}));
    }

    #[test]
    fn test_store_and_get_result() {
        let store = ToolResultStore::new();

        // Register a tool call
        store.register_tool_call(
            "test-id-1".to_string(),
            "TestTool".to_string(),
            json!({"param": "value"}),
        ).unwrap();

        assert_eq!(store.pending_count(), 1);

        // Store the result
        let result = store.store_result(
            "test-id-1".to_string(),
            json!({"output": "success"}),
            false,
        );

        assert!(result.is_ok());
        assert_eq!(store.result_count(), 1);
        assert_eq!(store.pending_count(), 0); // Should be removed from pending
        assert!(!store.is_pending("test-id-1"));

        // Get the result
        let stored = store.get_result("test-id-1");
        assert!(stored.is_some());
        let stored = stored.unwrap();
        assert_eq!(stored.tool_use_id, "test-id-1");
        assert_eq!(stored.content, json!({"output": "success"}));
        assert!(!stored.is_error);
    }

    #[test]
    fn test_store_error_result() {
        let store = ToolResultStore::new();

        store.store_result(
            "error-id".to_string(),
            json!({"error": "Something went wrong"}),
            true,
        ).unwrap();

        let result = store.get_result("error-id");
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.is_error);
        assert_eq!(result.content, json!({"error": "Something went wrong"}));
    }

    #[test]
    fn test_get_all_pending() {
        let store = ToolResultStore::new();

        store.register_tool_call(
            "id-1".to_string(),
            "Tool1".to_string(),
            json!({"a": 1}),
        ).unwrap();

        store.register_tool_call(
            "id-2".to_string(),
            "Tool2".to_string(),
            json!({"b": 2}),
        ).unwrap();

        let pending = store.get_all_pending();
        assert_eq!(pending.len(), 2);

        let ids: Vec<String> = pending.iter().map(|p| p.id.clone()).collect();
        assert!(ids.contains(&"id-1".to_string()));
        assert!(ids.contains(&"id-2".to_string()));
    }

    #[test]
    fn test_get_all_results() {
        let store = ToolResultStore::new();

        store.store_result(
            "id-1".to_string(),
            json!({"result": 1}),
            false,
        ).unwrap();

        store.store_result(
            "id-2".to_string(),
            json!({"result": 2}),
            false,
        ).unwrap();

        let results = store.get_all_results();
        assert_eq!(results.len(), 2);

        let ids: Vec<String> = results.iter().map(|r| r.tool_use_id.clone()).collect();
        assert!(ids.contains(&"id-1".to_string()));
        assert!(ids.contains(&"id-2".to_string()));
    }

    #[test]
    fn test_clear_old_results() {
        let store = ToolResultStore::new();

        // Add some results
        store.store_result(
            "old-1".to_string(),
            json!({"data": "old"}),
            false,
        ).unwrap();

        store.register_tool_call(
            "old-pending".to_string(),
            "OldTool".to_string(),
            json!({}),
        ).unwrap();

        // Wait a bit
        thread::sleep(Duration::from_millis(100));

        // Add a newer result
        store.store_result(
            "new-1".to_string(),
            json!({"data": "new"}),
            false,
        ).unwrap();

        // Clear results older than 50ms
        let cleared = store.clear_old_results(Duration::from_millis(50)).unwrap();
        assert_eq!(cleared, 2); // old-1 and old-pending

        // Check what remains
        assert!(store.get_result("old-1").is_none());
        assert!(store.get_result("new-1").is_some());
        assert!(!store.is_pending("old-pending"));
    }

    #[test]
    fn test_clear_all() {
        let store = ToolResultStore::new();

        store.register_tool_call(
            "pending-1".to_string(),
            "Tool".to_string(),
            json!({}),
        ).unwrap();

        store.store_result(
            "result-1".to_string(),
            json!({"data": "test"}),
            false,
        ).unwrap();

        assert_eq!(store.pending_count(), 1);
        assert_eq!(store.result_count(), 1);

        store.clear_all().unwrap();

        assert_eq!(store.pending_count(), 0);
        assert_eq!(store.result_count(), 0);
    }

    #[test]
    fn test_thread_safety() {
        let store = Arc::new(ToolResultStore::new());
        let mut handles = vec![];

        // Spawn multiple threads that write
        for i in 0..10 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for j in 0..10 {
                    let id = format!("thread-{}-item-{}", i, j);
                    store_clone.register_tool_call(
                        id.clone(),
                        format!("Tool-{}", i),
                        json!({"thread": i, "item": j}),
                    ).unwrap();

                    store_clone.store_result(
                        id,
                        json!({"result": format!("{}-{}", i, j)}),
                        false,
                    ).unwrap();
                }
            });
            handles.push(handle);
        }

        // Spawn threads that read
        for i in 0..5 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for _ in 0..20 {
                    let _ = store_clone.get_all_results();
                    let _ = store_clone.get_all_pending();
                    let _ = store_clone.result_count();
                    thread::sleep(Duration::from_millis(1));
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify final state
        assert_eq!(store.result_count(), 100);
        assert_eq!(store.pending_count(), 0); // All should be completed
    }

    #[test]
    fn test_get_nonexistent() {
        let store = ToolResultStore::new();

        assert!(store.get_result("nonexistent").is_none());
        assert!(store.get_pending("nonexistent").is_none());
        assert!(!store.is_pending("nonexistent"));
    }

    #[test]
    fn test_overwrite_result() {
        let store = ToolResultStore::new();

        // Store initial result
        store.store_result(
            "id-1".to_string(),
            json!({"version": 1}),
            false,
        ).unwrap();

        // Overwrite with new result
        store.store_result(
            "id-1".to_string(),
            json!({"version": 2}),
            true,
        ).unwrap();

        let result = store.get_result("id-1").unwrap();
        assert_eq!(result.content, json!({"version": 2}));
        assert!(result.is_error);
        assert_eq!(store.result_count(), 1); // Should still be just one
    }
}