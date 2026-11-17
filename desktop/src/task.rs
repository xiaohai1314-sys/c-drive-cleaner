//! 通用长任务状态：开始、阶段、计数、取消、结束。
//!
//! 前端轮询 `task_status` 显示全局任务坞；变更类任务同时只允许一个。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Serialize, Clone)]
pub struct TaskStatus {
    pub id: String,
    pub title: String,
    pub stage: String,
    pub done: u64,
    pub total: u64,
    pub unit: String,
    pub active: bool,
    pub cancellable: bool,
    pub cancelling: bool,
    pub outcome: String,
    pub summary: String,
    pub started_ms: u64,
    pub finished_ms: u64,
    pub mutating: bool,
}

struct Task {
    title: Mutex<String>,
    stage: Mutex<String>,
    done: AtomicU64,
    total: AtomicU64,
    unit: Mutex<String>,
    cancellable: bool,
    mutating: bool,
    cancel: AtomicBool,
    active: AtomicBool,
    outcome: Mutex<String>,
    summary: Mutex<String>,
    started_ms: u64,
    finished_ms: AtomicU64,
}

fn registry() -> &'static Mutex<HashMap<String, Arc<Task>>> {
    static CELL: OnceLock<Mutex<HashMap<String, Arc<Task>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub struct TaskGuard {
    task: Arc<Task>,
    finished: AtomicBool,
}

impl TaskGuard {
    pub fn stage(&self, stage: &str) {
        if let Ok(mut value) = self.task.stage.lock() {
            *value = stage.to_string();
        }
    }

    pub fn set_unit(&self, unit: &str) {
        if let Ok(mut value) = self.task.unit.lock() {
            *value = unit.to_string();
        }
    }

    pub fn set_total(&self, total: u64) {
        self.task.total.store(total, Ordering::Relaxed);
    }

    pub fn progress(&self, done: u64, total: u64) {
        self.task.done.store(done, Ordering::Relaxed);
        self.task.total.store(total, Ordering::Relaxed);
    }

    pub fn add_done(&self, amount: u64) {
        self.task.done.fetch_add(amount, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.task.cancel.load(Ordering::Relaxed)
    }

    pub fn finish(&self, outcome: &str, summary: &str) {
        finish_task(&self.task, outcome, summary);
        self.finished.store(true, Ordering::Relaxed);
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.finished.load(Ordering::Relaxed) {
            finish_task(&self.task, "failed", "任务中断");
        }
    }
}

fn finish_task(task: &Task, outcome: &str, summary: &str) {
    task.active.store(false, Ordering::Relaxed);
    if let Ok(mut value) = task.outcome.lock() {
        *value = outcome.to_string();
    }
    if let Ok(mut value) = task.summary.lock() {
        *value = summary.to_string();
    }
    task.finished_ms.store(now_ms(), Ordering::Relaxed);
}

pub fn begin(
    id: &str,
    title: &str,
    cancellable: bool,
    mutating: bool,
) -> Result<TaskGuard, String> {
    let mut map = registry()
        .lock()
        .map_err(|_| "任务状态不可用".to_string())?;
    // 变更类任务互斥；测试会并行调用清理命令，测试构建下不启用。
    #[cfg(not(test))]
    if mutating
        && map
            .values()
            .any(|task| task.mutating && task.active.load(Ordering::Relaxed))
    {
        return Err("已有清理任务正在进行，请等它结束后再试".to_string());
    }
    let task = Arc::new(Task {
        title: Mutex::new(title.to_string()),
        stage: Mutex::new(String::new()),
        done: AtomicU64::new(0),
        total: AtomicU64::new(0),
        unit: Mutex::new(String::new()),
        cancellable,
        mutating,
        cancel: AtomicBool::new(false),
        active: AtomicBool::new(true),
        outcome: Mutex::new("running".to_string()),
        summary: Mutex::new(String::new()),
        started_ms: now_ms(),
        finished_ms: AtomicU64::new(0),
    });
    map.insert(id.to_string(), task.clone());
    Ok(TaskGuard {
        task,
        finished: AtomicBool::new(false),
    })
}

pub fn status() -> HashMap<String, TaskStatus> {
    let now = now_ms();
    let mut map = match registry().lock() {
        Ok(map) => map,
        Err(_) => return HashMap::new(),
    };
    map.retain(|_, task| {
        task.active.load(Ordering::Relaxed)
            || now.saturating_sub(task.finished_ms.load(Ordering::Relaxed)) < 30_000
    });
    let mut out = HashMap::with_capacity(map.len());
    for (id, task) in map.iter() {
        out.insert(
            id.clone(),
            TaskStatus {
                id: id.clone(),
                title: task.title.lock().map(|value| value.clone()).unwrap_or_default(),
                stage: task.stage.lock().map(|value| value.clone()).unwrap_or_default(),
                done: task.done.load(Ordering::Relaxed),
                total: task.total.load(Ordering::Relaxed),
                unit: task.unit.lock().map(|value| value.clone()).unwrap_or_default(),
                active: task.active.load(Ordering::Relaxed),
                cancellable: task.cancellable,
                cancelling: task.cancel.load(Ordering::Relaxed),
                outcome: task
                    .outcome
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_default(),
                summary: task
                    .summary
                    .lock()
                    .map(|value| value.clone())
                    .unwrap_or_default(),
                started_ms: task.started_ms,
                finished_ms: task.finished_ms.load(Ordering::Relaxed),
                mutating: task.mutating,
            },
        );
    }
    out
}

pub fn cancel(id: &str) -> bool {
    let map = match registry().lock() {
        Ok(map) => map,
        Err(_) => return false,
    };
    match map.get(id) {
        Some(task) if task.active.load(Ordering::Relaxed) && task.cancellable => {
            task.cancel.store(true, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

pub fn cancel_all() {
    if let Ok(map) = registry().lock() {
        for task in map.values() {
            if task.active.load(Ordering::Relaxed) && task.cancellable {
                task.cancel.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// 模块内部按 id 更新进度，避免把 guard 一路传进递归函数。
pub fn add_done(id: &str, amount: u64) {
    if let Ok(map) = registry().lock() {
        if let Some(task) = map.get(id) {
            task.done.fetch_add(amount, Ordering::Relaxed);
        }
    }
}

pub fn is_cancelled(id: &str) -> bool {
    registry()
        .lock()
        .ok()
        .and_then(|map| {
            map.get(id)
                .map(|task| task.cancel.load(Ordering::Relaxed))
        })
        .unwrap_or(false)
}

pub fn set_stage(id: &str, stage: &str) {
    if let Ok(map) = registry().lock() {
        if let Some(task) = map.get(id) {
            if let Ok(mut value) = task.stage.lock() {
                *value = stage.to_string();
            }
        }
    }
}

pub fn set_progress(id: &str, done: u64, total: u64) {
    if let Ok(map) = registry().lock() {
        if let Some(task) = map.get(id) {
            task.done.store(done, Ordering::Relaxed);
            task.total.store(total, Ordering::Relaxed);
        }
    }
}

pub fn set_unit(id: &str, unit: &str) {
    if let Ok(map) = registry().lock() {
        if let Some(task) = map.get(id) {
            if let Ok(mut value) = task.unit.lock() {
                *value = unit.to_string();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lifecycle_reports_progress_cancel_and_finish() {
        let task = begin("test_task", "测试任务", true, true).unwrap();
        task.set_unit("个");
        task.set_total(10);
        task.add_done(3);
        task.stage("处理中");

        let snapshot = status();
        let running = snapshot.get("test_task").unwrap();
        assert!(running.active);
        assert_eq!(running.done, 3);
        assert_eq!(running.total, 10);
        assert_eq!(running.unit, "个");
        assert_eq!(running.stage, "处理中");
        assert!(running.cancellable);

        assert!(cancel("test_task"));
        assert!(task.cancelled());

        task.finish("cancelled", "已取消");
        let snapshot = status();
        let finished = snapshot.get("test_task").unwrap();
        assert!(!finished.active);
        assert_eq!(finished.outcome, "cancelled");
        assert_eq!(finished.summary, "已取消");
        assert!(finished.finished_ms >= finished.started_ms);
    }
}
