use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::{error, warn};

pub struct TaskSupervisor {
    tasks: Arc<RwLock<HashMap<String, TaskEntry>>>,
}

struct TaskEntry {
    handle: JoinHandle<()>,
    status: TaskStatus,
    restart_count: u32,
    last_restart: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum TaskStatus {
    Running,
    Failed { error: String, at: Instant },
    Restarting { attempt: u32 },
    Stopped,
}

impl Default for TaskSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskSupervisor {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn spawn_supervised<F>(&self, name: &str, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let name_owned = name.to_string();

        let handle = tokio::spawn(async move {
            future.await;
        });

        self.tasks.write().insert(
            name_owned,
            TaskEntry {
                handle,
                status: TaskStatus::Running,
                restart_count: 0,
                last_restart: None,
            },
        );
    }

    pub fn spawn_critical<F, Fut>(&self, name: &str, factory: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let tasks = self.tasks.clone();
        let name_owned = name.to_string();

        let monitor_handle = tokio::spawn(async move {
            let mut restart_count = 0u32;
            let max_restarts = 5u32;
            let backoff_durations = [
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ];

            loop {
                let handle = tokio::spawn(factory());

                {
                    let mut t = tasks.write();
                    if let Some(entry) = t.get_mut(&name_owned) {
                        entry.status = TaskStatus::Running;
                    }
                }

                match handle.await {
                    Ok(()) => {
                        let mut t = tasks.write();
                        if let Some(entry) = t.get_mut(&name_owned) {
                            entry.status = TaskStatus::Stopped;
                        }
                        break;
                    }
                    Err(e) => {
                        restart_count += 1;

                        if restart_count > max_restarts {
                            error!(task = %name_owned, "Task exceeded max restarts ({}), giving up", max_restarts);
                            let mut t = tasks.write();
                            if let Some(entry) = t.get_mut(&name_owned) {
                                entry.status = TaskStatus::Failed {
                                    error: format!("Exceeded max restarts: {}", e),
                                    at: Instant::now(),
                                };
                            }
                            break;
                        }

                        let backoff_idx =
                            (restart_count as usize - 1).min(backoff_durations.len() - 1);
                        let backoff = backoff_durations[backoff_idx];

                        warn!(
                            task = %name_owned,
                            error = %e,
                            attempt = restart_count,
                            backoff_ms = backoff.as_millis() as u64,
                            "Critical task failed, restarting with backoff"
                        );

                        {
                            let mut t = tasks.write();
                            if let Some(entry) = t.get_mut(&name_owned) {
                                entry.status = TaskStatus::Restarting {
                                    attempt: restart_count,
                                };
                                entry.restart_count = restart_count;
                                entry.last_restart = Some(Instant::now());
                            }
                        }

                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        });

        self.tasks.write().insert(
            name.to_string(),
            TaskEntry {
                handle: monitor_handle,
                status: TaskStatus::Running,
                restart_count: 0,
                last_restart: None,
            },
        );
    }

    pub fn task_status(&self, name: &str) -> TaskStatus {
        let mut tasks = self.tasks.write();
        if let Some(entry) = tasks.get_mut(name) {
            if entry.handle.is_finished() {
                entry.status = TaskStatus::Failed {
                    error: "Task exited".into(),
                    at: Instant::now(),
                };
            }
            entry.status.clone()
        } else {
            TaskStatus::Stopped
        }
    }

    pub fn check_all(&self) -> Vec<(String, TaskStatus)> {
        let mut tasks = self.tasks.write();
        tasks
            .iter_mut()
            .map(|(name, entry)| {
                if entry.handle.is_finished() {
                    entry.status = TaskStatus::Failed {
                        error: "Task exited".into(),
                        at: Instant::now(),
                    };
                }
                (name.clone(), entry.status.clone())
            })
            .collect()
    }
}
