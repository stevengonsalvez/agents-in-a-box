//! Typed repository wrappers over the Hangar schema.
//!
//! Each sub-module is a thin, stateless sqlx layer over one logical group of
//! tables. Methods take a [`sqlx::SqlitePool`] borrow (handed out by
//! [`crate::Store::pool`]) rather than owning a connection, so a single pool is
//! shared across every repo call.

pub mod activity;
pub mod agent;
pub mod agent_invocation_target;
pub mod agent_runtime;
pub mod atc_instance;
pub mod attention;
pub mod autopilot;
pub mod autopilot_access;
pub mod autopilot_rule_version;
pub mod autopilot_run;
pub mod autopilot_webhook;
pub mod beads_mapping;
pub mod board;
pub mod card_dependency;
pub mod card_parity;
pub mod comment;
pub mod daemon_config;
pub mod dispatch_attempt;
pub mod event_log;
pub mod fleet;
pub mod fleet_provider_event;
pub mod fleet_work;
pub mod inbox;
pub mod invitation;
pub mod issue;
pub mod issue_metadata;
pub mod issue_property;
pub mod issue_reaction;
pub mod issue_subscriber;
pub mod label;
pub mod member;
pub mod notify_rule;
pub mod profile;
pub mod run_history;
pub mod search;
pub mod skill;
pub mod squad;
pub mod standup;
pub mod task;
pub mod token;
pub mod usage;
pub mod workspace;
