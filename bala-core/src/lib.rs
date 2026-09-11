//! Domain logic for Bala: task model, error types, in-memory store, and the
//! `Core` facade.

mod error;
mod facade;
mod hierarchy;
mod in_memory_store;
mod model;
mod store;

pub use error::CoreError;
pub use facade::Core;
pub use in_memory_store::InMemoryStore;
pub use model::{NewTask, Task, TaskId, TaskStatus, TaskType, TreeFilter, User, UserId};
pub use store::{Store, StoreError, StoreTx};
