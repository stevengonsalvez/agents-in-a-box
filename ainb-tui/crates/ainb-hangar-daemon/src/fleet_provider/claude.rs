//! Claude structured-control adapter.
//!
//! Managed sessions answer through the blocking hook broker using exact request
//! fingerprints. Degraded sessions may route verified picker key positions.
//! Structured option labels are never submitted as generic terminal text.

use serde::{Deserialize, Serialize};

use super::{
    ApprovalDecision, ProviderError, ProviderReceipt, QuestionAnswer, RequestFingerprint,
    SessionIdentity, StructuredQuestion,
};

/// Exact Claude hook request identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeRequestIdentity {
    /// Stable Fleet identity and optimistic version.
    pub session: SessionIdentity,
    /// Claude hook invocation identifier assigned by the broker.
    pub request_id: String,
    /// Fingerprint over request identity plus ordered payload.
    pub fingerprint: RequestFingerprint,
}

/// Complete `AskUserQuestion` request preserved from Claude hook payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeQuestionRequest {
    /// Exact request identity.
    pub identity: ClaudeRequestIdentity,
    /// Questions in provider order.
    pub questions: Vec<StructuredQuestion>,
}

/// Complete Claude permission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeApprovalRequest {
    /// Exact request identity.
    pub identity: ClaudeRequestIdentity,
    /// Tool name supplied by `PermissionRequest`.
    pub tool_name: String,
    /// Original tool input, retained for broker validation and display.
    pub tool_input: serde_json::Value,
}

/// Authoritative blocking-hook transport.
pub trait ClaudeBrokerTransport {
    /// Answer exact `AskUserQuestion` request.
    fn answer_questions(
        &mut self,
        request: &ClaudeQuestionRequest,
        answers: &[QuestionAnswer],
    ) -> Result<(), ProviderError>;

    /// Decide exact `PermissionRequest` request.
    fn decide_approval(
        &mut self,
        request: &ClaudeApprovalRequest,
        decision: ApprovalDecision,
    ) -> Result<(), ProviderError>;
}

/// Verified legacy picker state captured immediately before key routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPickerSnapshot {
    /// Exact tmux target that displayed the picker.
    pub tmux_target: String,
    /// Session identity observed during verification.
    pub session: SessionIdentity,
    /// Request fingerprint observed during verification.
    pub fingerprint: RequestFingerprint,
    /// Provider-native question IDs in display order.
    pub question_ids: Vec<String>,
    /// Option labels per question in display order.
    pub option_labels: Vec<Vec<String>>,
}

/// Position-only picker route. No generic text appears in this contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPickerRoute {
    /// Exact tmux target verified by caller.
    pub tmux_target: String,
    /// Zero-based selected option positions for every question.
    pub selections: Vec<Vec<usize>>,
}

/// Degraded tmux key router.
pub trait ClaudePickerTransport {
    /// Route picker positions only after adapter validation succeeds.
    fn route_picker(&mut self, route: &VerifiedPickerRoute) -> Result<(), ProviderError>;
}

/// Claude provider control adapter.
pub struct ClaudeAdapter<B, P> {
    broker: B,
    picker: P,
}

impl<B, P> ClaudeAdapter<B, P>
where
    B: ClaudeBrokerTransport,
    P: ClaudePickerTransport,
{
    /// Build adapter from authoritative broker and degraded picker transports.
    pub const fn new(broker: B, picker: P) -> Self {
        Self { broker, picker }
    }

    /// Answer a managed request through the broker using exact fingerprint.
    pub fn answer_authoritative(
        &mut self,
        request: &ClaudeQuestionRequest,
        answers: &[QuestionAnswer],
    ) -> Result<ProviderReceipt, ProviderError> {
        validate_answers(request, answers)?;
        validate_identity(&request.identity)?;
        self.broker.answer_questions(request, answers)?;
        Ok(ProviderReceipt {
            authoritative: true,
            transport: "claude-hook-broker",
        })
    }

    /// Answer degraded visible picker through verified option positions only.
    pub fn answer_degraded(
        &mut self,
        request: &ClaudeQuestionRequest,
        answers: &[QuestionAnswer],
        snapshot: &VerifiedPickerSnapshot,
    ) -> Result<ProviderReceipt, ProviderError> {
        validate_answers(request, answers)?;
        validate_picker_snapshot(request, snapshot)?;
        let selections = selected_positions(request, answers)?;
        self.picker.route_picker(&VerifiedPickerRoute {
            tmux_target: snapshot.tmux_target.clone(),
            selections,
        })?;
        Ok(ProviderReceipt {
            authoritative: false,
            transport: "verified-tmux-picker",
        })
    }

    /// Decide managed permission through exact blocking-hook request.
    pub fn decide_approval(
        &mut self,
        request: &ClaudeApprovalRequest,
        decision: ApprovalDecision,
    ) -> Result<ProviderReceipt, ProviderError> {
        validate_identity(&request.identity)?;
        self.broker.decide_approval(request, decision)?;
        Ok(ProviderReceipt {
            authoritative: true,
            transport: "claude-hook-broker",
        })
    }

    /// Return owned transports for integration or tests.
    pub fn into_inner(self) -> (B, P) {
        (self.broker, self.picker)
    }
}

fn validate_identity(identity: &ClaudeRequestIdentity) -> Result<(), ProviderError> {
    if identity.session.session_key.is_empty()
        || identity.request_id.is_empty()
        || identity.fingerprint.0.is_empty()
    {
        return Err(ProviderError::Protocol(
            "Claude request identity must include session key, request id, and fingerprint".into(),
        ));
    }
    Ok(())
}

fn validate_answers(
    request: &ClaudeQuestionRequest,
    answers: &[QuestionAnswer],
) -> Result<(), ProviderError> {
    if request.questions.is_empty() {
        return Err(ProviderError::Protocol(
            "Claude question request has no questions".into(),
        ));
    }
    if answers.len() != request.questions.len() {
        return Err(ProviderError::Protocol(format!(
            "expected {} Claude answers, got {}",
            request.questions.len(),
            answers.len()
        )));
    }

    for question in &request.questions {
        let matching: Vec<_> =
            answers.iter().filter(|answer| answer.question_id == question.id).collect();
        if matching.len() != 1 {
            return Err(ProviderError::Protocol(format!(
                "question {} must have exactly one answer",
                question.id
            )));
        }
        let values = &matching[0].answers;
        if values.is_empty() || (!question.multi_select && values.len() != 1) {
            return Err(ProviderError::Protocol(format!(
                "question {} has invalid answer count",
                question.id
            )));
        }
    }
    Ok(())
}

fn validate_picker_snapshot(
    request: &ClaudeQuestionRequest,
    snapshot: &VerifiedPickerSnapshot,
) -> Result<(), ProviderError> {
    validate_identity(&request.identity)?;
    if request.identity.session != snapshot.session {
        return Err(ProviderError::Stale(
            "Claude picker session identity or version changed".into(),
        ));
    }
    if request.identity.fingerprint != snapshot.fingerprint {
        return Err(ProviderError::Stale(
            "Claude picker request fingerprint changed".into(),
        ));
    }

    let question_ids: Vec<_> =
        request.questions.iter().map(|question| question.id.clone()).collect();
    let option_labels: Vec<_> = request
        .questions
        .iter()
        .map(|question| {
            question.options.iter().map(|option| option.label.clone()).collect::<Vec<_>>()
        })
        .collect();
    if question_ids != snapshot.question_ids || option_labels != snapshot.option_labels {
        return Err(ProviderError::Stale(
            "Claude picker question or option order changed".into(),
        ));
    }
    Ok(())
}

fn selected_positions(
    request: &ClaudeQuestionRequest,
    answers: &[QuestionAnswer],
) -> Result<Vec<Vec<usize>>, ProviderError> {
    request
        .questions
        .iter()
        .map(|question| {
            let answer =
                answers.iter().find(|answer| answer.question_id == question.id).ok_or_else(
                    || ProviderError::Protocol(format!("missing answer for {}", question.id)),
                )?;
            answer
                .answers
                .iter()
                .map(|value| {
                    question.options.iter().position(|option| option.label == *value).ok_or_else(
                        || {
                            ProviderError::Unsupported(format!(
                                "degraded Claude picker cannot route free-form answer for {}",
                                question.id
                            ))
                        },
                    )
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_provider::QuestionOption;

    #[derive(Default)]
    struct FakeBroker {
        answered: Vec<(ClaudeQuestionRequest, Vec<QuestionAnswer>)>,
        approvals: Vec<(ClaudeApprovalRequest, ApprovalDecision)>,
    }

    impl ClaudeBrokerTransport for FakeBroker {
        fn answer_questions(
            &mut self,
            request: &ClaudeQuestionRequest,
            answers: &[QuestionAnswer],
        ) -> Result<(), ProviderError> {
            self.answered.push((request.clone(), answers.to_vec()));
            Ok(())
        }

        fn decide_approval(
            &mut self,
            request: &ClaudeApprovalRequest,
            decision: ApprovalDecision,
        ) -> Result<(), ProviderError> {
            self.approvals.push((request.clone(), decision));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakePicker {
        routes: Vec<VerifiedPickerRoute>,
    }

    impl ClaudePickerTransport for FakePicker {
        fn route_picker(&mut self, route: &VerifiedPickerRoute) -> Result<(), ProviderError> {
            self.routes.push(route.clone());
            Ok(())
        }
    }

    fn request() -> ClaudeQuestionRequest {
        ClaudeQuestionRequest {
            identity: ClaudeRequestIdentity {
                session: SessionIdentity {
                    session_key: "claude:s-1".into(),
                    provider_session_id: Some("s-1".into()),
                    expected_version: 7,
                },
                request_id: "req-1".into(),
                fingerprint: RequestFingerprint("fp-1".into()),
            },
            questions: vec![
                StructuredQuestion {
                    id: "editor".into(),
                    header: "Editor".into(),
                    question: "Pick editor".into(),
                    options: vec![
                        QuestionOption {
                            label: "Vim".into(),
                            description: "Modal".into(),
                        },
                        QuestionOption {
                            label: "Emacs".into(),
                            description: "Extensible".into(),
                        },
                    ],
                    multi_select: false,
                    is_other: false,
                    is_secret: false,
                },
                StructuredQuestion {
                    id: "tools".into(),
                    header: "Tools".into(),
                    question: "Pick tools".into(),
                    options: vec![
                        QuestionOption {
                            label: "rg".into(),
                            description: "Text".into(),
                        },
                        QuestionOption {
                            label: "ast-grep".into(),
                            description: "Syntax".into(),
                        },
                    ],
                    multi_select: true,
                    is_other: false,
                    is_secret: false,
                },
            ],
        }
    }

    fn answers() -> Vec<QuestionAnswer> {
        vec![
            QuestionAnswer {
                question_id: "editor".into(),
                answers: vec!["Vim".into()],
            },
            QuestionAnswer {
                question_id: "tools".into(),
                answers: vec!["rg".into(), "ast-grep".into()],
            },
        ]
    }

    fn snapshot(request: &ClaudeQuestionRequest) -> VerifiedPickerSnapshot {
        VerifiedPickerSnapshot {
            tmux_target: "claude-session:0.0".into(),
            session: request.identity.session.clone(),
            fingerprint: request.identity.fingerprint.clone(),
            question_ids: request.questions.iter().map(|q| q.id.clone()).collect(),
            option_labels: request
                .questions
                .iter()
                .map(|q| q.options.iter().map(|o| o.label.clone()).collect())
                .collect(),
        }
    }

    #[test]
    fn authoritative_answer_preserves_all_questions_and_multiselect_values() {
        let request = request();
        let answers = answers();
        let mut adapter = ClaudeAdapter::new(FakeBroker::default(), FakePicker::default());

        let receipt = adapter.answer_authoritative(&request, &answers).expect("broker answer");
        let (broker, picker) = adapter.into_inner();

        assert!(receipt.authoritative);
        assert_eq!(broker.answered, vec![(request, answers)]);
        assert!(picker.routes.is_empty());
    }

    #[test]
    fn degraded_answer_routes_only_verified_option_positions() {
        let request = request();
        let answers = answers();
        let snapshot = snapshot(&request);
        let mut adapter = ClaudeAdapter::new(FakeBroker::default(), FakePicker::default());

        let receipt = adapter.answer_degraded(&request, &answers, &snapshot).expect("picker route");
        let (broker, picker) = adapter.into_inner();

        assert!(!receipt.authoritative);
        assert!(broker.answered.is_empty());
        assert_eq!(
            picker.routes,
            vec![VerifiedPickerRoute {
                tmux_target: "claude-session:0.0".into(),
                selections: vec![vec![0], vec![0, 1]],
            }]
        );
    }

    #[test]
    fn degraded_answer_refuses_stale_version_before_tmux_route() {
        let request = request();
        let mut snapshot = snapshot(&request);
        snapshot.session.expected_version += 1;
        let mut adapter = ClaudeAdapter::new(FakeBroker::default(), FakePicker::default());

        let error = adapter
            .answer_degraded(&request, &answers(), &snapshot)
            .expect_err("stale picker must fail");
        let (_, picker) = adapter.into_inner();

        assert!(matches!(error, ProviderError::Stale(_)));
        assert!(picker.routes.is_empty());
    }

    #[test]
    fn degraded_answer_never_routes_free_form_value_as_text() {
        let mut request = request();
        request.questions[0].options.clear();
        let snapshot = snapshot(&request);
        let mut answer = answers();
        answer[0].answers = vec!["custom editor".into()];
        let mut adapter = ClaudeAdapter::new(FakeBroker::default(), FakePicker::default());

        let error = adapter
            .answer_degraded(&request, &answer, &snapshot)
            .expect_err("free-form degraded answer must fail");
        let (_, picker) = adapter.into_inner();

        assert!(matches!(error, ProviderError::Unsupported(_)));
        assert!(picker.routes.is_empty());
    }
}
