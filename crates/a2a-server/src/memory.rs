//! The stores kept in memory, which a server uses by default.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use composable_agents::model_types::ModelExchange;
use composable_agents::types::{AgentEvent, Artifact, Message, Task, TaskState, TaskStatus};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::store::{
    ExchangeStore, StoreError, StoreResult, TaskPage, TaskQuery, TaskStore, is_interrupted,
    is_terminal,
};

/// How many tasks an [`InMemoryTaskStore`] keeps by default.
pub const DEFAULT_TASK_CAPACITY: usize = 256;

/// How many contexts an [`InMemoryExchangeStore`] keeps by default.
pub const DEFAULT_CONTEXT_CAPACITY: usize = 256;

/// Tasks kept in memory, up to a capacity. Past it, the store evicts the task
/// in a terminal state whose status is oldest. With none of those, it evicts
/// the oldest interrupted task (`input-required` or `auth-required`). It
/// never evicts a task that is `submitted` or `working`, so it holds more
/// than its capacity while that many are active.
pub struct InMemoryTaskStore {
    tasks: Mutex<HashMap<String, Entry>>,
    capacity: usize,
}

struct Entry {
    task: Task,
    // The time of the task's status, which orders the tasks.
    status_time: OffsetDateTime,
}

impl InMemoryTaskStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            capacity,
        }
    }

    // Updates the task with `id`, unless the store doesn't have it or it is in
    // a terminal state. Then evicts down to the capacity, since an update can
    // put a task in a terminal state.
    fn update(&self, id: &str, update: impl FnOnce(&mut Entry)) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(entry) = tasks.get_mut(id)
            && !is_terminal(entry.task.status.state)
        {
            update(entry);
        }
        evict(&mut tasks, self.capacity);
    }
}

impl Default for InMemoryTaskStore {
    fn default() -> Self {
        Self::new(DEFAULT_TASK_CAPACITY)
    }
}

impl Entry {
    fn set_status(&mut self, status: TaskStatus) {
        self.status_time = status_time(&status);
        self.task.status = status;
    }
}

// The time of `status` as its timestamp, or the current time when it has none
// or one that isn't RFC 3339.
fn status_time(status: &TaskStatus) -> OffsetDateTime {
    status
        .timestamp
        .as_deref()
        .and_then(|timestamp| OffsetDateTime::parse(timestamp, &Rfc3339).ok())
        .unwrap_or_else(OffsetDateTime::now_utc)
}

// Evicts tasks until `capacity` remain or every remaining task is active.
fn evict(tasks: &mut HashMap<String, Entry>, capacity: usize) {
    while tasks.len() > capacity {
        // The id of the task with the oldest status among those in a state
        // that `evictable` accepts.
        let oldest = |evictable: fn(TaskState) -> bool| {
            tasks
                .iter()
                .filter(|(_, entry)| evictable(entry.task.status.state))
                .min_by_key(|(_, entry)| entry.status_time)
                .map(|(id, _)| id.clone())
        };
        let Some(id) = oldest(is_terminal).or_else(|| oldest(is_interrupted)) else {
            return;
        };
        tasks.remove(&id);
    }
}

// A page token names the last task of its page by the time of its status and
// its id, which is where the next page starts after.
fn page_token(status_time: OffsetDateTime, id: &str) -> String {
    format!("{}:{id}", status_time.unix_timestamp_nanos())
}

fn parse_page_token(token: &str) -> StoreResult<(OffsetDateTime, &str)> {
    token
        .split_once(':')
        .and_then(|(nanos, id)| {
            let nanos = nanos.parse().ok()?;
            let time = OffsetDateTime::from_unix_timestamp_nanos(nanos).ok()?;
            Some((time, id))
        })
        .ok_or(StoreError::InvalidPageToken)
}

impl TaskStore for InMemoryTaskStore {
    async fn insert(&self, task: Task) -> StoreResult<()> {
        let mut tasks = self.tasks.lock().unwrap();
        let entry = Entry {
            status_time: status_time(&task.status),
            task,
        };
        tasks.insert(entry.task.id.clone(), entry);
        evict(&mut tasks, self.capacity);
        Ok(())
    }

    async fn get(&self, id: &str) -> StoreResult<Option<Task>> {
        let tasks = self.tasks.lock().unwrap();
        Ok(tasks.get(id).map(|entry| entry.task.clone()))
    }

    async fn apply(&self, event: AgentEvent) -> StoreResult<()> {
        match event {
            AgentEvent::Status(event) => {
                self.update(&event.task_id, |entry| entry.set_status(event.status));
            }
            AgentEvent::Artifact(event) => self.update(&event.task_id, |entry| {
                let artifacts = &mut entry.task.artifacts;
                let artifact = event.artifact;
                match artifacts
                    .iter_mut()
                    .find(|existing| existing.artifact_id == artifact.artifact_id)
                {
                    Some(existing) if event.append => existing.parts.extend(artifact.parts),
                    Some(existing) => *existing = artifact,
                    None => artifacts.push(artifact),
                }
            }),
        }
        Ok(())
    }

    async fn add_history(&self, id: &str, message: Message) -> StoreResult<()> {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(entry) = tasks.get_mut(id) {
            entry.task.history.push(message);
        }
        Ok(())
    }

    async fn finish(
        &self,
        id: &str,
        status: TaskStatus,
        artifacts: Vec<Artifact>,
    ) -> StoreResult<()> {
        self.update(id, |entry| {
            entry.set_status(status);
            entry.task.artifacts = artifacts;
        });
        Ok(())
    }

    async fn list(&self, query: TaskQuery) -> StoreResult<TaskPage> {
        let after = query
            .status_timestamp_after
            .as_deref()
            .map(|timestamp| {
                OffsetDateTime::parse(timestamp, &Rfc3339).map_err(|_| {
                    StoreError::Failed(format!("'{timestamp}' is not an RFC 3339 timestamp"))
                })
            })
            .transpose()?;
        let start_after = query
            .page_token
            .as_deref()
            .map(parse_page_token)
            .transpose()?;

        let tasks = self.tasks.lock().unwrap();
        let mut matched: Vec<&Entry> = tasks
            .values()
            .filter(|entry| {
                let task = &entry.task;
                query
                    .context_id
                    .as_ref()
                    .is_none_or(|id| task.context_id.as_ref() == Some(id))
                    && query.state.is_none_or(|state| task.status.state == state)
                    && after.is_none_or(|after| entry.status_time >= after)
            })
            .collect();
        // The most recent status first, and by id where two are equal.
        matched.sort_by(|a, b| {
            b.status_time
                .cmp(&a.status_time)
                .then_with(|| a.task.id.cmp(&b.task.id))
        });
        let total_size = matched.len();

        let start = match start_after {
            None => 0,
            Some((time, id)) => matched
                .iter()
                .position(|entry| {
                    entry.status_time < time
                        || (entry.status_time == time && entry.task.id.as_str() > id)
                })
                .unwrap_or(total_size),
        };
        let page_size = query.page_size.max(1) as usize;
        let page = &matched[start..total_size.min(start + page_size)];
        let next_page_token = match page.last() {
            Some(last) if start + page.len() < total_size => {
                Some(page_token(last.status_time, &last.task.id))
            }
            _ => None,
        };
        Ok(TaskPage {
            tasks: page.iter().map(|entry| entry.task.clone()).collect(),
            next_page_token,
            total_size: total_size as u32,
        })
    }
}

/// Each context's model exchanges kept in memory, up to a capacity in
/// contexts. Past it, the store evicts the context that was appended to least
/// recently, and that context's conversation starts again.
pub struct InMemoryExchangeStore {
    // The contexts, the one appended to least recently first.
    contexts: Mutex<VecDeque<(String, Vec<ModelExchange>)>>,
    capacity: usize,
}

impl InMemoryExchangeStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            contexts: Mutex::new(VecDeque::new()),
            capacity,
        }
    }
}

impl Default for InMemoryExchangeStore {
    fn default() -> Self {
        Self::new(DEFAULT_CONTEXT_CAPACITY)
    }
}

impl ExchangeStore for InMemoryExchangeStore {
    async fn history(&self, context_id: &str) -> StoreResult<Vec<ModelExchange>> {
        let contexts = self.contexts.lock().unwrap();
        Ok(contexts
            .iter()
            .find(|(id, _)| id == context_id)
            .map(|(_, exchanges)| exchanges.clone())
            .unwrap_or_default())
    }

    async fn append(&self, context_id: &str, added: Vec<ModelExchange>) -> StoreResult<()> {
        if added.is_empty() {
            return Ok(());
        }
        let mut contexts = self.contexts.lock().unwrap();
        let mut exchanges = contexts
            .iter()
            .position(|(id, _)| id == context_id)
            .and_then(|index| contexts.remove(index))
            .map(|(_, exchanges)| exchanges)
            .unwrap_or_default();
        exchanges.extend(added);
        while contexts.len() >= self.capacity {
            contexts.pop_front();
        }
        contexts.push_back((context_id.to_string(), exchanges));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use composable_agents::model_types::{ModelInputItem, ModelRequest, TokenUsage};
    use composable_agents::types::{
        Part, PartContent, TaskArtifactUpdateEvent, TaskState, TaskStatusUpdateEvent,
    };

    use super::*;

    // A status at `minute` past 10:00 on one day.
    fn status(state: TaskState, minute: u8) -> TaskStatus {
        TaskStatus {
            state,
            message: None,
            timestamp: Some(format!("2026-11-19T10:{minute:02}:00Z")),
        }
    }

    fn task(id: &str, context_id: &str, state: TaskState, minute: u8) -> Task {
        Task {
            id: id.to_string(),
            context_id: Some(context_id.to_string()),
            status: status(state, minute),
            artifacts: Vec::new(),
            history: Vec::new(),
            metadata: None,
        }
    }

    fn status_event(id: &str, state: TaskState, minute: u8) -> AgentEvent {
        AgentEvent::Status(TaskStatusUpdateEvent {
            task_id: id.to_string(),
            context_id: "c1".to_string(),
            status: status(state, minute),
            metadata: None,
        })
    }

    fn artifact(id: &str, text: &str) -> Artifact {
        Artifact {
            artifact_id: id.to_string(),
            name: None,
            description: None,
            parts: vec![Part {
                content: PartContent::Text(text.to_string()),
                metadata: None,
                filename: None,
                media_type: None,
            }],
            metadata: None,
            extensions: Vec::new(),
        }
    }

    fn artifact_event(id: &str, artifact: Artifact, append: bool) -> AgentEvent {
        AgentEvent::Artifact(TaskArtifactUpdateEvent {
            task_id: id.to_string(),
            context_id: "c1".to_string(),
            artifact,
            append,
            last_chunk: false,
            metadata: None,
        })
    }

    // The text of each part of each of a task's artifacts.
    fn texts(task: &Task) -> Vec<Vec<String>> {
        task.artifacts
            .iter()
            .map(|artifact| {
                artifact
                    .parts
                    .iter()
                    .map(|part| match &part.content {
                        PartContent::Text(text) => text.clone(),
                        _ => panic!("expected text"),
                    })
                    .collect()
            })
            .collect()
    }

    fn ids(page: &TaskPage) -> Vec<&str> {
        page.tasks.iter().map(|task| task.id.as_str()).collect()
    }

    fn query(page_size: u32) -> TaskQuery {
        TaskQuery {
            page_size,
            ..TaskQuery::default()
        }
    }

    #[tokio::test]
    async fn a_status_event_replaces_the_status_until_the_task_is_terminal() {
        let store = InMemoryTaskStore::new(8);
        store
            .insert(task("t1", "c1", TaskState::Submitted, 0))
            .await
            .unwrap();
        assert!(store.get("t2").await.unwrap().is_none());

        store
            .apply(status_event("t1", TaskState::Working, 1))
            .await
            .unwrap();
        let task = store.get("t1").await.unwrap().unwrap();
        assert!(matches!(task.status.state, TaskState::Working));

        store
            .apply(status_event("t1", TaskState::Canceled, 2))
            .await
            .unwrap();
        store
            .apply(status_event("t1", TaskState::Working, 3))
            .await
            .unwrap();
        let task = store.get("t1").await.unwrap().unwrap();
        assert!(matches!(task.status.state, TaskState::Canceled));
    }

    #[tokio::test]
    async fn chunks_are_added_to_their_artifact_and_a_resent_artifact_replaces_it() {
        let store = InMemoryTaskStore::new(8);
        store
            .insert(task("t1", "c1", TaskState::Working, 0))
            .await
            .unwrap();
        for (artifact, append) in [
            (artifact("a1", "Hel"), false),
            (artifact("a1", "lo"), true),
            (artifact("a2", "other"), false),
        ] {
            store
                .apply(artifact_event("t1", artifact, append))
                .await
                .unwrap();
        }
        let task = store.get("t1").await.unwrap().unwrap();
        assert_eq!(texts(&task), [vec!["Hel", "lo"], vec!["other"]]);

        store
            .apply(artifact_event("t1", artifact("a1", "Hello"), false))
            .await
            .unwrap();
        let task = store.get("t1").await.unwrap().unwrap();
        assert_eq!(texts(&task), [vec!["Hello"], vec!["other"]]);
    }

    #[tokio::test]
    async fn finishing_a_task_sets_its_status_and_its_assembled_artifacts() {
        let store = InMemoryTaskStore::new(8);
        store
            .insert(task("t1", "c1", TaskState::Working, 0))
            .await
            .unwrap();
        store
            .apply(artifact_event("t1", artifact("a1", "Hel"), false))
            .await
            .unwrap();
        store
            .finish(
                "t1",
                status(TaskState::Completed, 1),
                vec![artifact("a1", "Hello")],
            )
            .await
            .unwrap();

        let task = store.get("t1").await.unwrap().unwrap();
        assert!(matches!(task.status.state, TaskState::Completed));
        assert_eq!(texts(&task), [vec!["Hello"]]);
    }

    #[tokio::test]
    async fn tasks_are_listed_by_the_time_of_their_status_most_recent_first() {
        let store = InMemoryTaskStore::new(8);
        for (id, minute) in [("t1", 1), ("t2", 3), ("t3", 2)] {
            store
                .insert(task(id, "c1", TaskState::Working, minute))
                .await
                .unwrap();
        }
        let page = store.list(query(10)).await.unwrap();
        assert_eq!(ids(&page), ["t2", "t3", "t1"]);
        assert_eq!(page.total_size, 3);
        assert!(page.next_page_token.is_none());

        // A new status moves its task to the front.
        store
            .apply(status_event("t1", TaskState::Completed, 4))
            .await
            .unwrap();
        let page = store.list(query(10)).await.unwrap();
        assert_eq!(ids(&page), ["t1", "t2", "t3"]);
    }

    #[tokio::test]
    async fn a_list_is_limited_by_context_state_and_status_time() {
        let store = InMemoryTaskStore::new(8);
        for task in [
            task("t1", "c1", TaskState::Completed, 1),
            task("t2", "c1", TaskState::Working, 2),
            task("t3", "c2", TaskState::Working, 3),
        ] {
            store.insert(task).await.unwrap();
        }
        let list = async |query: TaskQuery| {
            let page = store.list(query).await.unwrap();
            let ids: Vec<String> = page.tasks.into_iter().map(|task| task.id).collect();
            (ids, page.total_size)
        };

        let in_context = TaskQuery {
            context_id: Some("c1".to_string()),
            ..query(10)
        };
        assert_eq!(list(in_context).await.0, ["t2", "t1"]);

        let working = TaskQuery {
            state: Some(TaskState::Working),
            ..query(10)
        };
        assert_eq!(list(working).await.0, ["t3", "t2"]);

        // A task whose status has the given time is included.
        let recent = TaskQuery {
            status_timestamp_after: Some("2026-11-19T10:02:00Z".to_string()),
            ..query(10)
        };
        assert_eq!(
            list(recent).await,
            (vec!["t3".to_string(), "t2".to_string()], 2)
        );
    }

    #[tokio::test]
    async fn each_page_starts_after_the_last_task_of_the_page_before() {
        let store = InMemoryTaskStore::new(8);
        for (id, minute) in [("t1", 1), ("t2", 2), ("t3", 3), ("t4", 3), ("t5", 5)] {
            store
                .insert(task(id, "c1", TaskState::Working, minute))
                .await
                .unwrap();
        }
        // t3 and t4 have the same status time, so they are in order of id.
        let mut seen = Vec::new();
        let mut page_token = None;
        loop {
            let page = store
                .list(TaskQuery {
                    page_token,
                    ..query(2)
                })
                .await
                .unwrap();
            assert_eq!(page.total_size, 5);
            seen.push(
                page.tasks
                    .iter()
                    .map(|task| task.id.clone())
                    .collect::<Vec<_>>(),
            );
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        assert_eq!(seen, [vec!["t5", "t3"], vec!["t4", "t2"], vec!["t1"]]);
    }

    #[tokio::test]
    async fn a_page_token_the_store_did_not_return_is_not_valid() {
        let store = InMemoryTaskStore::new(8);
        let result = store
            .list(TaskQuery {
                page_token: Some("next".to_string()),
                ..query(2)
            })
            .await;
        assert_eq!(result.err(), Some(StoreError::InvalidPageToken));
    }

    #[tokio::test]
    async fn past_its_capacity_the_store_evicts_terminal_tasks_then_interrupted_ones() {
        let store = InMemoryTaskStore::new(2);
        for task in [
            task("completed", "c1", TaskState::Completed, 1),
            task("interrupted", "c1", TaskState::InputRequired, 2),
            task("failed", "c1", TaskState::Failed, 3),
            task("active", "c1", TaskState::Working, 4),
        ] {
            store.insert(task).await.unwrap();
        }
        // The two terminal tasks were evicted.
        let page = store.list(query(10)).await.unwrap();
        assert_eq!(ids(&page), ["active", "interrupted"]);

        // With no terminal task, the interrupted one is evicted.
        store
            .insert(task("active-2", "c1", TaskState::Working, 5))
            .await
            .unwrap();
        let page = store.list(query(10)).await.unwrap();
        assert_eq!(ids(&page), ["active-2", "active"]);
    }

    #[tokio::test]
    async fn an_active_task_is_never_evicted() {
        let store = InMemoryTaskStore::new(2);
        for (id, minute) in [("t1", 1), ("t2", 2), ("t3", 3)] {
            store
                .insert(task(id, "c1", TaskState::Working, minute))
                .await
                .unwrap();
        }
        assert_eq!(store.list(query(10)).await.unwrap().total_size, 3);

        // When one reaches a terminal state, the store evicts to its capacity.
        store
            .finish("t2", status(TaskState::Completed, 4), Vec::new())
            .await
            .unwrap();
        let page = store.list(query(10)).await.unwrap();
        assert_eq!(ids(&page), ["t3", "t1"]);
    }

    fn exchange(text: &str) -> ModelExchange {
        ModelExchange {
            request: ModelRequest {
                input: vec![ModelInputItem::Text(text.to_string())],
                max_tokens: None,
            },
            output: None,
            opaque_content: None,
            provider: None,
            request_model: None,
            response_model: None,
            usage: TokenUsage {
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
        }
    }

    async fn inputs(store: &InMemoryExchangeStore, context_id: &str) -> Vec<String> {
        store
            .history(context_id)
            .await
            .unwrap()
            .iter()
            .map(|exchange| match &exchange.request.input[0] {
                ModelInputItem::Text(text) => text.clone(),
                _ => panic!("expected text"),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_context_keeps_its_exchanges_in_order() {
        let store = InMemoryExchangeStore::new(8);
        store.append("c1", vec![exchange("one")]).await.unwrap();
        store.append("c2", vec![exchange("other")]).await.unwrap();
        store
            .append("c1", vec![exchange("two"), exchange("three")])
            .await
            .unwrap();

        assert_eq!(inputs(&store, "c1").await, ["one", "two", "three"]);
        assert_eq!(inputs(&store, "c2").await, ["other"]);
        assert!(inputs(&store, "c3").await.is_empty());
    }

    #[tokio::test]
    async fn past_its_capacity_the_store_evicts_the_context_appended_to_least_recently() {
        let store = InMemoryExchangeStore::new(2);
        store.append("c1", vec![exchange("one")]).await.unwrap();
        store.append("c2", vec![exchange("two")]).await.unwrap();
        store.append("c1", vec![exchange("again")]).await.unwrap();
        store.append("c3", vec![exchange("three")]).await.unwrap();

        assert!(inputs(&store, "c2").await.is_empty());
        assert_eq!(inputs(&store, "c1").await, ["one", "again"]);
        assert_eq!(inputs(&store, "c3").await, ["three"]);
    }
}
