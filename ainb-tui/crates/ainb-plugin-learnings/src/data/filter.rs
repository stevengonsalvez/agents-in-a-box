//! Browse-tab filter predicates.
//!
//! A [`Filter`] is an AND-combination of field equality predicates
//! (scope / category / source_tool / project) plus an optional minimum
//! confidence. Building it is fluent ([`Filter::with`], [`Filter::with_min_confidence`])
//! and applying it ([`Filter::apply`]) is a pure borrow-and-retain over a slice
//! of [`LearningRecord`]s — the Browse tab derives chip facets from the records,
//! sets the active predicates, and re-applies on every keystroke.

use super::record::LearningRecord;

/// A facet a [`Filter`] can constrain by exact (case-sensitive) string match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterField {
    /// `scope` facet (`universal` / `project` / ...).
    Scope,
    /// `category` facet.
    Category,
    /// `provenance.source_tool` facet.
    Source,
    /// `provenance.project` facet.
    Project,
}

/// An AND-combination of field predicates + an optional confidence floor.
///
/// Empty (the [`Default`]) keeps every record. Each `with` adds one equality
/// predicate; repeating a field overwrites it (a facet chip is single-select).
#[derive(Debug, Clone, Default)]
pub struct Filter {
    scope: Option<String>,
    category: Option<String>,
    source: Option<String>,
    project: Option<String>,
    min_confidence: Option<f64>,
}

impl Filter {
    /// An empty filter that keeps every record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Constrain `field` to equal `value` (consuming-builder style).
    #[must_use]
    pub fn with(mut self, field: FilterField, value: impl Into<String>) -> Self {
        let v = value.into();
        match field {
            FilterField::Scope => self.scope = Some(v),
            FilterField::Category => self.category = Some(v),
            FilterField::Source => self.source = Some(v),
            FilterField::Project => self.project = Some(v),
        }
        self
    }

    /// Require `confidence >= floor`.
    #[must_use]
    pub fn with_min_confidence(mut self, floor: f64) -> Self {
        self.min_confidence = Some(floor);
        self
    }

    /// `true` when no predicate is set (keeps all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scope.is_none()
            && self.category.is_none()
            && self.source.is_none()
            && self.project.is_none()
            && self.min_confidence.is_none()
    }

    /// Does a single record satisfy every active predicate?
    #[must_use]
    pub fn matches(&self, rec: &LearningRecord) -> bool {
        if let Some(scope) = &self.scope {
            if &rec.scope != scope {
                return false;
            }
        }
        if let Some(category) = &self.category {
            if &rec.category != category {
                return false;
            }
        }
        if let Some(source) = &self.source {
            if rec.source_tool.as_deref() != Some(source.as_str()) {
                return false;
            }
        }
        if let Some(project) = &self.project {
            if rec.project.as_deref() != Some(project.as_str()) {
                return false;
            }
        }
        if let Some(floor) = self.min_confidence {
            if rec.confidence < floor {
                return false;
            }
        }
        true
    }

    /// Return the subset of `records` that satisfy the filter, preserving the
    /// input order.
    #[must_use]
    pub fn apply<'a>(&self, records: &'a [LearningRecord]) -> Vec<&'a LearningRecord> {
        records.iter().filter(|r| self.matches(r)).collect()
    }
}
