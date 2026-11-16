//! Plan rows survive a restart and cannot be claimed twice.

use zerith_sessions::plan::{task, NewTask, TaskState};
use zerith_sessions::Store;

fn plan() -> Vec<NewTask> {
    vec![
        NewTask {
            title: "primeiro".into(),
            prompt: "faz o primeiro".into(),
            agent: "default".into(),
            depends_on: vec![],
        },
        NewTask {
            title: "segundo".into(),
            prompt: "faz o segundo".into(),
            agent: "default".into(),
            depends_on: vec!["p-000".into()],
        },
    ]
}

#[test]
fn a_plan_is_written_whole_and_read_back_in_order() {
    let s = Store::open_in_memory().unwrap();
    let tasks = s.create_plan("p", &plan()).unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[1].depends_on, vec!["p-000".to_string()]);
    let back = s.tasks("p").unwrap();
    assert_eq!(back[0].title, "primeiro");
    assert_eq!(back[1].state, TaskState::Pending);
}

#[test]
fn only_one_worker_can_claim_a_task() {
    let s = Store::open_in_memory().unwrap();
    s.create_plan("p", &plan()).unwrap();
    assert!(s.claim_task("p-000").unwrap());
    assert!(!s.claim_task("p-000").unwrap());
    s.finish_task("p-000", TaskState::Done, Some("ok"), None, None)
        .unwrap();
    let t = task(&s, "p-000").unwrap().unwrap();
    assert_eq!(t.state, TaskState::Done);
    assert_eq!(t.result.as_deref(), Some("ok"));
}

#[test]
fn an_interrupted_task_is_requeued_not_lost() {
    let s = Store::open_in_memory().unwrap();
    s.create_plan("p", &plan()).unwrap();
    s.claim_task("p-000").unwrap();
    assert_eq!(s.unfinished_plans().unwrap(), vec!["p".to_string()]);
    assert_eq!(s.requeue_running("p").unwrap(), 1);
    let t = task(&s, "p-000").unwrap().unwrap();
    assert_eq!(t.state, TaskState::Pending);
}

#[test]
fn a_finished_plan_is_not_listed_as_unfinished() {
    let s = Store::open_in_memory().unwrap();
    s.create_plan("p", &plan()).unwrap();
    for id in ["p-000", "p-001"] {
        s.claim_task(id).unwrap();
        s.finish_task(id, TaskState::Done, None, None, None)
            .unwrap();
    }
    assert!(s.unfinished_plans().unwrap().is_empty());
}
